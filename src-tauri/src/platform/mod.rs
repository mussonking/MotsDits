//! Platform boundary for OS-specific integration code.
//!
//! Keep transcription, settings, history, and custom-word logic outside this
//! module. Put OS windowing, audio-device quirks, shortcuts, paste, and tray
//! behavior behind small functions here so Linux and Windows can evolve without
//! surprising each other.

pub mod app;
pub mod audio;
pub mod clipboard;
pub mod gpu_diag;
pub mod onnx_runtime;
pub mod overlay;
pub mod shortcuts;
pub mod transcription;
pub mod tray;
pub mod webview_profile;
