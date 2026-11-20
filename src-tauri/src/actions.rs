#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::is_microphone_access_denied;
use crate::managers::audio::{AudioRecordingManager, RecordingStopError};
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, AppSettings, APPLE_INTELLIGENCE_PROVIDER_ID};
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, warn};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri::{AppHandle, Emitter};

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Drop guard that notifies the [`TranscriptionCoordinator`] when the
/// transcription pipeline finishes — whether it completes normally or panics.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        // A panic skips the normal cleanup: without this the overlay would
        // stay on "transcribing" until the app is restarted.
        if std::thread::panicking() {
            reset_ui(&self.0);
        }
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
    }
}

/// How long an error message stays on the overlay before it hides.
const ERROR_OVERLAY_DURATION: Duration = Duration::from_secs(4);

/// Back to the resting state: no overlay, idle tray icon.
fn reset_ui(app: &AppHandle) {
    utils::hide_recording_overlay(app);
    change_tray_icon(app, TrayIconState::Idle);
}

/// Shows an error on the overlay for a few seconds and reports it to the main
/// window, so a failed dictation is never just silence.
fn report_pipeline_error(app: &AppHandle, error_type: &str, detail: String) {
    change_tray_icon(app, TrayIconState::Idle);
    let _ = app.emit(
        "recording-error",
        RecordingErrorEvent {
            error_type: error_type.to_string(),
            detail: Some(detail),
        },
    );
    utils::hide_recording_overlay_after(app, ERROR_OVERLAY_DURATION);
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction {
    post_process: bool,
}

/// Field name for structured output JSON schema
const TRANSCRIPTION_FIELD: &str = "transcription";

/// Strip invisible Unicode characters that some LLMs may insert
fn strip_invisible_chars(s: &str) -> String {
    s.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Strip reasoning/thinking blocks emitted by open-weight LLMs (Gemma, DeepSeek,
/// Qwen-thinking, etc.) before the actual answer. Without this, the entire
/// chain-of-thought ends up pasted into the user's target app.
static THINKING_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?si)<think>.*?</think>|<thinking>.*?</thinking>|<thought>.*?</thought>")
        .expect("thinking-block regex must compile")
});

pub(crate) fn strip_thinking_blocks(s: &str) -> String {
    THINKING_BLOCK_RE.replace_all(s, "").trim().to_string()
}

/// Combined cleanup applied to every LLM response before it is shown or pasted.
fn clean_llm_response(s: &str) -> String {
    strip_invisible_chars(&strip_thinking_blocks(s))
}

/// Build a system prompt from the user's prompt template.
/// Removes `${output}` placeholder since the transcription is sent as the user message.
fn build_system_prompt(prompt_template: &str) -> String {
    prompt_template.replace("${output}", "").trim().to_string()
}

