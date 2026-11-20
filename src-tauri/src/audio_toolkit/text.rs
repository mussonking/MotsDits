use crate::settings::CustomWordEntry;
use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use strsim::levenshtein;

/// Builds an n-gram string by cleaning and concatenating words (lowercased).
///
/// Curly Unicode apostrophes (U+2019, U+02BC) are normalized to a straight
/// ASCII apostrophe so transcriptions from Whisper (which often emits the
/// curly form for French contractions like "l'app") still match aliases the
/// user typed with a regular keyboard.
fn build_ngram(words: &[&str]) -> String {
    build_ngram_cased(words, true)
}

/// Like [`build_ngram`] but optionally preserves the original letter casing.
///
/// When `lowercase` is false the fragment keeps its case, which lets us match
/// aliases and blacklist tokens the user typed with specific capitals (e.g.
/// "xI" vs "Xi") exactly, instead of collapsing them together.
fn build_ngram_cased(words: &[&str], lowercase: bool) -> String {
    words
        .iter()
        .map(|w| {
            let fragment = w
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .replace(['\u{2019}', '\u{02BC}'], "'");
            if lowercase {
                fragment.to_lowercase()
            } else {
                fragment
            }
        })
        .collect::<Vec<_>>()
        .concat()
}

/// Whether a user-typed token carries explicit casing (has any uppercase).
///
/// Such tokens are matched exactly; all-lowercase tokens stay case-insensitive.
fn has_explicit_case(token: &str) -> bool {
    token.chars().any(|c| c.is_uppercase())
}

/// Finds the best matching custom word for a candidate string
///
/// Uses Levenshtein distance and Soundex phonetic matching to find
/// the best match above the given threshold.
fn find_best_match<'a>(
    candidate: &str,
    candidate_cs: &str,
    candidate_first_upper: bool,
    word_strings: &'a [String],
    custom_words_nospace: &[String],
    custom_words_case_sensitive: &[bool],
    threshold: f64,
    blacklist_ci: &HashSet<String>,
    blacklist_cs: &HashSet<String>,
) -> Option<(&'a String, f64)> {
    // Skip if the candidate is blacklisted. Lowercase-only tokens block any
    // casing; tokens the user typed with a capital block only that exact casing.
    if blacklist_ci.contains(candidate) || blacklist_cs.contains(candidate_cs) {
        return None;
    }
    if candidate.is_empty() || candidate.len() > 50 {
        return None;
    }

    let mut best_match: Option<&String> = None;
    let mut best_score = f64::MAX;

    for (i, custom_word_nospace) in custom_words_nospace.iter().enumerate() {
        // Case-sensitive entries (any uppercase in user-typed word) require the
        // transcribed candidate to start with uppercase — typically because
        // Whisper identified it as a proper noun. This prevents lowercase
        // common-word transcriptions from being rewritten into proper nouns.
        if custom_words_case_sensitive[i] && !candidate_first_upper {
            continue;
        }

        // Skip if lengths are too different (optimization + prevents over-matching)
        // Use percentage-based check: max 25% length difference (prevents n-grams from
        // matching significantly shorter custom words, e.g., "openaigpt" vs "openai")
        let len_diff = (candidate.len() as i32 - custom_word_nospace.len() as i32).abs() as f64;
        let max_len = candidate.len().max(custom_word_nospace.len()) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0); // At least 2 chars difference allowed
        if len_diff > max_allowed_diff {
            continue;
        }

        // Calculate Levenshtein distance (normalized by length)
        let levenshtein_dist = levenshtein(candidate, custom_word_nospace);
        let max_len = candidate.len().max(custom_word_nospace.len()) as f64;
        let levenshtein_score = if max_len > 0.0 {
            levenshtein_dist as f64 / max_len
        } else {
            1.0
        };

        // Calculate phonetic similarity using Soundex
        let phonetic_match = soundex(candidate, custom_word_nospace);

        // Combine scores: favor phonetic matches, but also consider string similarity
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3 // Give significant boost to phonetic matches
        } else {
            levenshtein_score
        };

        // Accept if the score is good enough (configurable threshold)
        if combined_score < threshold && combined_score < best_score {
            best_match = Some(&word_strings[i]);
            best_score = combined_score;
        }
    }

    best_match.map(|m| (m, best_score))
}

