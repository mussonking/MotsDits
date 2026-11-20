use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::audio_toolkit::{
    apply_custom_word_aliases, apply_custom_words, filter_transcription_output,
};
use crate::managers::model::{EngineType, ModelInfo, ModelManager};
use crate::managers::whisper::CancellableWhisperEngine;
use crate::platform;
use crate::settings::{
    get_settings, ModelUnloadTimeout, OrtAcceleratorSetting, WhisperAcceleratorSetting,
};
use anyhow::Result;
use log::{debug, error, info, warn};
use serde::Serialize;
use specta::Type;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use transcribe_rs::{
    onnx::{
        canary::CanaryModel,
        gigaam::GigaAMModel,
        moonshine::{MoonshineModel, MoonshineVariant, StreamingModel},
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        sense_voice::{SenseVoiceModel, SenseVoiceParams},
        Quantization,
    },
    whisper_cpp::WhisperInferenceParams,
    SpeechModel, TranscribeOptions,
};

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

const MODEL_LOAD_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

/// Processing time above which a transcription is logged as abnormally slow.
///
/// Healthy GPU inference runs far faster than real time (about 1.5s for 40s
/// of audio), while degraded inference costs several seconds even for a short
/// clip. A fixed floor plus a tenth of the audio length separates the two.
fn slow_transcription_threshold_ms(audio_ms: u64) -> u64 {
    2500 + audio_ms / 10
}

enum LoadedEngine {
    Whisper(CancellableWhisperEngine),
    Parakeet(ParakeetModel),
    Moonshine(MoonshineModel),
    MoonshineStreaming(StreamingModel),
    SenseVoice(SenseVoiceModel),
    GigaAM(GigaAMModel),
    Canary(CanaryModel),
}

/// RAII guard that clears the `is_loading` flag and notifies waiters on drop.
/// Ensures the loading flag is always reset, even on early returns or panics.
pub struct LoadingGuard {
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        let mut is_loading = self.is_loading.lock().unwrap();
        *is_loading = false;
        self.loading_condvar.notify_all();
    }
}

