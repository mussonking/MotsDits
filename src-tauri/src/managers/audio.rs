use crate::audio_toolkit::{vad::SmoothedVad, AudioRecorder, MicrophoneSilent, SileroVad};
use crate::platform::audio as platform_audio;
use crate::settings::get_settings;
use crate::utils;
use log::{debug, error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::Manager;

const WHISPER_SAMPLE_RATE: usize = 16000;

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone, Debug)]
pub enum RecordingState {
    Idle,
    Recording { binding_id: String },
}

#[derive(Clone, Debug)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/* ──────────────────────────────────────────────────────────────── */

fn create_audio_recorder(
    vad_path: &str,
    app_handle: &tauri::AppHandle,
) -> Result<AudioRecorder, anyhow::Error> {
    let mut recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?;

    if platform_audio::uses_recording_vad() {
        debug!("Initializing recording VAD");
        let silero = SileroVad::new(vad_path, 0.3)
            .map_err(|e| anyhow::anyhow!("Failed to create SileroVad: {}", e))?;
        let smoothed_vad = SmoothedVad::new(Box::new(silero), 15, 15, 2);
        recorder = recorder.with_vad(Box::new(smoothed_vad));
    } else {
        debug!("Recording VAD disabled on this platform");
    }

    let recorder = recorder
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
            }
        })
        .with_silent_callback({
            let app_handle = app_handle.clone();
            move |silent| {
                if silent {
                    utils::show_microphone_silent_overlay(&app_handle);
                } else {
                    utils::show_recording_overlay(&app_handle);
                }
            }
        });

    Ok(recorder)
}

/// Why a recording produced no usable audio.
#[derive(Debug)]
pub enum RecordingStopError {
    /// The stream was open but the device never delivered audio (stalled driver).
    MicrophoneSilent(String),
}

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone)]
pub struct AudioRecordingManager {
    state: Arc<Mutex<RecordingState>>,
    mode: Arc<Mutex<MicrophoneMode>>,
    app_handle: tauri::AppHandle,

    // Lock order: `is_open` before `recorder`. `did_mute` is never held while
    // taking `is_open` or `recorder`, so the mute path cannot deadlock the
    // stream path.
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    is_open: Arc<Mutex<bool>>,
    did_mute: Arc<Mutex<bool>>,
    /// The selected microphone changed while a recording was running; the
    /// stream is reopened on the new device once that recording ends.
    reopen_after_recording: Arc<AtomicBool>,
}

impl AudioRecordingManager {
    /* ---------- construction ------------------------------------------------ */

    pub fn new(app: &tauri::AppHandle) -> Result<Self, anyhow::Error> {
        let settings = get_settings(app);
        let mode = if settings.always_on_microphone {
            MicrophoneMode::AlwaysOn
        } else {
            MicrophoneMode::OnDemand
        };

        let manager = Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            mode: Arc::new(Mutex::new(mode.clone())),
            app_handle: app.clone(),

            recorder: Arc::new(Mutex::new(None)),
            is_open: Arc::new(Mutex::new(false)),
            did_mute: Arc::new(Mutex::new(false)),
            reopen_after_recording: Arc::new(AtomicBool::new(false)),
        };

        // Always-on?  Open immediately. A missing or unplugged microphone must
        // not keep the app from starting: the stream is opened again on the
        // first recording.
        if matches!(mode, MicrophoneMode::AlwaysOn) {
            if let Err(e) = manager.start_microphone_stream() {
                error!("Failed to open always-on microphone at startup: {e}");
            }
        }