/// Applies custom word corrections to transcribed text.
///
/// Two-phase correction:
/// 1. **Hard aliases** — exact replacement; lowercase aliases match any casing,
///    aliases with a capital match that exact casing (e.g. "jiminy" → "Gemini")
/// 2. **Fuzzy matching** — Levenshtein + Soundex with blacklist protection
///
/// # Arguments
/// * `text` - The input text to correct
/// * `custom_words` - List of custom word entries (with optional aliases and blacklist)
/// * `threshold` - Maximum similarity score for fuzzy matching
pub fn apply_custom_words(text: &str, custom_words: &[CustomWordEntry], threshold: f64) -> String {
    if custom_words.is_empty() {
        return text.to_string();
    }

    // Phase 0: hard alias replacement (casing handled per-alias inside).
    let phase0_text = apply_custom_word_aliases(text, custom_words);

    // === Phase 1: Fuzzy matching with blacklist ===
    // Extract word strings for fuzzy comparison
    let word_strings: Vec<String> = custom_words.iter().map(|e| e.word.clone()).collect();
    let custom_words_lower: Vec<String> = word_strings.iter().map(|w| w.to_lowercase()).collect();
    let custom_words_nospace: Vec<String> = custom_words_lower
        .iter()
        .map(|w| w.replace(' ', ""))
        .collect();
    // An entry is case-sensitive when the user-typed word contains any uppercase.
    let custom_words_case_sensitive: Vec<bool> = word_strings
        .iter()
        .map(|w| w.chars().any(|c| c.is_uppercase()))
        .collect();

    // Build global blacklist from all entries, split by casing: lowercase-only
    // tokens stay case-insensitive; tokens with a capital match that exact casing.
    let mut blacklist_ci: HashSet<String> = HashSet::new();
    let mut blacklist_cs: HashSet<String> = HashSet::new();
    for token in custom_words.iter().flat_map(|e| e.blacklist.iter()) {
        if has_explicit_case(token) {
            blacklist_cs.insert(token.clone());
        } else {
            blacklist_ci.insert(token.to_lowercase());
        }
    }

    let phase0_words: Vec<&str> = phase0_text.split_whitespace().collect();
    let mut result = Vec::new();
    let mut j = 0;

    while j < phase0_words.len() {
        let mut matched = false;

        // Try n-grams from longest (3) to shortest (1) - greedy matching
        for n in (1..=3).rev() {
            if j + n > phase0_words.len() {
                continue;
            }

            let ngram_words = &phase0_words[j..j + n];
            let ngram = build_ngram(ngram_words);
            let ngram_cs = build_ngram_cased(ngram_words, false);
            // First-letter case of the original n-gram (skipping leading punctuation).
            let candidate_first_upper = ngram_words[0]
                .chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(|c| c.is_uppercase());

            if let Some((replacement, _score)) = find_best_match(
                &ngram,
                &ngram_cs,
                candidate_first_upper,
                &word_strings,
                &custom_words_nospace,
                &custom_words_case_sensitive,
                threshold,
                &blacklist_ci,
                &blacklist_cs,
            ) {
                let (prefix, _) = extract_punctuation(ngram_words[0]);
                let (_, suffix) = extract_punctuation(ngram_words[n - 1]);
                // Emit the user-typed word verbatim — never mirror the
                // transcription's casing onto it.
                result.push(format!("{}{}{}", prefix, replacement, suffix));
                j += n;
                matched = true;
                break;
            }
        }

        if !matched {
            result.push(phase0_words[j].to_string());
            j += 1;
        }
    }

    result.join(" ")
}

