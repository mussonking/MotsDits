use std::cell::Cell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use log::{debug, info};
use transcribe_rs::{
    get_whisper_accelerator, get_whisper_gpu_device, whisper_cpp::WhisperInferenceParams,
    TranscribeError, TranscriptionResult, TranscriptionSegment,
};
use whisper_rs::{
    whisper_rs_sys::{whisper_context, whisper_state},
    FullParams, SamplingStrategy, SegmentCallbackData, WhisperContext, WhisperContextParameters,
    WhisperState,
};

// Start of the current `full()` call. whisper.cpp runs the encoder and segment
// callbacks on the calling thread, so a thread-local is enough to time phases.
thread_local! {
    static FULL_STARTED: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn ms_since_full_start() -> u128 {
    FULL_STARTED.with(|s| s.get().map_or(0, |t| t.elapsed().as_millis()))
}

/// Logs when each 30 s window enters the encoder. Time before the first call is
/// the mel spectrogram (CPU); time between this and the first segment is the
/// encoder plus language detection and decoding (GPU).
unsafe extern "C" fn log_encoder_start(
    _ctx: *mut whisper_context,
    _state: *mut whisper_state,
    _user_data: *mut c_void,
) -> bool {
    debug!(
        "Whisper phase: encoder start at {}ms",
        ms_since_full_start()
    );
    true
}

pub struct CancellableWhisperEngine {
    state: WhisperState,
    #[allow(dead_code)]
    context: WhisperContext,
}

fn compiled_whisper_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "metal"
    } else if cfg!(target_os = "windows") || cfg!(target_os = "linux") {
        "vulkan"
    } else {
        "cpu"
    }
}

impl CancellableWhisperEngine {
    pub fn load(model_path: &Path) -> Result<Self, TranscribeError> {
        if !model_path.exists() {
            return Err(TranscribeError::ModelNotFound(model_path.to_path_buf()));
        }

        let whisper_accelerator = get_whisper_accelerator();
        let use_gpu = whisper_accelerator.use_gpu();
        let mut context_params = WhisperContextParameters {
            use_gpu,
            flash_attn: true,
            ..Default::default()
        };

        let gpu_device = get_whisper_gpu_device();
        if gpu_device >= 0 {
            context_params.gpu_device = gpu_device;
        }

        info!(
            "Loading Whisper model with accelerator={}, compiled_backend={}, use_gpu={}, gpu_device={}",
            whisper_accelerator,
            compiled_whisper_backend(),
            use_gpu,
            if gpu_device >= 0 {
                gpu_device.to_string()
            } else {
                "auto".to_string()
            }
        );

        // Route whisper.cpp/ggml output (GPU device selection, backend, memory)
        // into the app log so a silent GPU -> CPU fallback is visible.
        whisper_rs::install_logging_hooks();

        let context = WhisperContext::new_with_params(model_path, context_params)
            .map_err(|e| TranscribeError::Inference(e.to_string()))?;
        let state = context
            .create_state()
            .map_err(|e| TranscribeError::Inference(e.to_string()))?;

        Ok(Self { state, context })
    }

    pub fn transcribe_with(
        &mut self,
        samples: &[f32],
        params: &WhisperInferenceParams,
        cancel_requested: Arc<AtomicBool>,
    ) -> Result<TranscriptionResult, TranscribeError> {
        let mut full_params = FullParams::new(sampling_strategy());
        full_params.set_language(params.language.as_deref());
        full_params.set_translate(params.translate);
        full_params.set_print_special(params.print_special);
        full_params.set_print_progress(params.print_progress);
        full_params.set_print_realtime(params.print_realtime);
        full_params.set_print_timestamps(params.print_timestamps);
        full_params.set_suppress_blank(params.suppress_blank);
        full_params.set_suppress_nst(params.suppress_non_speech_tokens);
        full_params.set_no_speech_thold(params.no_speech_thold);
        let abort_cb: Box<dyn FnMut() -> bool> =
            Box::new(move || cancel_requested.load(Ordering::Relaxed));
        full_params
            .set_abort_callback_safe::<Option<Box<dyn FnMut() -> bool>>, Box<dyn FnMut() -> bool>>(
                Some(abort_cb),
            );

        if params.n_threads > 0 {
            full_params.set_n_threads(params.n_threads);
        }

        if let Some(ref prompt) = params.initial_prompt {
            full_params.set_initial_prompt(prompt);
        }

        // SAFETY: the callback only logs; it never touches the context or state.
        unsafe { full_params.set_start_encoder_callback(Some(log_encoder_start)) };
        full_params.set_segment_callback_safe(|seg: SegmentCallbackData| {
            debug!(
                "Whisper phase: segment {} decoded at {}ms",
                seg.segment,
                ms_since_full_start()
            );
        });

        FULL_STARTED.with(|s| s.set(Some(Instant::now())));
        let full_result = self.state.full(full_params, samples);
        debug!(
            "Whisper phase: full() returned at {}ms",
            ms_since_full_start()
        );
        FULL_STARTED.with(|s| s.set(None));
        full_result.map_err(|e| TranscribeError::Inference(e.to_string()))?;

        let num_segments = self.state.full_n_segments();
        let mut segments = Vec::new();
        let mut full_text = String::new();

        for i in 0..num_segments {
            let segment = self
                .state
                .get_segment(i)
                .ok_or_else(|| TranscribeError::Inference(format!("segment {i} out of bounds")))?;
            let text = segment
                .to_str()
                .map_err(|e| TranscribeError::Inference(e.to_string()))?;

            segments.push(TranscriptionSegment {
                start: segment.start_timestamp() as f32 / 100.0,
                end: segment.end_timestamp() as f32 / 100.0,
                text: text.to_string(),
            });
            full_text.push_str(text);
        }

        Ok(TranscriptionResult {
            text: full_text.trim().to_string(),
            segments: Some(segments),
        })
    }
}

fn sampling_strategy() -> SamplingStrategy {
    if cfg!(target_os = "windows") {
        SamplingStrategy::Greedy { best_of: 1 }
    } else {
        SamplingStrategy::BeamSearch {
            beam_size: 3,
            patience: -1.0,
        }
    }
}
