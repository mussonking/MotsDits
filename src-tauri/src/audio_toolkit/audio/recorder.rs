use std::{
    io::Error,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Sample, SizedSample,
};

use crate::audio_toolkit::{
    audio::{AudioVisualiser, FrameResampler},
    constants,
    vad::{self, VadFrame},
    VoiceActivityDetector,
};

enum Cmd {
    Start,
    Stop(mpsc::Sender<StopReply>),
    Shutdown,
}

struct StopReply {
    samples: Vec<f32>,
    received_audio: bool,
}

/// How long a recording may run without a single audio callback before the
/// microphone is considered silent (stream open, but the driver delivers nothing).
const SILENT_MICROPHONE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Returned by [`AudioRecorder::stop`] when the device never delivered audio
/// during the recording. Seen on Windows when a USB microphone's driver stalls:
/// the stream opens normally but no callback ever fires.
#[derive(Debug)]
pub struct MicrophoneSilent {
    pub device: Option<String>,
}

impl std::fmt::Display for MicrophoneSilent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.device {
            Some(name) => write!(f, "Microphone '{name}' delivered no audio"),
            None => write!(f, "Microphone delivered no audio"),
        }
    }
}

impl std::error::Error for MicrophoneSilent {}

enum AudioChunk {
    Samples(Vec<f32>),
    EndOfStream,
}

/// Called with `true` when the microphone is found silent, and with `false`
/// if audio then starts flowing during the same recording.
type SilentCallback = Arc<dyn Fn(bool) + Send + Sync + 'static>;

pub struct AudioRecorder {
    device: Option<Device>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    silent_cb: Option<SilentCallback>,
    /// Set by the worker once a recording ran without any audio callback.
    /// A stalled driver can also block the stream teardown for many seconds.
    stalled: Arc<AtomicBool>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioRecorder {
            device: None,
            cmd_tx: None,
            worker_handle: None,
            vad: None,
            level_cb: None,
            silent_cb: None,
            stalled: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_vad(mut self, vad: Box<dyn VoiceActivityDetector>) -> Self {
        self.vad = Some(Arc::new(Mutex::new(vad)));
        self
    }

    pub fn with_level_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        self.level_cb = Some(Arc::new(cb));
        self
    }