/// Applies only hard aliases from custom words.
///
/// Whisper models use custom words as an initial prompt, but aliases are user-defined
/// exact replacements and still need a deterministic post-pass.
pub fn apply_custom_word_aliases(text: &str, custom_words: &[CustomWordEntry]) -> String {
    if custom_words.is_empty() {
        return text.to_string();
    }

    // Aliases the user typed with a capital match that exact casing; lowercase-only
    // aliases stay case-insensitive (they catch every casing the model emits).
    let mut alias_map_ci: HashMap<String, &str> = HashMap::new();
    let mut alias_map_cs: HashMap<String, &str> = HashMap::new();
    for entry in custom_words {
        for alias in &entry.aliases {
            let words: Vec<&str> = alias.split_whitespace().collect();
            if has_explicit_case(alias) {
                let key = build_ngram_cased(&words, false);
                if !key.is_empty() {
                    alias_map_cs.insert(key, &entry.word);
                }
            } else {
                let key = build_ngram_cased(&words, true);
                if !key.is_empty() {
                    alias_map_ci.insert(key, &entry.word);
                }
            }
        }
    }

    if alias_map_ci.is_empty() && alias_map_cs.is_empty() {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let mut alias_matched = false;

        // Try n-grams from longest (3) to shortest (1)
        for n in (1..=3).rev() {
            if i + n > words.len() {
                continue;
            }
            let ngram_words = &words[i..i + n];
            let ngram_cs = build_ngram_cased(ngram_words, false);
            let ngram_ci = build_ngram(ngram_words);
            let target = alias_map_cs
                .get(&ngram_cs)
                .or_else(|| alias_map_ci.get(&ngram_ci));

            if let Some(target) = target {
                let (prefix, _) = extract_punctuation(ngram_words[0]);
                let (_, suffix) = extract_punctuation(ngram_words[n - 1]);
                // Emit the alias target verbatim, exactly as the user typed it.
                result.push(format!("{}{}{}", prefix, target, suffix));
                i += n;
                alias_matched = true;
                break;
            }
        }

        if !alias_matched {
            result.push(words[i].to_string());
            i += 1;
        }
    }

    result.join(" ")
}

/// Extracts punctuation prefix and suffix from a word.
///
/// Works on byte offsets of whole characters: French transcriptions carry
/// multi-byte punctuation (guillemets, ellipsis, curly quotes), and slicing by
/// character count would cut one in half and panic.
fn extract_punctuation(word: &str) -> (&str, &str) {
    let Some(first) = word.find(|c: char| c.is_alphanumeric()) else {
        // Nothing but punctuation: it is both prefix and suffix, so a
        // free-standing mark ("Gemini !") survives on either side.
        return (word, word);
    };
    let last = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(word.len());

    (&word[..first], &word[last..])
}

/// Returns filler words appropriate for the given language code.
///
/// Some words like "um" and "ha" are real words in certain languages
/// (e.g., Portuguese "um" = "a/an", Spanish "ha" = "has"), so we only
/// include them as fillers for languages where they are truly fillers.
fn get_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &[
            "uh", "um", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "eh",
            "ehh", "ha",
        ],
        "es" => &["ehm", "mmm", "hmm", "hm"],
        "pt" => &["ahm", "hmm", "mmm", "hm"],
        "fr" => &["euh", "hmm", "hm", "mmm"],
        "de" => &["äh", "ähm", "hmm", "hm", "mmm"],
        "it" => &["ehm", "hmm", "mmm", "hm"],
        "cs" => &["ehm", "hmm", "mmm", "hm"],
        "pl" => &["hmm", "mmm", "hm"],
        "tr" => &["hmm", "mmm", "hm"],
        "ru" => &["хм", "ммм", "hmm", "mmm"],
        "uk" => &["хм", "ммм", "hmm", "mmm"],
        "ar" => &["hmm", "mmm"],
        "ja" => &["hmm", "mmm"],
        "ko" => &["hmm", "mmm"],
        "vi" => &["hmm", "mmm", "hm"],
        "zh" => &["hmm", "mmm"],
        // Conservative universal fallback (no "um", "eh", "ha")
        _ => &[
            "uh", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "ehh",
        ],
    }
}

static MULTI_SPACE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s{2,}").unwrap());

/// Collapses repeated 1-2 letter words (3+ repetitions) to a single instance.
/// E.g., "wh wh wh wh" -> "wh", "I I I I" -> "I"
fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let word = words[i];
        let word_lower = word.to_lowercase();

        // Only process 1-2 letter words
        if word_lower.len() <= 2 && word_lower.chars().all(|c| c.is_alphabetic()) {
            // Count consecutive repetitions (case-insensitive)
            let mut count = 1;
            while i + count < words.len() && words[i + count].to_lowercase() == word_lower {
                count += 1;
            }

            // If 3+ repetitions, collapse to single instance
            if count >= 3 {
                result.push(word);
                i += count;
            } else {
                result.push(word);
                i += 1;
            }
        } else {
            result.push(word);
            i += 1;
        }
    }

    result.join(" ")
}