pub async fn post_process_transcription(
    settings: &AppSettings,
    transcription: &str,
) -> Option<String> {
    let provider = match settings.active_post_process_provider().cloned() {
        Some(provider) => provider,
        None => {
            debug!("Post-processing enabled but no provider is selected");
            return None;
        }
    };

    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    if model.trim().is_empty() {
        debug!(
            "Post-processing skipped because provider '{}' has no model configured",
            provider.id
        );
        return None;
    }

    let selected_prompt_id = match &settings.post_process_selected_prompt_id {
        Some(id) => id.clone(),
        None => {
            debug!("Post-processing skipped because no prompt is selected");
            return None;
        }
    };

    let prompt = match settings
        .post_process_prompts
        .iter()
        .find(|prompt| prompt.id == selected_prompt_id)
    {
        Some(prompt) => prompt.prompt.clone(),
        None => {
            debug!(
                "Post-processing skipped because prompt '{}' was not found",
                selected_prompt_id
            );
            return None;
        }
    };

    if prompt.trim().is_empty() {
        debug!("Post-processing skipped because the selected prompt is empty");
        return None;
    }

    debug!(
        "Starting LLM post-processing with provider '{}' (model: {})",
        provider.id, model
    );

    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    if provider.supports_structured_output {
        debug!("Using structured outputs for provider '{}'", provider.id);

        let system_prompt = build_system_prompt(&prompt);
        let user_content = transcription.to_string();

        // Handle Apple Intelligence separately since it uses native Swift APIs
        if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                if !apple_intelligence::check_apple_intelligence_availability() {
                    debug!(
                        "Apple Intelligence selected but not currently available on this device"
                    );
                    return None;
                }

                let token_limit = model.trim().parse::<i32>().unwrap_or(0);
                return match apple_intelligence::process_text_with_system_prompt(
                    &system_prompt,
                    &user_content,
                    token_limit,
                ) {
                    Ok(result) => {
                        if result.trim().is_empty() {
                            debug!("Apple Intelligence returned an empty response");
                            None
                        } else {
                            let result = strip_invisible_chars(&result);
                            debug!(
                                "Apple Intelligence post-processing succeeded. Output length: {} chars",
                                result.len()
                            );
                            Some(result)
                        }
                    }
                    Err(err) => {
                        error!("Apple Intelligence post-processing failed: {}", err);
                        None
                    }
                };
            }

            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                debug!("Apple Intelligence provider selected on unsupported platform");
                return None;
            }
        }

        // Define JSON schema for transcription output
        let json_schema = serde_json::json!({
            "type": "object",
            "properties": {
                (TRANSCRIPTION_FIELD): {
                    "type": "string",
                    "description": "The cleaned and processed transcription text"
                }
            },
            "required": [TRANSCRIPTION_FIELD],
            "additionalProperties": false
        });

        match crate::llm_client::send_chat_completion_with_schema(
            &provider,
            api_key.clone(),
            &model,
            user_content,
            Some(system_prompt),
            Some(json_schema),
        )
        .await
        {
            Ok(Some(content)) => {
                // Parse the JSON response to extract the transcription field
                match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(json) => {
                        if let Some(transcription_value) =
                            json.get(TRANSCRIPTION_FIELD).and_then(|t| t.as_str())
                        {
                            let result = clean_llm_response(transcription_value);
                            debug!(
                                "Structured output post-processing succeeded for provider '{}'. Output length: {} chars",
                                provider.id,
                                result.len()
                            );
                            return Some(result);
                        } else {
                            error!("Structured output response missing 'transcription' field");
                            return Some(clean_llm_response(&content));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to parse structured output JSON: {}. Returning raw content.",
                            e
                        );
                        return Some(clean_llm_response(&content));
                    }
                }
            }
            Ok(None) => {
                error!("LLM API response has no content");
                return None;
            }
            Err(e) => {
                warn!(
                    "Structured output failed for provider '{}': {}. Falling back to legacy mode.",
                    provider.id, e
                );
                // Fall through to legacy mode below
            }
        }
    }

    // Legacy mode: Replace ${output} variable in the prompt with the actual text
    let processed_prompt = prompt.replace("${output}", transcription);
    debug!("Processed prompt length: {} chars", processed_prompt.len());

    match crate::llm_client::send_chat_completion(&provider, api_key, &model, processed_prompt)
        .await
    {
        Ok(Some(content)) => {
            let content = clean_llm_response(&content);
            debug!(
                "LLM post-processing succeeded for provider '{}'. Output length: {} chars",
                provider.id,
                content.len()
            );
            Some(content)
        }
        Ok(None) => {
            error!("LLM API response has no content");
            None
        }
        Err(e) => {
            error!(
                "LLM post-processing failed for provider '{}': {}. Falling back to original transcription.",
                provider.id,
                e
            );
            None
        }
    }
}

