use log::debug;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub const APPLE_INTELLIGENCE_PROVIDER_ID: &str = "apple_intelligence";
pub const APPLE_INTELLIGENCE_DEFAULT_MODEL_ID: &str = "Apple Intelligence";

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

// Custom deserializer to handle both old numeric format (1-5) and new string format ("trace", "debug", etc.)
impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LogLevelVisitor;

        impl<'de> Visitor<'de> for LogLevelVisitor {
            type Value = LogLevel;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or integer representing log level")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<LogLevel, E> {
                match value.to_lowercase().as_str() {
                    "trace" => Ok(LogLevel::Trace),
                    "debug" => Ok(LogLevel::Debug),
                    "info" => Ok(LogLevel::Info),
                    "warn" => Ok(LogLevel::Warn),
                    "error" => Ok(LogLevel::Error),
                    _ => Err(E::unknown_variant(
                        value,
                        &["trace", "debug", "info", "warn", "error"],
                    )),
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<LogLevel, E> {
                match value {
                    1 => Ok(LogLevel::Trace),
                    2 => Ok(LogLevel::Debug),
                    3 => Ok(LogLevel::Info),
                    4 => Ok(LogLevel::Warn),
                    5 => Ok(LogLevel::Error),
                    _ => Err(E::invalid_value(de::Unexpected::Unsigned(value), &"1-5")),
                }
            }
        }

        deserializer.deserialize_any(LogLevelVisitor)
    }
}