/// Filters transcription output by removing filler words and stutter artifacts.
///
/// This function cleans up raw transcription text by:
/// 1. Removing filler words based on the app language (or custom list)
/// 2. Collapsing repeated 1-2 letter stutters (e.g., "wh wh wh" -> "wh")
/// 3. Cleaning up excess whitespace
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `lang` - The app language code (e.g., "en", "pt-BR") used to select filler words
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
///
/// # Returns
/// The filtered text with filler words and stutters removed
pub fn filter_transcription_output(
    text: &str,
    lang: &str,
    custom_filler_words: &Option<Vec<String>>,
) -> String {
    let mut filtered = text.to_string();

    // Build filler patterns from custom list or language defaults
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => get_filler_words_for_language(lang)
            .iter()
            .map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap())
            .collect(),
    };

    // Remove filler words
    for pattern in &patterns {
        filtered = pattern.replace_all(&filtered, "").to_string();
    }

    // Collapse repeated 1-2 letter words (stutter artifacts like "wh wh wh wh")
    filtered = collapse_stutters(&filtered);

    // Clean up multiple spaces to single space
    filtered = MULTI_SPACE_PATTERN.replace_all(&filtered, " ").to_string();

    // Trim leading/trailing whitespace
    filtered.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_punctuation_handles_multibyte_punctuation() {
        assert_eq!(
            extract_punctuation("\u{ab}Gemini\u{bb}"),
            ("\u{ab}", "\u{bb}")
        );
        assert_eq!(extract_punctuation("Gemini\u{2026}"), ("", "\u{2026}"));
        assert_eq!(extract_punctuation("(Gemini),"), ("(", "),"));
        assert_eq!(extract_punctuation("Gemini"), ("", ""));
        assert_eq!(extract_punctuation("\u{2026}"), ("\u{2026}", "\u{2026}"));
    }

    /// Helper: create simple CustomWordEntry with no aliases/blacklist
    fn word(s: &str) -> CustomWordEntry {
        CustomWordEntry {
            word: s.to_string(),
            aliases: Vec::new(),
            blacklist: Vec::new(),
        }
    }

    /// Helper: create CustomWordEntry with aliases
    fn word_with_aliases(s: &str, aliases: &[&str]) -> CustomWordEntry {
        CustomWordEntry {
            word: s.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            blacklist: Vec::new(),
        }
    }

    /// Helper: create CustomWordEntry with blacklist
    fn word_with_blacklist(s: &str, blacklist: &[&str]) -> CustomWordEntry {
        CustomWordEntry {
            word: s.to_string(),
            aliases: Vec::new(),
            blacklist: blacklist.iter().map(|b| b.to_string()).collect(),
        }
    }

    #[test]
    fn test_apply_custom_words_exact_match() {
        let text = "Hello World";
        let custom_words = vec![word("Hello"), word("World")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_apply_custom_words_case_sensitive_skips_lowercase() {
        // Entry "Hello" has uppercase → case-sensitive; lowercase "hello" must not match.
        let text = "hello world";
        let custom_words = vec![word("Hello"), word("World")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_apply_custom_words_lowercase_entry_matches_anywhere() {
        // Lowercase-only entry matches any casing, and now outputs verbatim
        // (the registered lowercase form), not the transcription's casing.
        let text = "Hello World";
        let custom_words = vec![word("hello"), word("world")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_apply_custom_words_fuzzy_match() {
        let text = "helo wrold";
        let custom_words = vec![word("hello"), word("world")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_replacement_is_verbatim() {
        // The registered word is emitted exactly as typed — the transcription's
        // casing is never mirrored onto it.
        let custom_words = vec![word("xhigh")];
        assert_eq!(
            apply_custom_words("XHIGH now", &custom_words, 0.5),
            "xhigh now"
        );
        assert_eq!(
            apply_custom_words("Xhigh now", &custom_words, 0.5),
            "xhigh now"
        );
    }

    #[test]
    fn test_extract_punctuation() {
        assert_eq!(extract_punctuation("hello"), ("", ""));
        assert_eq!(extract_punctuation("!hello?"), ("!", "?"));
        assert_eq!(extract_punctuation("...hello..."), ("...", "..."));
    }

    #[test]
    fn test_empty_custom_words() {
        let text = "hello world";
        let custom_words: Vec<CustomWordEntry> = vec![];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_filter_filler_words() {
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was thinking about this");
    }

    #[test]
    fn test_filter_filler_words_case_insensitive() {
        let text = "UHM this is UH a test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "this is a test");
    }

    #[test]
    fn test_filter_filler_words_with_punctuation() {
        let text = "Well, uhm, I think, uh. that's right";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Well, I think, that's right");
    }

    #[test]
    fn test_filter_cleans_whitespace() {
        let text = "Hello    world   test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_filter_trims() {
        let text = "  Hello world  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_filter_combined() {
        let text = "  Uhm, so I was, uh, thinking about this  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "so I was, thinking about this");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "w wh why");
    }

    #[test]
    fn test_filter_stutter_short_words() {
        let text = "I I I I think so so so so";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think so");
    }

    #[test]
    fn test_filter_stutter_mixed_case() {
        let text = "No NO no NO no";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "No");
    }

    #[test]
    fn test_filter_stutter_preserves_two_repetitions() {
        let text = "no no is fine";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_filter_english_removes_um() {
        let text = "um I think um this is good";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think this is good");
    }

    #[test]
    fn test_filter_portuguese_preserves_um() {
        // "um" means "a/an" in Portuguese
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_spanish_preserves_ha() {
        // "ha" means "has" in Spanish
        let text = "ha sido un buen día";
        let result = filter_transcription_output(text, "es", &None);
        assert_eq!(result, "ha sido un buen día");
    }

    #[test]
    fn test_filter_language_code_with_region() {
        // "pt-BR" should normalize to "pt"
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt-BR", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_custom_filler_words_override() {
        let custom = Some(vec!["okay".to_string(), "right".to_string()]);
        let text = "okay so I think right this works";
        let result = filter_transcription_output(text, "en", &custom);
        assert_eq!(result, "so I think this works");
    }

    #[test]
    fn test_filter_custom_filler_words_empty_disables() {
        let custom = Some(vec![]);
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &custom);
        // No filler words removed since custom list is empty
        assert_eq!(result, "So uhm I was thinking uh about this");
    }

    #[test]
    fn test_filter_unknown_language_uses_fallback() {
        let text = "uh I think uhm this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "I think this works");
    }

    #[test]
    fn test_filter_fallback_does_not_remove_um() {
        // Fallback (unknown language) should not remove "um" since it's a real word in some languages
        let text = "um I think this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "um I think this works");
    }

    #[test]
    fn test_apply_custom_words_ngram_two_words() {
        let text = "il cui nome è Charge B, che permette";
        let custom_words = vec![word("ChargeBee")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee"));
        assert!(!result.contains("Charge B"));
    }

    #[test]
    fn test_apply_custom_words_ngram_three_words() {
        let text = "use Chat G P T for this";
        let custom_words = vec![word("ChatGPT")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChatGPT"));
    }

    #[test]
    fn test_apply_custom_words_prefers_longer_ngram() {
        let text = "Open AI GPT model";
        let custom_words = vec![word("OpenAI"), word("GPT")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "OpenAI GPT model");
    }

    #[test]
    fn test_apply_custom_words_ngram_preserves_case() {
        let text = "CHARGE B is great";
        let custom_words = vec![word("ChargeBee")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee"));
    }

    #[test]
    fn test_apply_custom_words_ngram_with_spaces_in_custom() {
        let text = "using Mac Book Pro";
        let custom_words = vec![word("MacBook Pro")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("MacBook"));
    }

    #[test]
    fn test_apply_custom_words_trailing_number_not_doubled() {
        let text = "use GPT4 for this";
        let custom_words = vec![word("GPT-4")];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(
            !result.contains("GPT-44"),
            "got double-counted result: {}",
            result
        );
    }

    // === Hard alias tests ===

    #[test]
    fn test_hard_alias_exact_replacement() {
        let text = "Ask Jiminy about this";
        let custom_words = vec![word_with_aliases("Gemini", &["Jiminy"])];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "Ask Gemini about this");
    }

    #[test]
    fn test_hard_alias_lowercase_is_case_insensitive() {
        // A lowercase-only alias still catches every casing the model emits.
        let text = "ask JIMINY about this";
        let custom_words = vec![word_with_aliases("Gemini", &["jiminy"])];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "ask Gemini about this");
    }

    #[test]
    fn test_hard_alias_uppercase_is_case_sensitive() {
        // Aliases typed with a capital match only that exact casing, so distinct
        // casings can map to the same target while others stay untouched.
        let custom_words = vec![word_with_aliases("xhigh", &["xI"])];
        // Exact casing is rewritten.
        assert_eq!(
            apply_custom_words("use xI here", &custom_words, 0.18),
            "use xhigh here"
        );
        // Other casings are left untouched.
        assert_eq!(
            apply_custom_words("use xi here", &custom_words, 0.18),
            "use xi here"
        );
        assert_eq!(
            apply_custom_words("use XI here", &custom_words, 0.18),
            "use XI here"
        );
    }

    #[test]
    fn test_hard_alias_distinct_casings_to_same_target() {
        // Both "xI" and "Xi" map to xhigh; "xi"/"XI" are left alone.
        let custom_words = vec![word_with_aliases("xhigh", &["xI", "Xi"])];
        assert_eq!(
            apply_custom_words("use xI here", &custom_words, 0.18),
            "use xhigh here"
        );
        // "Xi" maps to the same target, emitted verbatim as typed -> "xhigh".
        assert_eq!(
            apply_custom_words("use Xi here", &custom_words, 0.18),
            "use xhigh here"
        );
        assert_eq!(
            apply_custom_words("use xi here", &custom_words, 0.18),
            "use xi here"
        );
    }

    #[test]
    fn test_blacklist_case_sensitive_token() {
        // Blacklist "Fotter" (has a capital) blocks only that exact casing; the
        // lowercase "fotter" transcription can still fuzzy-match the entry.
        let custom_words = vec![word_with_blacklist("FOOTER", &["Fotter"])];
        // Exact-cased candidate is blocked -> stays as typed.
        assert_eq!(
            apply_custom_words("the Fotter here", &custom_words, 0.5),
            "the Fotter here"
        );
    }

    #[test]
    fn test_hard_alias_multiple() {
        let text = "Jimmy said hello";
        let custom_words = vec![word_with_aliases("Gemini", &["Jiminy", "Jimmy"])];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "Gemini said hello");
    }

    #[test]
    fn test_hard_alias_preserves_punctuation() {
        let text = "Ask Jiminy, please.";
        let custom_words = vec![word_with_aliases("Gemini", &["Jiminy"])];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "Ask Gemini, please.");
    }

    #[test]
    fn test_hard_alias_multi_word_replacement() {
        let text = "Ask gem and I about this";
        let custom_words = vec![word_with_aliases("Gemini", &["gem and I"])];
        let result = apply_custom_words(text, &custom_words, 0.18);
        assert_eq!(result, "Ask Gemini about this");
    }

    #[test]
    fn test_alias_only_pass_for_whisper_pipeline() {
        let text = "Ask Jiminy about this";
        let custom_words = vec![word_with_aliases("Gemini", &["Jiminy"])];
        let result = apply_custom_word_aliases(text, &custom_words);
        assert_eq!(result, "Ask Gemini about this");
    }

    // === Blacklist tests ===

    #[test]
    fn test_blacklist_prevents_fuzzy_match() {
        let text = "this feature is great";
        let custom_words = vec![word_with_blacklist("FOOTER", &["feature"])];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "this feature is great");
    }

    #[test]
    fn test_blacklist_case_insensitive() {
        let text = "this Feature is great";
        let custom_words = vec![word_with_blacklist("FOOTER", &["feature"])];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "this Feature is great");
    }

    #[test]
    fn test_blacklist_doesnt_block_other_words() {
        // "feature" is blacklisted for FOOTER, but a similar word should still match.
        // Uppercase-starting candidate is required since the entry "FOOTER" is case-sensitive.
        let text = "check the Fotter please";
        let custom_words = vec![word_with_blacklist("FOOTER", &["feature"])];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "check the FOOTER please");
    }

    #[test]
    fn test_alias_and_blacklist_combined() {
        let custom_words = vec![
            word_with_aliases("Gemini", &["Jiminy"]),
            word_with_blacklist("FOOTER", &["feature"]),
        ];
        let text = "Jiminy said the feature is in the Fotter";
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "Gemini said the feature is in the FOOTER");
    }
}