#[derive(Clone)]
pub struct TranscriptionManager {
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    cancel_requested: Arc<AtomicBool>,
    /// Serializes `transcribe()`: it takes the engine out of `engine` while it
    /// runs, so a second caller (history retry vs. live dictation) would find
    /// no engine and try to load another copy of the model.
    transcribe_lock: Arc<Mutex<()>>,
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: Arc::new(Mutex::new(None)),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            )),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            is_loading: Arc::new(Mutex::new(false)),
            loading_condvar: Arc::new(Condvar::new()),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            transcribe_lock: Arc::new(Mutex::new(())),
        };

        // Start the idle watcher
        {
            let app_handle_cloned = app_handle.clone();
            let manager_cloned = manager.clone();
            let shutdown_signal = manager.shutdown_signal.clone();
            let handle = thread::spawn(move || {
                debug!("Idle watcher thread started");
                while !shutdown_signal.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10)); // Check every 10 seconds

                    // Check shutdown signal again after sleep
                    if shutdown_signal.load(Ordering::Relaxed) {
                        break;
                    }

                    let settings = get_settings(&app_handle_cloned);
                    let timeout_seconds = settings.model_unload_timeout.to_seconds();

                    if let Some(limit_seconds) = timeout_seconds {
                        let last = manager_cloned.last_activity.load(Ordering::Relaxed);
                        let now_ms = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let idle_ms = now_ms.saturating_sub(last);
                        let limit_ms = limit_seconds * 1000;

                        if idle_ms > limit_ms {
                            // idle -> unload
                            if manager_cloned.is_model_loaded() {
                                let unload_start = std::time::Instant::now();
                                info!(
                                    "Model idle for {}s (limit: {}s), unloading",
                                    idle_ms / 1000,
                                    limit_seconds
                                );
                                match manager_cloned.unload_model() {
                                    Ok(()) => {
                                        let unload_duration = unload_start.elapsed();
                                        info!(
                                            "Model unloaded due to inactivity (took {}ms)",
                                            unload_duration.as_millis()
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to unload idle model: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                debug!("Idle watcher thread shutting down gracefully");
            });
            *manager.watcher_handle.lock().unwrap() = Some(handle);
        }

        Ok(manager)
    }

    /// Lock the engine mutex, recovering from poison if a previous transcription panicked.
    fn lock_engine(&self) -> MutexGuard<'_, Option<LoadedEngine>> {
        self.engine.lock().unwrap_or_else(|poisoned| {
            warn!("Engine mutex was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }

    pub fn is_model_loaded(&self) -> bool {
        let engine = self.lock_engine();
        engine.is_some()
    }

    pub fn is_loading(&self) -> bool {
        *self.is_loading.lock().unwrap()
    }

    /// Atomically check whether a model load is in progress and, if not, mark
    /// one as starting. Returns a [`LoadingGuard`] whose [`Drop`] impl will
    /// clear the flag and wake waiters. Returns `None` if a load is already in
    /// progress.
    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return None;
        }
        *is_loading = true;
        Some(LoadingGuard {
            is_loading: self.is_loading.clone(),
            loading_condvar: self.loading_condvar.clone(),
        })
    }

    pub fn unload_model(&self) -> Result<()> {
        let unload_start = std::time::Instant::now();
        debug!("Starting to unload model");

        {
            let mut engine = self.lock_engine();
            // v0.3.0: no explicit unload — dropping the engine frees all resources
            *engine = None;
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        self.emit_model_state("unloaded", None, None, None);

        let unload_duration = unload_start.elapsed();
        debug!(
            "Model unloaded manually (took {}ms)",
            unload_duration.as_millis()
        );
        Ok(())
    }

    /// Unloads the model immediately if the setting is enabled and the model is loaded
    pub fn maybe_unload_immediately(&self, context: &str) {
        let settings = get_settings(&self.app_handle);
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately
            && self.is_model_loaded()
        {
            info!("Immediately unloading model after {}", context);
            if let Err(e) = self.unload_model() {
                warn!("Failed to immediately unload model: {}", e);
            }
        }
    }

    pub fn cancel_current_transcription(&self) {
        self.cancel_requested.store(true, Ordering::Relaxed);
    }

    /// Forgets a previous cancellation. Called when a new recording starts.
    pub fn clear_cancel(&self) {
        self.cancel_requested.store(false, Ordering::Relaxed);
    }

    /// Whether the user cancelled since the current recording started. Only
    /// Whisper can abort mid-inference; the pipeline checks this so a result
    /// from any other engine (or from post-processing) is not pasted anyway.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::Relaxed)
    }

    fn touch_activity(&self) {
        self.last_activity.store(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    fn emit_model_state(
        &self,
        event_type: &str,
        model_id: Option<&str>,
        model_name: Option<String>,
        error: Option<String>,
    ) {
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: event_type.to_string(),
                model_id: model_id.map(str::to_string),
                model_name,
                error,
            },
        );
    }

    /// Builds the engine for `model_id`. Failures are reported to the
    /// frontend by [`Self::load_model`], in one place.
    fn load_engine(&self, model_id: &str, model_info: Option<&ModelInfo>) -> Result<LoadedEngine> {
        let model_info =
            model_info.ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;
        if !model_info.is_downloaded {
            return Err(anyhow::anyhow!("Model not downloaded"));
        }
        let model_path = self.model_manager.get_model_path(model_id)?;

        let engine = match model_info.engine_type {
            EngineType::Whisper => CancellableWhisperEngine::load(&model_path)
                .map(LoadedEngine::Whisper)
                .map_err(|e| e.to_string()),
            EngineType::Parakeet => ParakeetModel::load(&model_path, &Quantization::Int8)
                .map(LoadedEngine::Parakeet)
                .map_err(|e| e.to_string()),
            EngineType::Moonshine => MoonshineModel::load(
                &model_path,
                MoonshineVariant::Base,
                &Quantization::default(),
            )
            .map(LoadedEngine::Moonshine)
            .map_err(|e| e.to_string()),
            EngineType::MoonshineStreaming => {
                StreamingModel::load(&model_path, 0, &Quantization::default())
                    .map(LoadedEngine::MoonshineStreaming)
                    .map_err(|e| e.to_string())
            }
            EngineType::SenseVoice => SenseVoiceModel::load(&model_path, &Quantization::Int8)
                .map(LoadedEngine::SenseVoice)
                .map_err(|e| e.to_string()),
            EngineType::GigaAM => GigaAMModel::load(&model_path, &Quantization::Int8)
                .map(LoadedEngine::GigaAM)
                .map_err(|e| e.to_string()),
            EngineType::Canary => CanaryModel::load(&model_path, &Quantization::Int8)
                .map(LoadedEngine::Canary)
                .map_err(|e| e.to_string()),
        };

        engine.map_err(|e| {
            anyhow::anyhow!(
                "Failed to load {:?} model {}: {}",
                model_info.engine_type,
                model_id,
                e
            )
        })
    }

    /// Makes sure a model is loaded before transcribing: waits for a load in
    /// progress, and loads the selected model itself when there is none.
    ///
    /// The model can be missing even though a load was started with the
    /// recording: the idle watcher may unload it while the user is still
    /// speaking, or the background load may have failed. Loading here keeps
    /// that from throwing the recording away.
    fn ensure_model_loaded(&self) -> Result<()> {
        let wait_start = Instant::now();
        loop {
            {
                let mut is_loading = self.is_loading.lock().unwrap();
                while *is_loading {
                    let remaining = MODEL_LOAD_WAIT_TIMEOUT.saturating_sub(wait_start.elapsed());
                    if remaining.is_zero() {
                        warn!(
                            "Timed out waiting for model load after {}s",
                            MODEL_LOAD_WAIT_TIMEOUT.as_secs()
                        );
                        return Err(anyhow::anyhow!(
                            "Timed out waiting for the transcription model to finish loading after {}s.",
                            MODEL_LOAD_WAIT_TIMEOUT.as_secs()
                        ));
                    }
                    is_loading = self
                        .loading_condvar
                        .wait_timeout(is_loading, remaining)
                        .unwrap()
                        .0;
                }
            }

            // Engine check with the loading lock released. Taking `engine`
            // while still holding `is_loading` inverts the lock order used by
            // LoadingGuard::drop (which takes `is_loading` after a load path
            // has touched `engine`) and deadlocks both threads.
            if self.is_model_loaded() {
                return Ok(());
            }

            // Another thread may have claimed the load in between: wait again.
            let Some(_loading_guard) = self.try_start_loading() else {
                continue;
            };
            let settings = get_settings(&self.app_handle);
            info!(
                "No model loaded at transcription time, loading '{}' now",
                settings.selected_model
            );
            return self.load_model(&settings.selected_model);
        }
    }

    pub fn load_model(&self, model_id: &str) -> Result<()> {
        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        self.emit_model_state("loading_started", Some(model_id), None, None);

        let model_info = self.model_manager.get_model_info(model_id);
        let model_name = model_info.as_ref().map(|info| info.name.clone());

        let loaded_engine = match self.load_engine(model_id, model_info.as_ref()) {
            Ok(engine) => engine,
            Err(e) => {
                self.emit_model_state(
                    "loading_failed",
                    Some(model_id),
                    model_name,
                    Some(e.to_string()),
                );
                return Err(e);
            }
        };

        // Update the current engine and model ID
        {
            let mut engine = self.lock_engine();
            *engine = Some(loaded_engine);
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = Some(model_id.to_string());
        }

        // Reset idle timer so the watcher doesn't immediately unload a just-loaded model
        self.touch_activity();

        self.emit_model_state("loading_completed", Some(model_id), model_name, None);

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );
        info!(
            "GPU after model load: {}",
            platform::gpu_diag::gpu_memory_summary()
        );
        Ok(())
    }

    /// Kicks off the model loading in a background thread if it's not already loaded
    pub fn initiate_model_load(&self) {
        // A recording is starting: the model is in use from now on, so the
        // idle watcher must not count the time spent speaking as idle time.
        self.touch_activity();

        if self.is_model_loaded() {
            return;
        }

        let Some(loading_guard) = self.try_start_loading() else {
            return;
        };

        let self_clone = self.clone();
        thread::spawn(move || {
            let _loading_guard = loading_guard;
            let settings = get_settings(&self_clone.app_handle);
            if let Err(e) = self_clone.load_model(&settings.selected_model) {
                error!("Failed to load model: {}", e);
            }
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = self.current_model_id.lock().unwrap();
        current_model.clone()
    }

    pub fn transcribe(&self, audio: Vec<f32>) -> Result<String> {
        let _serialized = self
            .transcribe_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.touch_activity();

        let st = std::time::Instant::now();

        debug!("Audio vector length: {}", audio.len());

        if audio.is_empty() {
            debug!("Empty audio vector");
            self.maybe_unload_immediately("empty audio");
            return Ok(String::new());
        }

        self.ensure_model_loaded()?;

        // Timed separately from `st` so waiting on a model load does not count
        // as slow inference.
        let inference_start = Instant::now();
        let audio_ms = audio.len() as u64 * 1000 / u64::from(WHISPER_SAMPLE_RATE);

        // Get current settings for configuration
        let settings = get_settings(&self.app_handle);

        // Validate selected language against the model's supported languages.
        // If the language isn't supported, fall back to "auto" to prevent errors.
        let validated_language = if settings.selected_language == "auto" {
            "auto".to_string()
        } else {
            let is_supported = self
                .model_manager
                .get_model_info(&settings.selected_model)
                .map(|info| {
                    info.supported_languages.is_empty()
                        || info
                            .supported_languages
                            .contains(&settings.selected_language)
                })
                .unwrap_or(true);

            if is_supported {
                settings.selected_language.clone()
            } else {
                warn!(
                    "Language '{}' not supported by current model, falling back to auto-detect",
                    settings.selected_language
                );
                "auto".to_string()
            }
        };

        // Perform transcription with the appropriate engine.
        // We use catch_unwind to prevent engine panics from poisoning the mutex,
        // which would make the app hang indefinitely on subsequent operations.
        let result = {
            let mut engine_guard = self.lock_engine();

            // Take the engine out so we own it during transcription.
            // If the engine panics, we simply don't put it back (effectively unloading it)
            // instead of poisoning the mutex.
            let mut engine = match engine_guard.take() {
                Some(e) => e,
                None => {
                    return Err(anyhow::anyhow!(
                        "Model failed to load after auto-load attempt. Please check your model settings."
                    ));
                }
            };

            // Release the lock before transcribing — no mutex held during the engine call
            drop(engine_guard);

            let transcribe_result = catch_unwind(AssertUnwindSafe(
                || -> Result<transcribe_rs::TranscriptionResult> {
                    match &mut engine {
                        LoadedEngine::Whisper(whisper_engine) => {
                            let whisper_language = if validated_language == "auto" {
                                None
                            } else {
                                let normalized = if validated_language == "zh-Hans"
                                    || validated_language == "zh-Hant"
                                {
                                    "zh".to_string()
                                } else {
                                    validated_language.clone()
                                };
                                Some(normalized)
                            };

                            let params = WhisperInferenceParams {
                                language: whisper_language,
                                translate: settings.translate_to_english,
                                n_threads: platform::transcription::whisper_inference_threads(),
                                initial_prompt: if settings.custom_words.is_empty() {
                                    None
                                } else {
                                    Some(
                                        settings
                                            .custom_words
                                            .iter()
                                            .map(|e| e.word.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    )
                                },
                                ..Default::default()
                            };

                            whisper_engine
                                .transcribe_with(&audio, &params, self.cancel_requested.clone())
                                .map_err(|e| anyhow::anyhow!("Whisper transcription failed: {}", e))
                        }
                        LoadedEngine::Parakeet(parakeet_engine) => {
                            let params = ParakeetParams {
                                timestamp_granularity: Some(TimestampGranularity::Segment),
                                ..Default::default()
                            };
                            parakeet_engine
                                .transcribe_with(&audio, &params)
                                .map_err(|e| {
                                    anyhow::anyhow!("Parakeet transcription failed: {}", e)
                                })
                        }
                        LoadedEngine::Moonshine(moonshine_engine) => moonshine_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| anyhow::anyhow!("Moonshine transcription failed: {}", e)),
                        LoadedEngine::MoonshineStreaming(streaming_engine) => streaming_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| {
                                anyhow::anyhow!("Moonshine streaming transcription failed: {}", e)
                            }),
                        LoadedEngine::SenseVoice(sense_voice_engine) => {
                            let language = match validated_language.as_str() {
                                "zh" | "zh-Hans" | "zh-Hant" => Some("zh".to_string()),
                                "en" => Some("en".to_string()),
                                "ja" => Some("ja".to_string()),
                                "ko" => Some("ko".to_string()),
                                "yue" => Some("yue".to_string()),
                                _ => None,
                            };
                            let params = SenseVoiceParams {
                                language,
                                use_itn: Some(true),
                            };
                            sense_voice_engine
                                .transcribe_with(&audio, &params)
                                .map_err(|e| {
                                    anyhow::anyhow!("SenseVoice transcription failed: {}", e)
                                })
                        }
                        LoadedEngine::GigaAM(gigaam_engine) => gigaam_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| anyhow::anyhow!("GigaAM transcription failed: {}", e)),
                        LoadedEngine::Canary(canary_engine) => {
                            let lang = if validated_language == "auto" {
                                None
                            } else {
                                Some(validated_language.clone())
                            };
                            let options = TranscribeOptions {
                                language: lang,
                                translate: settings.translate_to_english,
                                ..Default::default()
                            };
                            canary_engine
                                .transcribe(&audio, &options)
                                .map_err(|e| anyhow::anyhow!("Canary transcription failed: {}", e))
                        }
                    }
                },
            ));

            match transcribe_result {
                Ok(inner_result) => {
                    // Success or normal error — put the engine back
                    let mut engine_guard = self.lock_engine();
                    *engine_guard = Some(engine);
                    inner_result?
                }
                Err(panic_payload) => {
                    // Engine panicked — do NOT put it back (it's in an unknown state).
                    // The engine is dropped here, effectively unloading it.
                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    error!(
                        "Transcription engine panicked: {}. Model has been unloaded.",
                        panic_msg
                    );

                    // Clear the model ID so it will be reloaded on next attempt
                    {
                        let mut current_model = self
                            .current_model_id
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *current_model = None;
                    }

                    self.emit_model_state(
                        "unloaded",
                        None,
                        None,
                        Some(format!("Engine panicked: {}", panic_msg)),
                    );

                    return Err(anyhow::anyhow!(
                        "Transcription engine panicked: {}. The model has been unloaded and will reload on next attempt.",
                        panic_msg
                    ));
                }
            }
        };

        // Apply word correction if custom words are configured. Whisper already
        // receives target words as an initial prompt, but hard aliases are still
        // deterministic replacements that need a post-pass.
        let is_whisper = self
            .model_manager
            .get_model_info(&settings.selected_model)
            .map(|info| matches!(info.engine_type, EngineType::Whisper))
            .unwrap_or(false);

        let corrected_result = if settings.custom_words.is_empty() {
            result.text
        } else if is_whisper {
            apply_custom_word_aliases(&result.text, &settings.custom_words)
        } else {
            apply_custom_words(
                &result.text,
                &settings.custom_words,
                settings.word_correction_threshold,
            )
        };

        // Filter out filler words and hallucinations
        let filtered_result = filter_transcription_output(
            &corrected_result,
            &settings.app_language,
            &settings.custom_filler_words,
        );

        let et = std::time::Instant::now();
        let translation_note = if settings.translate_to_english {
            " (translated)"
        } else {
            ""
        };
        info!(
            "Transcription completed in {}ms{}",
            (et - st).as_millis(),
            translation_note
        );

        let inference_ms = inference_start.elapsed().as_millis() as u64;
        if inference_ms > slow_transcription_threshold_ms(audio_ms) {
            warn!(
                "Slow transcription: {}ms of processing for {}ms of audio. GPU: {}",
                inference_ms,
                audio_ms,
                platform::gpu_diag::gpu_memory_summary()
            );
        }

        let final_result = filtered_result;

        if final_result.is_empty() {
            info!("Transcription result is empty");
        } else {
            info!("Transcription result: {}", final_result);
        }

        self.maybe_unload_immediately("transcription");

        Ok(final_result)
    }
}

/// Apply the user's accelerator preferences to the transcribe-rs global atomics.
/// Called on startup and whenever the user changes the setting.
pub fn apply_accelerator_settings(app: &tauri::AppHandle) {
    use transcribe_rs::accel;

    let settings = get_settings(app);

    let whisper_pref = match settings.whisper_accelerator {
        WhisperAcceleratorSetting::Auto => accel::WhisperAccelerator::Auto,
        WhisperAcceleratorSetting::Cpu => accel::WhisperAccelerator::CpuOnly,
        WhisperAcceleratorSetting::Gpu => accel::WhisperAccelerator::Gpu,
    };
    accel::set_whisper_accelerator(whisper_pref);
    info!("Whisper accelerator set to: {}", whisper_pref);

    let ort_pref = match settings.ort_accelerator {
        OrtAcceleratorSetting::Auto => accel::OrtAccelerator::Auto,
        OrtAcceleratorSetting::Cpu => accel::OrtAccelerator::CpuOnly,
        OrtAcceleratorSetting::Cuda => accel::OrtAccelerator::Cuda,
        OrtAcceleratorSetting::DirectMl => accel::OrtAccelerator::DirectMl,
        OrtAcceleratorSetting::Rocm => accel::OrtAccelerator::Rocm,
    };
    accel::set_ort_accelerator(ort_pref);
    info!("ORT accelerator set to: {}", ort_pref);
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub whisper: Vec<String>,
    pub ort: Vec<String>,
}

/// Return which accelerators are compiled into this build.
pub fn get_available_accelerators() -> AvailableAccelerators {
    use transcribe_rs::accel::OrtAccelerator;

    let ort_options: Vec<String> = OrtAccelerator::available()
        .into_iter()
        .map(|a| a.to_string())
        .collect();

    let whisper_options = vec!["auto".to_string(), "cpu".to_string(), "gpu".to_string()];

    AvailableAccelerators {
        whisper: whisper_options,
        ort: ort_options,
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        // Skip shutdown unless this is the very last clone. TranscriptionManager
        // is cloned by initiate_model_load() and the watcher thread — those
        // clones dropping must not kill the watcher. The watcher thread holds
        // its own clone, so engine's strong_count is always >= 2 while the
        // watcher is alive. When it reaches 1, only this instance remains
        // and we can safely shut down.
        if Arc::strong_count(&self.engine) > 1 {
            return;
        }

        // Signal the watcher thread to shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wait for the thread to finish gracefully
        if let Some(handle) = self.watcher_handle.lock().unwrap().take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