impl From<LogLevel> for tauri_plugin_log::LogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => tauri_plugin_log::LogLevel::Trace,
            LogLevel::Debug => tauri_plugin_log::LogLevel::Debug,
            LogLevel::Info => tauri_plugin_log::LogLevel::Info,
            LogLevel::Warn => tauri_plugin_log::LogLevel::Warn,
            LogLevel::Error => tauri_plugin_log::LogLevel::Error,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ShortcutBinding {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_binding: String,
    pub current_binding: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LLMPrompt {
    pub id: String,
    pub name: String,
    pub prompt: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PostProcessProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub allow_base_url_edit: bool,
    #[serde(default)]
    pub models_endpoint: Option<String>,
    #[serde(default)]
    pub supports_structured_output: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPosition {
    None,
    Top,
    Bottom,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelUnloadTimeout {
    Never,
    Immediately,
    Min2,
    #[default]
    Min5,
    Min10,
    Min15,
    Hour1,
    Sec5, // Debug mode only
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    CtrlV,
    Direct,
    None,
    ShiftInsert,
    CtrlShiftV,
    ExternalScript,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardHandling {
    #[default]
    DontModify,
    CopyToClipboard,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoSubmitKey {
    #[default]
    Enter,
    CtrlEnter,
    CmdEnter,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRetentionPeriod {
    Never,
    PreserveLimit,
    Days3,
    Weeks2,
    Months3,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardImplementation {
    Tauri,
    HandyKeys,
}

impl Default for KeyboardImplementation {
    fn default() -> Self {
        crate::platform::shortcuts::default_keyboard_implementation()
    }
}

impl Default for PasteMethod {
    fn default() -> Self {
        crate::platform::clipboard::default_paste_method()
    }
}

impl ModelUnloadTimeout {
    pub fn to_minutes(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Min2 => Some(2),
            ModelUnloadTimeout::Min5 => Some(5),
            ModelUnloadTimeout::Min10 => Some(10),
            ModelUnloadTimeout::Min15 => Some(15),
            ModelUnloadTimeout::Hour1 => Some(60),
            ModelUnloadTimeout::Sec5 => Some(0), // Special case for debug - handled separately
        }
    }

    pub fn to_seconds(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Sec5 => Some(5),
            _ => self.to_minutes().map(|m| m * 60),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum SoundTheme {
    Marimba,
    Pop,
    Custom,
}

impl SoundTheme {
    fn as_str(self) -> &'static str {
        match self {
            SoundTheme::Marimba => "marimba",
            SoundTheme::Pop => "pop",
            SoundTheme::Custom => "custom",
        }
    }

    pub fn to_start_path(self) -> String {
        format!("resources/{}_start.wav", self.as_str())
    }

    pub fn to_stop_path(self) -> String {
        format!("resources/{}_stop.wav", self.as_str())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum TypingTool {
    #[default]
    Auto,
    Wtype,
    Kwtype,
    Dotool,
    Ydotool,
    Xdotool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum WhisperAcceleratorSetting {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrtAcceleratorSetting {
    #[default]
    Auto,
    Cpu,
    Cuda,
    #[serde(rename = "directml")]
    DirectMl,
    Rocm,
}

/// A custom word entry with optional hard aliases and blacklist.
/// Backward-compatible: deserializes from both plain strings and full objects.
#[derive(Serialize, Debug, Clone, Type)]
pub struct CustomWordEntry {
    /// The target word (what it should become after correction)
    pub word: String,
    /// Hard aliases: transcription fragments that ALWAYS map to this word.
    /// e.g., aliases=["Jiminy"] for word="Gemini" → "Jiminy" always becomes "Gemini"
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Blacklisted words: words that must NEVER be fuzzy-matched to this custom word.
    /// e.g., blacklist=["feature"] for word="FOOTER" → "feature" stays untouched
    #[serde(default)]
    pub blacklist: Vec<String>,
}

impl<'de> Deserialize<'de> for CustomWordEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CustomWordEntryVisitor;

        impl<'de> Visitor<'de> for CustomWordEntryVisitor {
            type Value = CustomWordEntry;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or object representing a custom word entry")
            }

            // Old format: plain string "word"
            fn visit_str<E: de::Error>(self, value: &str) -> Result<CustomWordEntry, E> {
                Ok(CustomWordEntry {
                    word: value.to_string(),
                    aliases: Vec::new(),
                    blacklist: Vec::new(),
                })
            }

            // New format: { "word": "...", "aliases": [...], "blacklist": [...] }
            fn visit_map<M>(self, map: M) -> Result<CustomWordEntry, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                struct Inner {
                    word: String,
                    #[serde(default)]
                    aliases: Vec<String>,
                    #[serde(default)]
                    blacklist: Vec<String>,
                }
                let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(CustomWordEntry {
                    word: inner.word,
                    aliases: inner.aliases,
                    blacklist: inner.blacklist,
                })
            }
        }

        deserializer.deserialize_any(CustomWordEntryVisitor)
    }
}

/* still handy for composing the initial JSON in the store ------------- */
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct AppSettings {
    pub bindings: HashMap<String, ShortcutBinding>,
    pub push_to_talk: bool,
    pub audio_feedback: bool,
    #[serde(default = "default_audio_feedback_volume")]
    pub audio_feedback_volume: f32,
    #[serde(default = "default_sound_theme")]
    pub sound_theme: SoundTheme,
    #[serde(default = "default_start_hidden")]
    pub start_hidden: bool,
    #[serde(default = "default_autostart_enabled")]
    pub autostart_enabled: bool,
    #[serde(default = "default_update_checks_enabled")]
    pub update_checks_enabled: bool,
    #[serde(default = "default_model")]
    pub selected_model: String,
    #[serde(default = "default_always_on_microphone")]
    pub always_on_microphone: bool,
    #[serde(default)]
    pub selected_microphone: Option<String>,
    #[serde(default)]
    pub clamshell_microphone: Option<String>,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_translate_to_english")]
    pub translate_to_english: bool,
    #[serde(default = "default_selected_language")]
    pub selected_language: String,
    #[serde(default = "default_overlay_position")]
    pub overlay_position: OverlayPosition,
    #[serde(default = "default_debug_mode")]
    pub debug_mode: bool,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default)]
    pub custom_words: Vec<CustomWordEntry>,
    #[serde(default)]
    pub model_unload_timeout: ModelUnloadTimeout,
    #[serde(default = "default_word_correction_threshold")]
    pub word_correction_threshold: f64,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    #[serde(default = "default_recording_retention_period")]
    pub recording_retention_period: RecordingRetentionPeriod,
    #[serde(default)]
    pub paste_method: PasteMethod,
    #[serde(default)]
    pub clipboard_handling: ClipboardHandling,
    #[serde(default = "default_auto_submit")]
    pub auto_submit: bool,
    #[serde(default)]
    pub auto_submit_key: AutoSubmitKey,
    #[serde(default = "default_post_process_enabled")]
    pub post_process_enabled: bool,
    #[serde(default = "default_post_process_auto")]
    pub post_process_auto: bool,
    #[serde(default = "default_post_process_provider_id")]
    pub post_process_provider_id: String,
    #[serde(default = "default_post_process_providers")]
    pub post_process_providers: Vec<PostProcessProvider>,
    #[serde(default = "default_post_process_api_keys")]
    pub post_process_api_keys: HashMap<String, String>,
    #[serde(default = "default_post_process_models")]
    pub post_process_models: HashMap<String, String>,
    #[serde(default = "default_post_process_prompts")]
    pub post_process_prompts: Vec<LLMPrompt>,
    #[serde(default)]
    pub post_process_selected_prompt_id: Option<String>,
    #[serde(default)]
    pub mute_while_recording: bool,
    #[serde(default)]
    pub append_trailing_space: bool,
    #[serde(default = "default_app_language")]
    pub app_language: String,
    #[serde(default)]
    pub experimental_enabled: bool,
    #[serde(default)]
    pub keyboard_implementation: KeyboardImplementation,
    #[serde(default = "default_show_tray_icon")]
    pub show_tray_icon: bool,
    #[serde(default = "default_paste_delay_ms")]
    pub paste_delay_ms: u64,
    #[serde(default = "default_typing_tool")]
    pub typing_tool: TypingTool,
    pub external_script_path: Option<String>,
    #[serde(default)]
    pub custom_filler_words: Option<Vec<String>>,
    #[serde(default)]
    pub whisper_accelerator: WhisperAcceleratorSetting,
    #[serde(default)]
    pub ort_accelerator: OrtAcceleratorSetting,
}

fn default_model() -> String {
    "".to_string()
}

fn default_always_on_microphone() -> bool {
    false
}

fn default_translate_to_english() -> bool {
    false
}

fn default_start_hidden() -> bool {
    false
}

fn default_autostart_enabled() -> bool {
    false
}

fn default_update_checks_enabled() -> bool {
    false
}

fn default_selected_language() -> String {
    "fr".to_string()
}

fn default_overlay_position() -> OverlayPosition {
    OverlayPosition::Bottom
}

fn default_debug_mode() -> bool {
    false
}

fn default_log_level() -> LogLevel {
    LogLevel::Debug
}

fn default_word_correction_threshold() -> f64 {
    0.18
}

fn default_paste_delay_ms() -> u64 {
    60
}

fn default_auto_submit() -> bool {
    false
}

fn default_history_limit() -> usize {
    5
}

fn default_recording_retention_period() -> RecordingRetentionPeriod {
    RecordingRetentionPeriod::PreserveLimit
}

fn default_audio_feedback_volume() -> f32 {
    1.0
}

fn default_sound_theme() -> SoundTheme {
    SoundTheme::Marimba
}

fn default_post_process_enabled() -> bool {
    false
}

fn default_post_process_auto() -> bool {
    false
}

fn default_app_language() -> String {
    tauri_plugin_os::locale()
        .map(|l| l.replace('_', "-"))
        .unwrap_or_else(|| "en".to_string())
}

fn default_show_tray_icon() -> bool {
    true
}

fn default_post_process_provider_id() -> String {
    "openai".to_string()
}

fn default_post_process_providers() -> Vec<PostProcessProvider> {
    let mut providers = vec![
        PostProcessProvider {
            id: "openai".to_string(),
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "anthropic".to_string(),
            label: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "groq".to_string(),
            label: "Groq".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "cerebras".to_string(),
            label: "Cerebras".to_string(),
            base_url: "https://api.cerebras.ai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "gemini".to_string(),
            label: "Google Gemini".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
    ];

    // Note: We always include Apple Intelligence on macOS ARM64 without checking availability
    // at startup. The availability check is deferred to when the user actually tries to use it
    // (in actions.rs). This prevents crashes on macOS 26.x beta where accessing
    // SystemLanguageModel.default during early app initialization causes SIGABRT.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        providers.push(PostProcessProvider {
            id: APPLE_INTELLIGENCE_PROVIDER_ID.to_string(),
            label: "Apple Intelligence".to_string(),
            base_url: "apple-intelligence://local".to_string(),
            allow_base_url_edit: false,
            models_endpoint: None,
            supports_structured_output: true,
        });
    }

    // Custom provider always comes last
    providers.push(PostProcessProvider {
        id: "custom".to_string(),
        label: "Custom".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        allow_base_url_edit: true,
        models_endpoint: Some("/models".to_string()),
        supports_structured_output: false,
    });

    providers
}

fn default_post_process_api_keys() -> HashMap<String, String> {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(provider.id, String::new());
    }
    map
}

fn default_model_for_provider(provider_id: &str) -> String {
    if provider_id == APPLE_INTELLIGENCE_PROVIDER_ID {
        return APPLE_INTELLIGENCE_DEFAULT_MODEL_ID.to_string();
    }
    String::new()
}

fn default_post_process_models() -> HashMap<String, String> {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(
            provider.id.clone(),
            default_model_for_provider(&provider.id),
        );
    }
    map
}

fn default_post_process_prompts() -> Vec<LLMPrompt> {
    vec![
        LLMPrompt {
            id: "prompt_clean".to_string(),
            name: "Transcription propre".to_string(),
            prompt: "Tu nettoies une transcription vocale en français.\n\nTÂCHES\n1. Corrige l'orthographe, la capitalisation et la ponctuation manquante.\n2. Convertis les nombres en chiffres (vingt-cinq → 25, dix pour cent → 10 %, cinq dollars → 5 $).\n3. Remplace la ponctuation dictée par les symboles (point → ., virgule → ,, point d'interrogation → ?).\n4. Retire les mots de remplissage purs et sans valeur sémantique (euh, hum, « ben » répété comme tic, « tsé » comme tic).\n5. Conserve la langue d'origine. Si la source est en français québécois, reste en français québécois.\n\nCONTRAINTES\n- Préserve le sens exact, l'ordre des mots et la structure des phrases.\n- Ne reformule pas, ne paraphrase pas, ne résume pas.\n- Conserve le ton et le registre (familier, québécois, direct, etc.).\n- Si un passage est ambigu, laisse-le tel quel plutôt que de deviner.\n\nSORTIE\nRetourne uniquement le transcript nettoyé. Pas de commentaires, pas de balises, pas d'explications.\n\nTranscript :\n${output}".to_string(),
        },
        LLMPrompt {
            id: "prompt_legal".to_string(),
            name: "Domaine juridique".to_string(),
            prompt: "Tu nettoies une transcription vocale française provenant d'un professionnel du droit (avocat, notaire, parajuriste, juge).\n\nCONTEXTE JURIDIQUE\nLa discussion peut référer à des articles du Code civil du Québec, à de la jurisprudence (« Untel c. Untel »), à des termes latins (mens rea, prima facie, ipso facto, ultra vires), à des tribunaux, à des parties, à des échéances. Préserve toutes ces références sans les corriger ni les substituer.\n\nTÂCHES\n1. Corrige l'orthographe, la ponctuation et la capitalisation.\n2. Mets en forme les références légales (art. 1457 C.c.Q., L.R.Q., R.S.C., L.Q. 2021, c. 25).\n3. Convertis les nombres en chiffres pour les articles, montants, dates et délais.\n4. Conserve la formulation juridique formelle. Ne simplifie pas un terme technique en synonyme grand public.\n5. Retire seulement les hésitations vocales pures (euh, hum).\n\nCONTRAINTES\n- La précision est critique en contexte juridique. Ne paraphrase jamais.\n- Préserve l'intégralité du sens, des nuances et des conditions.\n- Conserve la terminologie technique latine et anglaise.\n\nSORTIE\nRetourne uniquement le transcript nettoyé.\n\nTranscript :\n${output}".to_string(),
        },
        LLMPrompt {
            id: "prompt_accounting".to_string(),
            name: "Domaine comptable".to_string(),
            prompt: "Tu nettoies une transcription vocale française provenant d'un professionnel comptable, fiscaliste ou en finance (CPA, CGA, comptable, contrôleur, conseiller financier).\n\nCONTEXTE COMPTABLE\nLa discussion peut inclure des montants, taux, ratios, dates fiscales, références (T1, T2, T4, TPS, TVQ, REER, CELI, FERR, REEE), normes (IFRS, ASPE, NCECF), comptes du grand livre, états financiers, organismes (ARC, RQ, AMF). Préserve toutes ces références.\n\nTÂCHES\n1. Convertis tous les nombres en chiffres au format québécois : 1 234,56 $, 12,5 %, 0,025 (espaces fines comme séparateur de milliers, virgule comme décimale, symbole après le nombre).\n2. Mets les acronymes en majuscules (TPS, TVQ, REER, CELI, CPA, ARC, RQ, AMF, BAQ).\n3. Corrige l'orthographe et la ponctuation.\n4. Préserve les termes anglais courants si utilisés naturellement (debit, credit, accrual, write-off).\n5. Retire seulement les hésitations vocales pures.\n\nCONTRAINTES\n- Précision numérique absolue. Ne jamais arrondir ni paraphraser un chiffre, un taux ou une date.\n- Préserve l'ordre exact des montants et des opérations.\n- Conserve le ton professionnel.\n\nSORTIE\nRetourne uniquement le transcript nettoyé.\n\nTranscript :\n${output}".to_string(),
        },
        LLMPrompt {
            id: "prompt_medical".to_string(),
            name: "Domaine médical".to_string(),
            prompt: "Tu nettoies une transcription vocale française provenant d'un professionnel de la santé (médecin, infirmier, pharmacien, dentiste).\n\nCONTEXTE MÉDICAL\nLa discussion peut référer à des médicaments (génériques et commerciaux), des posologies, des diagnostics, des examens, des symptômes, de l'anatomie, des abréviations latines et anglaises standard. Préserve toutes ces références sans substituer.\n\nTÂCHES\n1. Corrige l'orthographe, la ponctuation et la capitalisation.\n2. Mets en forme les unités (mg, mL, kg, °C, mmHg, bpm, /min, U/L).\n3. Convertis les nombres en chiffres pour posologies, dates, doses, fréquences, valeurs de laboratoire.\n4. Préserve les abréviations médicales standard telles quelles (q4h, prn, po, IV, IM, sc, tid, bid, qid, hs).\n5. Conserve la terminologie médicale latine, française et anglaise sans la traduire.\n6. Retire seulement les hésitations vocales pures.\n\nCONTRAINTES\n- Précision absolue. Ne paraphrase jamais une posologie, un diagnostic, une dose ou une mesure.\n- Les noms propres de médicaments (génériques et commerciaux) restent intacts.\n- En cas de doute sur un terme, laisse-le tel quel.\n\nSORTIE\nRetourne uniquement le transcript nettoyé.\n\nTranscript :\n${output}".to_string(),
        },
        LLMPrompt {
            id: "prompt_tech".to_string(),
            name: "Domaine technique".to_string(),
            prompt: "Tu nettoies une transcription vocale française provenant d'un développeur, ingénieur logiciel ou professionnel en technologie.\n\nCONTEXTE TECHNIQUE — IMPORTANT\nLe vocabulaire de dev est intentionnel. Les anglicismes techniques (commit, build, toggle, feature, branch, PR, merge, hotkey, overlay, stream, settings, log, deploy, push, pull, refactor), les noms de librairies, frameworks, fonctions et fichiers, les commandes CLI sont CORRECTS tels quels. Ne les traduis pas. Ne les remplace pas par des équivalents français. Ne les corrige pas même s'ils te semblent étranges hors contexte.\n\nTÂCHES\n1. Corrige l'orthographe, la capitalisation et la ponctuation manquante.\n2. Convertis les nombres en chiffres (vingt-cinq → 25, dix pour cent → 10 %).\n3. Remplace la ponctuation dictée par les symboles (point → ., virgule → ,, deux-points → :).\n4. Retire seulement les mots de remplissage purs et sans valeur sémantique (euh, hum, « ben » répété, « tsé » comme tic).\n5. Conserve la langue d'origine.\n\nCONTRAINTES STRICTES\n- Préserve le sens exact, l'ordre des mots et la structure des phrases.\n- Ne reformule pas, ne paraphrase pas, ne réorganise pas, ne résume pas.\n- Conserve le ton (familier, québécois, direct).\n- Ne change AUCUN terme technique, même si tu en doutes.\n- Si un passage est ambigu, laisse-le tel quel.\n\nSORTIE\nRetourne uniquement le transcript nettoyé. Pas de commentaires, pas de balises, pas d'explications, pas de markdown autour.\n\nTranscript :\n${output}".to_string(),
        },
    ]
}

fn default_typing_tool() -> TypingTool {
    TypingTool::Auto
}

fn ensure_post_process_defaults(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    for provider in default_post_process_providers() {
        // Use match to do a single lookup - either sync existing or add new
        match settings
            .post_process_providers
            .iter_mut()
            .find(|p| p.id == provider.id)
        {
            Some(existing) => {
                // Sync supports_structured_output field for existing providers (migration)
                if existing.supports_structured_output != provider.supports_structured_output {
                    debug!(
                        "Updating supports_structured_output for provider '{}' from {} to {}",
                        provider.id,
                        existing.supports_structured_output,
                        provider.supports_structured_output
                    );
                    existing.supports_structured_output = provider.supports_structured_output;
                    changed = true;
                }
            }
            None => {
                // Provider doesn't exist, add it
                settings.post_process_providers.push(provider.clone());
                changed = true;
            }
        }

        if !settings.post_process_api_keys.contains_key(&provider.id) {
            settings
                .post_process_api_keys
                .insert(provider.id.clone(), String::new());
            changed = true;
        }

        let default_model = default_model_for_provider(&provider.id);
        match settings.post_process_models.get_mut(&provider.id) {
            Some(existing) => {
                if existing.is_empty() && !default_model.is_empty() {
                    *existing = default_model.clone();
                    changed = true;
                }
            }
            None => {
                settings
                    .post_process_models
                    .insert(provider.id.clone(), default_model);
                changed = true;
            }
        }
    }

    // Add any default prompts the user is missing (matched by stable ID).
    // Existing user-edited or user-created prompts are never modified or removed.
    let existing_ids: HashSet<String> = settings
        .post_process_prompts
        .iter()
        .map(|p| p.id.clone())
        .collect();
    for default_prompt in default_post_process_prompts() {
        if !existing_ids.contains(&default_prompt.id) {
            settings.post_process_prompts.push(default_prompt);
            changed = true;
        }
    }

    changed
}

pub const SETTINGS_STORE_PATH: &str = "settings_store.json";

pub fn get_default_settings() -> AppSettings {
    let default_shortcut = crate::platform::shortcuts::default_transcribe_shortcut();

    let mut bindings = HashMap::new();
    bindings.insert(
        "transcribe".to_string(),
        ShortcutBinding {
            id: "transcribe".to_string(),
            name: "Transcribe".to_string(),
            description: "Converts your speech into text.".to_string(),
            default_binding: default_shortcut.to_string(),
            current_binding: default_shortcut.to_string(),
        },
    );
    let default_post_process_shortcut = crate::platform::shortcuts::default_post_process_shortcut();

    bindings.insert(
        "transcribe_with_post_process".to_string(),
        ShortcutBinding {
            id: "transcribe_with_post_process".to_string(),
            name: "Transcribe with Post-Processing".to_string(),
            description: "Converts your speech into text and applies AI post-processing."
                .to_string(),
            default_binding: default_post_process_shortcut.to_string(),
            current_binding: default_post_process_shortcut.to_string(),
        },
    );
    bindings.insert(
        "cancel".to_string(),
        ShortcutBinding {
            id: "cancel".to_string(),
            name: "Cancel".to_string(),
            description: "Cancels the current recording.".to_string(),
            default_binding: "escape".to_string(),
            current_binding: "escape".to_string(),
        },
    );
    // Optional shortcut: re-pastes the most recent transcription into the active
    // app (same output path as a fresh transcription). Ships unassigned (empty
    // binding) so it never collides with a default; the user opts in by recording
    // a key combo in the settings UI.
    bindings.insert(
        "paste_last_transcript".to_string(),
        ShortcutBinding {
            id: "paste_last_transcript".to_string(),
            name: "Paste Last Transcript".to_string(),
            description: "Pastes the most recent transcription into the active app.".to_string(),
            default_binding: String::new(),
            current_binding: String::new(),
        },
    );

    AppSettings {
        bindings,
        push_to_talk: true,
        audio_feedback: false,
        audio_feedback_volume: default_audio_feedback_volume(),
        sound_theme: default_sound_theme(),
        start_hidden: default_start_hidden(),
        autostart_enabled: default_autostart_enabled(),
        update_checks_enabled: default_update_checks_enabled(),
        selected_model: "".to_string(),
        always_on_microphone: false,
        selected_microphone: None,
        clamshell_microphone: None,
        selected_output_device: None,
        translate_to_english: false,
        selected_language: default_selected_language(),
        overlay_position: default_overlay_position(),
        debug_mode: false,
        log_level: default_log_level(),
        custom_words: Vec::new(),
        model_unload_timeout: ModelUnloadTimeout::default(),
        word_correction_threshold: default_word_correction_threshold(),
        history_limit: default_history_limit(),
        recording_retention_period: default_recording_retention_period(),
        paste_method: PasteMethod::default(),
        clipboard_handling: ClipboardHandling::default(),
        auto_submit: default_auto_submit(),
        auto_submit_key: AutoSubmitKey::default(),
        post_process_enabled: default_post_process_enabled(),
        post_process_auto: default_post_process_auto(),
        post_process_provider_id: default_post_process_provider_id(),
        post_process_providers: default_post_process_providers(),
        post_process_api_keys: default_post_process_api_keys(),
        post_process_models: default_post_process_models(),
        post_process_prompts: default_post_process_prompts(),
        post_process_selected_prompt_id: None,
        mute_while_recording: false,
        append_trailing_space: false,
        app_language: default_app_language(),
        experimental_enabled: false,
        keyboard_implementation: KeyboardImplementation::default(),
        show_tray_icon: default_show_tray_icon(),
        paste_delay_ms: default_paste_delay_ms(),
        typing_tool: default_typing_tool(),
        external_script_path: None,
        custom_filler_words: None,
        whisper_accelerator: WhisperAcceleratorSetting::default(),
        ort_accelerator: OrtAcceleratorSetting::default(),
    }
}

impl AppSettings {
    pub fn active_post_process_provider(&self) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == self.post_process_provider_id)
    }

    pub fn post_process_provider(&self, provider_id: &str) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }

    pub fn post_process_provider_mut(
        &mut self,
        provider_id: &str,
    ) -> Option<&mut PostProcessProvider> {
        self.post_process_providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
    }
}