        Ok(manager)
    }

    /* ---------- helper methods --------------------------------------------- */

    /* ---------- microphone life-cycle -------------------------------------- */

    /// Applies mute if mute_while_recording is enabled and a recording is running
    pub fn apply_mute(&self) {
        if !get_settings(&self.app_handle).mute_while_recording {
            return;
        }
        let mut did_mute_guard = self.did_mute.lock().unwrap();

        // Mute is requested after the start sound, which can end after a very
        // short recording is already over. Stopping flips the state to idle
        // before it unmutes, so checking it here (under the mute lock) means a
        // late request can never leave the system muted.
        if self.is_recording() {
            // If the user had already muted the system, leave it alone: we only
            // undo what we did, so it stays muted after the recording.
            if platform_audio::is_output_muted() == Some(true) {
                debug!("Output already muted; leaving it as the user set it");
                return;
            }
            platform_audio::set_output_muted(true);
            *did_mute_guard = true;
            debug!("Mute applied");
        }
    }

    /// Removes mute if it was applied
    pub fn remove_mute(&self) {
        let mut did_mute_guard = self.did_mute.lock().unwrap();
        if *did_mute_guard {
            platform_audio::set_output_muted(false);
            *did_mute_guard = false;
            debug!("Mute removed");
        }
    }

    pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error> {
        let mut open_flag = self.is_open.lock().unwrap();
        if *open_flag {
            debug!("Microphone stream already active");
            return Ok(());
        }

        let start_time = Instant::now();

        let vad_path = self
            .app_handle
            .path()
            .resolve(
                "resources/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {}", e))?;
        let vad_path = if vad_path.exists() {
            vad_path
        } else {
            self.app_handle
                .path()
                .resolve(
                    "resources/models/silero_vad_v4.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| anyhow::anyhow!("Failed to resolve VAD fallback path: {}", e))?
        };
        debug!("Preparing microphone recorder");
        let mut recorder_opt = self.recorder.lock().unwrap();

        if recorder_opt.is_none() {
            *recorder_opt = Some(create_audio_recorder(
                vad_path.to_str().unwrap(),
                &self.app_handle,
            )?);
        }

        // Get the selected device from settings, considering clamshell mode
        let settings = get_settings(&self.app_handle);
        debug!("Resolving selected microphone device");
        let selected_device = platform_audio::resolve_input_device(&settings);

        if let Some(rec) = recorder_opt.as_mut() {
            debug!("Opening microphone recorder");
            platform_audio::open_recorder(rec, selected_device)?;
        }

        *open_flag = true;
        info!(
            "Microphone stream initialized in {:?}",
            start_time.elapsed()
        );
        Ok(())
    }

    pub fn stop_microphone_stream(&self) {
        self.remove_mute();

        let mut open_flag = self.is_open.lock().unwrap();
        if !*open_flag {
            return;
        }

        if let Some(rec) = self.recorder.lock().unwrap().as_mut() {
            let _ = rec.close();
        }

        *open_flag = false;
        debug!("Microphone stream stopped");
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        *self.mode.lock().unwrap() = new_mode.clone();

        match new_mode {
            MicrophoneMode::AlwaysOn => self.start_microphone_stream()?,
            MicrophoneMode::OnDemand => {
                // A recording in progress closes the stream itself when it ends.
                if !self.is_recording() {
                    self.stop_microphone_stream();
                }
            }
        }
        Ok(())
    }

    /* ---------- recording --------------------------------------------------- */

    pub fn try_start_recording(&self, binding_id: &str) -> Result<(), String> {
        {
            let state = self.state.lock().unwrap();
            if !matches!(*state, RecordingState::Idle) {
                return Err("Already recording".to_string());
            }
        }

        // Ensure the microphone is open (a no-op when it already is). This also
        // covers always-on mode, where the stream may have been closed after a
        // device failure or never opened at startup. Do not hold the recording
        // state lock while opening: Windows audio device initialization can block
        // or time out, and the shortcut/cancel path must stay able to recover.
        if let Err(e) = self.start_microphone_stream() {
            let msg = format!("{e}");
            error!("Failed to open microphone stream: {msg}");
            return Err(msg);
        }

        if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
            if let Err(e) = rec.start() {
                return Err(format!("Failed to start recorder: {e}"));
            }
        } else {
            return Err("Recorder not available".to_string());
        }

        let mut state = self.state.lock().unwrap();
        if !matches!(*state, RecordingState::Idle) {
            return Err("Already recording".to_string());
        }

        *state = RecordingState::Recording {
            binding_id: binding_id.to_string(),
        };
        debug!("Recording started for binding {binding_id}");
        Ok(())
    }

    pub fn update_selected_device(&self) -> Result<(), anyhow::Error> {
        // Restarting the stream now would throw away a recording in progress;
        // switch devices as soon as it ends instead.
        if self.is_recording() {
            self.reopen_after_recording.store(true, Ordering::Relaxed);
            return Ok(());
        }

        // If currently open, restart the microphone stream to use the new device
        if *self.is_open.lock().unwrap() {
            self.stop_microphone_stream();
            self.start_microphone_stream()?;
        }
        Ok(())
    }

    /// Runs once a recording has ended (stopped or cancelled): restores the
    /// system volume and closes the stream when it should not stay open.
    fn finish_recording(&self, device_failed: bool) {
        self.remove_mute();

        // Close in on-demand mode, after a device failure (the next recording
        // then starts from a fresh stream instead of a dead one), or when the
        // selected microphone changed mid-recording.
        let reopen = self.reopen_after_recording.swap(false, Ordering::Relaxed);
        if device_failed || reopen || matches!(*self.mode.lock().unwrap(), MicrophoneMode::OnDemand)
        {
            self.stop_microphone_stream();
        }
    }

    pub fn stop_recording(&self, binding_id: &str) -> Option<Result<Vec<f32>, RecordingStopError>> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
            } if active == binding_id => {
                *state = RecordingState::Idle;
                drop(state);

                let mut silent_error = None;
                let samples = if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
                    match rec.stop() {
                        Ok(buf) => buf,
                        Err(e) if e.is::<MicrophoneSilent>() => {
                            error!("{e}");
                            silent_error =
                                Some(RecordingStopError::MicrophoneSilent(e.to_string()));
                            Vec::new()
                        }
                        Err(e) => {
                            error!("stop() failed: {e}");
                            Vec::new()
                        }
                    }
                } else {
                    error!("Recorder not available");
                    Vec::new()
                };

                self.finish_recording(silent_error.is_some());

                if let Some(err) = silent_error {
                    return Some(Err(err));
                }

                // Pad if very short
                let s_len = samples.len();
                // debug!("Got {} samples", s_len);
                if s_len < WHISPER_SAMPLE_RATE && s_len > 0 {
                    let mut padded = samples;
                    padded.resize(WHISPER_SAMPLE_RATE * 5 / 4, 0.0);
                    Some(Ok(padded))
                } else {
                    Some(Ok(samples))
                }
            }
            _ => None,
        }
    }
    pub fn is_recording(&self) -> bool {
        matches!(
            *self.state.lock().unwrap(),
            RecordingState::Recording { .. }
        )
    }

    /// Cancel any ongoing recording without returning audio samples
    pub fn cancel_recording(&self) {
        let mut state = self.state.lock().unwrap();

        if let RecordingState::Recording { .. } = *state {
            *state = RecordingState::Idle;
            drop(state);

            if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
                let _ = rec.stop(); // Discard the result
            }

            self.finish_recording(false);
        }
    }
}
