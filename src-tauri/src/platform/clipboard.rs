use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::settings::PasteMethod;

#[cfg(target_os = "linux")]
use crate::utils::is_wayland;

#[cfg(target_os = "linux")]
use std::process::Command;

pub fn write_text(app_handle: &AppHandle, text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if is_wayland() && is_wl_copy_available() {
            log::info!("Using wl-copy for clipboard write on Wayland");
            return write_text_via_wl_copy(text);
        }
    }

    app_handle
        .clipboard()
        .write_text(text)
        .map_err(|e| format!("Failed to write to clipboard: {e}"))
}

pub fn default_paste_method() -> PasteMethod {
    if cfg!(target_os = "linux") {
        PasteMethod::Direct
    } else {
        PasteMethod::CtrlV
    }
}

/// Check if wl-copy is available (Wayland clipboard tool)
#[cfg(target_os = "linux")]
fn is_wl_copy_available() -> bool {
    Command::new("which")
        .arg("wl-copy")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Write text to clipboard via wl-copy (Wayland clipboard tool).
/// Uses Stdio::null() to avoid blocking on repeated calls; wl-copy forks a
/// daemon that inherits piped fds, causing read_to_end to hang indefinitely.
#[cfg(target_os = "linux")]
fn write_text_via_wl_copy(text: &str) -> Result<(), String> {
    use std::process::Stdio;

    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute wl-copy: {e}"))?;

    if !status.success() {
        return Err("wl-copy failed".into());
    }

    Ok(())
}