/// Store key holding the last settings that could not be read.
const UNREADABLE_SETTINGS_BACKUP_KEY: &str = "settings_unreadable_backup";

/// Falls back to default settings when the stored ones cannot be parsed.
///
/// The unreadable value is kept under [`UNREADABLE_SETTINGS_BACKUP_KEY`]
/// instead of being overwritten: it holds everything the user configured
/// (custom words, prompts, API keys, shortcuts), and a single bad field would
/// otherwise wipe all of it with no way back.
fn reset_unreadable_settings<R: tauri::Runtime>(
    store: &tauri_plugin_store::Store<R>,
    unreadable: serde_json::Value,
    error: &serde_json::Error,
) -> AppSettings {
    log::error!(
        "Stored settings are unreadable ({}). Resetting to defaults; the previous settings are kept under '{}' in the settings file.",
        error,
        UNREADABLE_SETTINGS_BACKUP_KEY
    );
    store.set(UNREADABLE_SETTINGS_BACKUP_KEY, unreadable);

    let default_settings = get_default_settings();
    store.set("settings", serde_json::to_value(&default_settings).unwrap());
    default_settings
}

pub fn load_or_create_app_settings(app: &AppHandle) -> AppSettings {
    // Initialize store
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    let mut settings = if let Some(settings_value) = store.get("settings") {
        // Parse the entire settings object
        match serde_json::from_value::<AppSettings>(settings_value.clone()) {
            Ok(mut settings) => {
                // Never log the settings themselves: they hold the API keys.
                debug!("Found existing settings");
                let default_settings = get_default_settings();
                let mut updated = false;

                // Merge default bindings into existing settings
                for (key, value) in default_settings.bindings {
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        settings.bindings.entry(key)
                    {
                        debug!("Adding missing binding: {}", entry.key());
                        entry.insert(value);
                        updated = true;
                    }
                }

                if updated {
                    debug!("Settings updated with new bindings");
                    store.set("settings", serde_json::to_value(&settings).unwrap());
                }

                settings
            }
            Err(e) => reset_unreadable_settings(&store, settings_value, &e),
        }
    } else {
        let default_settings = get_default_settings();
        store.set("settings", serde_json::to_value(&default_settings).unwrap());
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        store.set("settings", serde_json::to_value(&settings).unwrap());
    }

    settings
}