async fn maybe_convert_chinese_variant(
    settings: &AppSettings,
    transcription: &str,
) -> Option<String> {
    // Check if language is set to Simplified or Traditional Chinese
    let is_simplified = settings.selected_language == "zh-Hans";
    let is_traditional = settings.selected_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("selected_language is not Simplified or Traditional Chinese; skipping translation");
        return None;
    }

    debug!(
        "Starting Chinese translation using OpenCC for language: {}",
        settings.selected_language
    );

    // Use OpenCC to convert based on selected language
    let config = if is_simplified {
        // Convert Traditional Chinese to Simplified Chinese
        BuiltinConfig::Tw2sp
    } else {
        // Convert Simplified Chinese to Traditional Chinese
        BuiltinConfig::S2twp
    };

    match OpenCC::from_config(config) {
        Ok(converter) => {
            let converted = converter.convert(transcription);
            debug!(
                "OpenCC translation completed. Input length: {}, Output length: {}",
                transcription.len(),
                converted.len()
            );
            Some(converted)
        }
        Err(e) => {
            error!("Failed to initialize OpenCC converter: {}. Falling back to original transcription.", e);
            None
        }
    }
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        tm.clear_cancel();
        tm.initiate_model_load();

        let binding_id = binding_id.to_string();

        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Get the microphone mode to determine audio feedback timing
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        debug!("Microphone mode - always_on: {}", is_always_on);

        // Start recording first, then play the start sound and mute. The mute
        // only applies while a recording is running, so it has to come after.
        let recording_start_time = Instant::now();
        let recording_error = rm.try_start_recording(&binding_id).err();
        match &recording_error {
            None => {
                debug!("Recording started in {:?}", recording_start_time.elapsed());
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !is_always_on {
                        // Small delay to ensure the freshly opened microphone
                        // stream is active before the sound plays.
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    // The blocking helper returns at once when audio feedback
                    // is disabled, so mute always follows the sound.
                    play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    rm_clone.apply_mute();
                });
            }
            Some(e) => debug!("Failed to start recording: {}", e),
        }

        if recording_error.is_none() {
            change_tray_icon(app, TrayIconState::Recording);
            show_recording_overlay(app);
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            reset_ui(app);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else {
                    "unknown"
                };
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(err),
                    },
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        change_tray_icon(app, TrayIconState::Transcribing);
        show_transcribing_overlay(app);

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string();
        let post_process = self.post_process;

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            let samples = match rm.stop_recording(&binding_id) {
                Some(Ok(samples)) => samples,
                Some(Err(RecordingStopError::MicrophoneSilent(detail))) => {
                    // Keep the red "microphone silent" state up long enough to read,
                    // and tell the main window so the cause is never a mystery.
                    utils::show_microphone_silent_overlay(&ah);
                    report_pipeline_error(&ah, "microphone_silent", detail);
                    return;
                }
                None => {
                    debug!("No samples retrieved from recording stop");
                    reset_ui(&ah);
                    return;
                }
            };
            debug!(
                "Recording stopped and samples retrieved in {:?}, sample count: {}",
                stop_recording_time.elapsed(),
                samples.len()
            );

            let transcription_time = Instant::now();
            let samples_clone = samples.clone(); // Clone for history saving
            let transcription = match tm.transcribe(samples) {
                Ok(transcription) => transcription,
                Err(_) if tm.is_cancelled() => {
                    debug!("Transcription aborted by cancellation");
                    reset_ui(&ah);
                    return;
                }
                Err(err) => {
                    error!("Transcription failed: {}", err);
                    utils::show_transcription_error_overlay(&ah);
                    report_pipeline_error(&ah, "transcription_failed", err.to_string());
                    // Keep the recording: an empty text marks it as failed in the
                    // history, where it can be replayed or transcribed again.
                    let hm_clone = Arc::clone(&hm);
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = hm_clone
                            .save_transcription(samples_clone, String::new(), None, None)
                            .await
                        {
                            error!("Failed to save failed recording to history: {}", e);
                        }
                    });
                    return;
                }
            };
            debug!(
                "Transcription completed in {:?}: '{}'",
                transcription_time.elapsed(),
                transcription
            );

            // Only Whisper stops mid-inference on cancel; any other engine
            // runs to the end, and its result must not be pasted.
            if transcription.is_empty() || tm.is_cancelled() {
                reset_ui(&ah);
                return;
            }

            let settings = get_settings(&ah);
            // Auto post-processing: when both post_process_enabled and
            // post_process_auto are on, every transcription is post-processed,
            // not just the dedicated hotkey.
            let post_process =
                post_process || (settings.post_process_enabled && settings.post_process_auto);
            let mut final_text = transcription.clone();
            let mut post_processed_text: Option<String> = None;
            let mut post_process_prompt: Option<String> = None;

            // First, check if Chinese variant conversion is needed
            if let Some(converted_text) =
                maybe_convert_chinese_variant(&settings, &transcription).await
            {
                final_text = converted_text;
            }

            // Then apply LLM post-processing if requested.
            // Uses final_text which may already have Chinese conversion applied
            let processed = if post_process {
                show_processing_overlay(&ah);
                post_process_transcription(&settings, &final_text).await
            } else {
                None
            };

            // The LLM call can take seconds: honour a cancel made meanwhile.
            if tm.is_cancelled() {
                debug!("Cancelled during post-processing, discarding result");
                reset_ui(&ah);
                return;
            }

            // Flash a red "processing error" overlay when PP was attempted
            // but failed (network error, bad API key, no model, etc.) so the
            // user knows the raw transcription is being pasted as a fallback.
            let pp_failed = post_process && processed.is_none();
            if pp_failed {
                utils::show_processing_error_overlay(&ah);
            }
            if let Some(processed_text) = processed {
                post_processed_text = Some(processed_text.clone());
                final_text = processed_text;

                // Get the prompt that was used
                post_process_prompt = settings
                    .post_process_selected_prompt_id
                    .as_ref()
                    .and_then(|id| settings.post_process_prompts.iter().find(|p| &p.id == id))
                    .map(|p| p.prompt.clone());
            } else if final_text != transcription {
                // Chinese conversion was applied but no LLM post-processing
                post_processed_text = Some(final_text.clone());
            }

            // Save to history with post-processed text and prompt
            let hm_clone = Arc::clone(&hm);
            tauri::async_runtime::spawn(async move {
                if let Err(e) = hm_clone
                    .save_transcription(
                        samples_clone,
                        transcription,
                        post_processed_text,
                        post_process_prompt,
                    )
                    .await
                {
                    error!("Failed to save transcription to history: {}", e);
                }
            });

            // Paste the final text (either processed or original)
            let ah_clone = ah.clone();
            let paste_time = Instant::now();
            ah.run_on_main_thread(move || {
                match utils::paste(final_text, ah_clone.clone()) {
                    Ok(()) => debug!("Text pasted successfully in {:?}", paste_time.elapsed()),
                    Err(e) => error!("Failed to paste transcription: {}", e),
                }
                change_tray_icon(&ah_clone, TrayIconState::Idle);
                if pp_failed {
                    // Keep the red error state visible briefly so the user
                    // notices the post-processing fallback occurred.
                    utils::hide_recording_overlay_after(&ah_clone, Duration::from_secs(3));
                } else {
                    utils::hide_recording_overlay(&ah_clone);
                }
            })
            .unwrap_or_else(|e| {
                error!("Failed to run paste on main thread: {:?}", e);
                reset_ui(&ah);
            });
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Paste Last Transcript Action
//
// Re-injects the most recent transcription into the active app using the same
// output path as a fresh transcription (honours paste_method, auto_submit, etc.).
struct PasteLastTranscriptAction;