    /// Called with `true` when the microphone delivers no audio within
    /// [`SILENT_MICROPHONE_TIMEOUT`] of the recording start, then with `false`
    /// if a slow device starts delivering audio later in that recording.
    pub fn with_silent_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        self.silent_cb = Some(Arc::new(cb));
        self
    }

    pub fn open(&mut self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker_handle.is_some() {
            return Ok(()); // already open
        }

        let (sample_tx, sample_rx) = mpsc::channel::<AudioChunk>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let host = crate::audio_toolkit::get_cpal_host();
        let device = match device {
            Some(dev) => dev,
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };

        let thread_device = device.clone();
        let vad = self.vad.clone();
        // Move the optional level callback into the worker thread
        let level_cb = self.level_cb.clone();
        let silent_cb = self.silent_cb.clone();
        self.stalled.store(false, Ordering::Relaxed);
        let stalled = self.stalled.clone();

        let worker = std::thread::spawn(move || {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_flag_for_stream = stop_flag.clone();
            let init_result = (|| -> Result<(cpal::Stream, u32), String> {
                let config = AudioRecorder::get_preferred_config(&thread_device)
                    .map_err(|e| format!("Failed to fetch preferred config: {e}"))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as usize;

                log::info!(
                    "Using device: {:?}\nSample rate: {}\nChannels: {}\nFormat: {:?}",
                    thread_device.name(),
                    sample_rate,
                    channels,
                    config.sample_format()
                );

                let stream = match config.sample_format() {
                    cpal::SampleFormat::U8 => AudioRecorder::build_stream::<u8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I8 => AudioRecorder::build_stream::<i8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I16 => AudioRecorder::build_stream::<i16>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I32 => AudioRecorder::build_stream::<i32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::F32 => AudioRecorder::build_stream::<f32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    sample_format => {
                        return Err(format!("Unsupported sample format: {sample_format:?}"));
                    }
                };

                stream
                    .play()
                    .map_err(|e| format!("Failed to start microphone stream: {e}"))?;

                Ok((stream, sample_rate))
            })();

            match init_result {
                Ok((stream, sample_rate)) => {
                    let _ = init_tx.send(Ok(()));
                    // Keep the stream alive while we process samples.
                    run_consumer(
                        sample_rate,
                        vad,
                        sample_rx,
                        cmd_rx,
                        level_cb,
                        silent_cb,
                        stalled,
                        stop_flag,
                    );
                    drop(stream);
                }
                Err(error_message) => {
                    log::error!("{error_message}");
                    let _ = init_tx.send(Err(error_message));
                }
            }
        });

        match init_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(Ok(())) => {
                self.device = Some(device);
                self.cmd_tx = Some(cmd_tx);
                self.worker_handle = Some(worker);
                Ok(())
            }
            Ok(Err(error_message)) => {
                let _ = worker.join();
                let kind = if is_microphone_access_denied(&error_message) {
                    std::io::ErrorKind::PermissionDenied
                } else {
                    std::io::ErrorKind::Other
                };
                Err(Box::new(Error::new(kind, error_message)))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = cmd_tx.send(Cmd::Shutdown);
                Err(Box::new(Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Timed out initializing microphone input. Check the selected microphone device.",
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                Err(Box::new(Error::other(
                    "Failed to initialize microphone worker",
                )))
            }
        }
    }

    /// The command channel of the open stream, or an error when it is closed.
    fn open_stream(&self) -> Result<&mpsc::Sender<Cmd>, Box<dyn std::error::Error>> {
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| Error::other("Microphone stream is not open").into())
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.open_stream()?.send(Cmd::Start)?;
        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let (resp_tx, resp_rx) = mpsc::channel();
        // Without an open stream nobody would ever answer: waiting on the
        // reply would block the whole transcription pipeline forever.
        self.open_stream()?.send(Cmd::Stop(resp_tx))?;
        let reply = resp_rx.recv()?; // wait for the samples
        if !reply.received_audio {
            return Err(Box::new(MicrophoneSilent {
                device: self.device.as_ref().and_then(|d| d.name().ok()),
            }));
        }
        Ok(reply.samples)
    }

    pub fn close(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(h) = self.worker_handle.take() {
            if self.stalled.load(Ordering::Relaxed) {
                // A stalled driver can block the stream teardown for 10-20s.
                // Let it finish in the background instead of freezing the caller.
                std::thread::spawn(move || {
                    let _ = h.join();
                });
            } else {
                let _ = h.join();
            }
        }
        self.device = None;
        Ok(())
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        sample_tx: mpsc::Sender<AudioChunk>,
        channels: usize,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: Sample + SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let mut output_buffer = Vec::new();
        let mut eos_sent = false;

        let stream_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
            if stop_flag.load(Ordering::Relaxed) {
                if !eos_sent {
                    let _ = sample_tx.send(AudioChunk::EndOfStream);
                    eos_sent = true;
                }
                return;
            }
            eos_sent = false;

            output_buffer.clear();

            if channels == 1 {
                output_buffer.extend(data.iter().map(|&sample| sample.to_sample::<f32>()));
            } else {
                let frame_count = data.len() / channels;
                output_buffer.reserve(frame_count);

                for frame in data.chunks_exact(channels) {
                    let mono_sample = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    output_buffer.push(mono_sample);
                }
            }

            if sample_tx
                .send(AudioChunk::Samples(output_buffer.clone()))
                .is_err()
            {
                log::error!("Failed to send samples");
            }
        };

        device.build_input_stream(
            &config.clone().into(),
            stream_cb,
            |err| log::error!("Stream error: {}", err),
            None,
        )
    }

    fn get_preferred_config(
        device: &cpal::Device,
    ) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
        let supported_configs = device.supported_input_configs()?;
        let mut best_config: Option<cpal::SupportedStreamConfigRange> = None;

        // Try to find a config that supports 16kHz, prioritizing better formats
        for config_range in supported_configs {
            if config_range.min_sample_rate().0 <= constants::WHISPER_SAMPLE_RATE
                && config_range.max_sample_rate().0 >= constants::WHISPER_SAMPLE_RATE
            {
                match best_config {
                    None => best_config = Some(config_range),
                    Some(ref current) => {
                        // Prioritize F32 > I16 > I32 > others
                        let score = |fmt: cpal::SampleFormat| match fmt {
                            cpal::SampleFormat::F32 => 4,
                            cpal::SampleFormat::I16 => 3,
                            cpal::SampleFormat::I32 => 2,
                            _ => 1,
                        };

                        if score(config_range.sample_format()) > score(current.sample_format()) {
                            best_config = Some(config_range);
                        }
                    }
                }
            }
        }

        if let Some(config) = best_config {
            return Ok(config.with_sample_rate(cpal::SampleRate(constants::WHISPER_SAMPLE_RATE)));
        }

        // If no config supports 16kHz, fall back to default
        Ok(device.default_input_config()?)
    }
}

pub fn is_microphone_access_denied(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("access is denied")
        || normalized.contains("permission denied")
        || normalized.contains("0x80070005")
}

#[cfg(test)]
mod tests {
    use super::is_microphone_access_denied;

    #[test]
    fn detects_access_is_denied() {
        assert!(is_microphone_access_denied("Access is denied"));
    }

    #[test]
    fn detects_permission_denied() {
        assert!(is_microphone_access_denied("permission denied"));
    }

    #[test]
    fn detects_windows_error_code() {
        assert!(is_microphone_access_denied("WASAPI error: 0x80070005"));
    }

    #[test]
    fn does_not_match_unrelated_errors() {
        assert!(!is_microphone_access_denied("device not found"));
    }
}