pub fn get_settings(app: &AppHandle) -> AppSettings {
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    let mut settings = if let Some(settings_value) = store.get("settings") {
        serde_json::from_value::<AppSettings>(settings_value.clone())
            .unwrap_or_else(|e| reset_unreadable_settings(&store, settings_value, &e))
    } else {
        let default_settings = get_default_settings();
        store.set("settings", serde_json::to_value(&default_settings).unwrap());
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        store.set("settings", serde_json::to_value(&settings).unwrap());
    }

    settings
}

pub fn write_settings(app: &AppHandle, settings: AppSettings) {
    let store = app
        .store(crate::portable::store_path(SETTINGS_STORE_PATH))
        .expect("Failed to initialize store");

    store.set("settings", serde_json::to_value(&settings).unwrap());
}

pub fn get_bindings(app: &AppHandle) -> HashMap<String, ShortcutBinding> {
    let settings = get_settings(app);

    settings.bindings
}

pub fn get_stored_binding(app: &AppHandle, id: &str) -> Option<ShortcutBinding> {
    get_bindings(app).get(id).cloned()
}

pub fn get_history_limit(app: &AppHandle) -> usize {
    let settings = get_settings(app);
    settings.history_limit
}

pub fn get_recording_retention_period(app: &AppHandle) -> RecordingRetentionPeriod {
    let settings = get_settings(app);
    settings.recording_retention_period
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_disable_auto_submit() {
        let settings = get_default_settings();
        assert!(!settings.auto_submit);
        assert_eq!(settings.auto_submit_key, AutoSubmitKey::Enter);
    }
}