impl ShortcutAction for PasteLastTranscriptAction {
    fn start(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Intentionally a no-op: we paste on key RELEASE (see `stop`). Pasting on
        // press would synthesize input while the user's shortcut modifiers
        // (Ctrl/Shift/etc.) are still physically held, which corrupts the paste
        // in terminals (a held Ctrl turns typed chars into control sequences).
    }

    fn stop(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            // Give the OS a moment to process the user releasing the shortcut
            // keys, so no modifier is still down when we synthesize the paste.
            // This mirrors the normal transcription path, which only pastes well
            // after the keys are released.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;

            let entry = {
                let history_manager = app.state::<Arc<HistoryManager>>();
                match history_manager.get_latest_entry() {
                    Ok(Some(entry)) => entry,
                    Ok(None) => {
                        warn!("No transcription history available to paste.");
                        return;
                    }
                    Err(err) => {
                        error!("Failed to fetch last transcription entry: {}", err);
                        return;
                    }
                }
            };

            // Prefer the post-processed text (what was actually produced) and
            // fall back to the raw transcription.
            let text = entry
                .post_processed_text
                .unwrap_or(entry.transcription_text);

            // Enigo-based pasting must run on the main thread, and it honours the
            // user's configured paste_method (direct, ctrl+v, etc.), mirroring
            // the normal transcription output path.
            let app_for_paste = app.clone();
            if let Err(e) = app.run_on_main_thread(move || {
                if let Err(e) = utils::paste(text, app_for_paste.clone()) {
                    error!("Failed to paste last transcript: {}", e);
                }
            }) {
                error!("Failed to schedule paste on main thread: {:?}", e);
            }
        });
    }
}

// Test Action
struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})", // Changed "Pressed" to "Started" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})", // Changed "Released" to "Stopped" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction {
            post_process: false,
        }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_post_process".to_string(),
        Arc::new(TranscribeAction { post_process: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "paste_last_transcript".to_string(),
        Arc::new(PasteLastTranscriptAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});