fn run_consumer(
    in_sample_rate: u32,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    sample_rx: mpsc::Receiver<AudioChunk>,
    cmd_rx: mpsc::Receiver<Cmd>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    silent_cb: Option<SilentCallback>,
    stalled: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
) {
    let mut frame_resampler = FrameResampler::new(
        in_sample_rate as usize,
        constants::WHISPER_SAMPLE_RATE as usize,
        Duration::from_millis(30),
    );

    let mut processed_samples = Vec::<f32>::new();
    let mut recording = false;
    let mut recording_started_at = Instant::now();
    let mut received_audio = false;
    let mut silence_reported = false;

    // ---------- spectrum visualisation setup ---------------------------- //
    const BUCKETS: usize = 16;
    const WINDOW_SIZE: usize = 512;
    let mut visualizer = AudioVisualiser::new(
        in_sample_rate,
        WINDOW_SIZE,
        BUCKETS,
        400.0,  // vocal_min_hz
        4000.0, // vocal_max_hz
    );

    fn handle_frame(
        samples: &[f32],
        recording: bool,
        vad: &Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
        out_buf: &mut Vec<f32>,
    ) {
        if !recording {
            return;
        }

        if let Some(vad_arc) = vad {
            let mut det = vad_arc.lock().unwrap();
            match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
                VadFrame::Speech(buf) => out_buf.extend_from_slice(buf),
                VadFrame::Noise => {}
            }
        } else {
            out_buf.extend_from_slice(samples);
        }
    }

    loop {
        // Commands must be handled even when the audio callback is silent. On
        // Windows, some devices can stop delivering samples while still leaving
        // the stream alive, so blocking forever on sample_rx would make Stop
        // and Shutdown appear ignored.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Start => {
                    stop_flag.store(false, Ordering::Relaxed);
                    processed_samples.clear();
                    recording = true;
                    recording_started_at = Instant::now();
                    received_audio = false;
                    silence_reported = false;
                    visualizer.reset();
                    if let Some(v) = &vad {
                        v.lock().unwrap().reset();
                    }
                }
                Cmd::Stop(reply_tx) => {
                    recording = false;
                    stop_flag.store(true, Ordering::Relaxed);

                    // Drain all remaining audio until the producer confirms end-of-stream.
                    // The cpal callback sees the stop flag, sends EndOfStream, and goes
                    // silent — guaranteeing every captured sample is in the channel
                    // ahead of the sentinel.
                    loop {
                        match sample_rx.recv_timeout(Duration::from_millis(500)) {
                            Ok(AudioChunk::Samples(remaining)) => {
                                received_audio = true;
                                frame_resampler.push(&remaining, &mut |frame: &[f32]| {
                                    handle_frame(frame, true, &vad, &mut processed_samples)
                                });
                            }
                            Ok(AudioChunk::EndOfStream) => {
                                // The callback fired, so the device is alive even
                                // if the recording was too short to carry samples.
                                received_audio = true;
                                break;
                            }
                            Err(_) => {
                                log::warn!("Timed out waiting for EndOfStream from audio callback");
                                break;
                            }
                        }
                    }

                    frame_resampler.finish(&mut |frame: &[f32]| {
                        handle_frame(frame, true, &vad, &mut processed_samples)
                    });

                    if !received_audio {
                        stalled.store(true, Ordering::Relaxed);
                    }
                    let _ = reply_tx.send(StopReply {
                        samples: std::mem::take(&mut processed_samples),
                        received_audio,
                    });

                    // Resume the audio callback so the consumer loop can continue
                    // receiving chunks (important for always-on microphone mode).
                    stop_flag.store(false, Ordering::Relaxed);
                }
                Cmd::Shutdown => {
                    stop_flag.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }

        if recording
            && !received_audio
            && !silence_reported
            && recording_started_at.elapsed() >= SILENT_MICROPHONE_TIMEOUT
        {
            silence_reported = true;
            stalled.store(true, Ordering::Relaxed);
            log::warn!(
                "Microphone delivered no audio {:?} after recording start",
                SILENT_MICROPHONE_TIMEOUT
            );
            if let Some(cb) = &silent_cb {
                cb(true);
            }
        }

        let chunk = match sample_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let raw = match chunk {
            AudioChunk::Samples(s) => {
                if recording && !received_audio {
                    received_audio = true;
                    stalled.store(false, Ordering::Relaxed);
                    // A slow device (Bluetooth, USB hub waking up) finally
                    // delivers audio: take the "silent" warning back down.
                    if silence_reported {
                        if let Some(cb) = &silent_cb {
                            cb(false);
                        }
                    }
                }
                s
            }
            AudioChunk::EndOfStream => continue,
        };

        // ---------- spectrum processing ---------------------------------- //
        // Levels only feed the recording overlay; an always-on microphone
        // would otherwise run an FFT and emit events around the clock.
        if recording {
            if let Some(buckets) = visualizer.feed(&raw) {
                if let Some(cb) = &level_cb {
                    cb(buckets);
                }
            }
        }

        // ---------- existing pipeline ------------------------------------ //
        frame_resampler.push(&raw, &mut |frame: &[f32]| {
            handle_frame(frame, recording, &vad, &mut processed_samples)
        });
    }
}
