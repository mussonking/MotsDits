use crate::audio_toolkit::{list_input_devices, AudioRecorder};
use crate::helpers::clamshell;
use crate::settings::AppSettings;
use cpal::traits::DeviceTrait;
use serde::{Deserialize, Serialize};
use specta::Type;

#[cfg(target_os = "windows")]
use winreg::{
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    RegKey, HKEY,
};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAccess {
    Allowed,
    Denied,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct WindowsMicrophonePermissionStatus {
    pub supported: bool,
    pub overall_access: PermissionAccess,
    pub device_access: PermissionAccess,
    pub app_access: PermissionAccess,
    pub desktop_app_access: PermissionAccess,
}

#[cfg(target_os = "windows")]
fn read_registry_permission_access(root_hkey: HKEY, path: &str) -> PermissionAccess {
    let root = RegKey::predef(root_hkey);
    let Ok(key) = root.open_subkey(path) else {
        return PermissionAccess::Unknown;
    };

    let Ok(value) = key.get_value::<String, _>("Value") else {
        return PermissionAccess::Unknown;
    };

    match value.to_ascii_lowercase().as_str() {
        "allow" => PermissionAccess::Allowed,
        "deny" => PermissionAccess::Denied,
        _ => PermissionAccess::Unknown,
    }
}

#[cfg(target_os = "windows")]
fn microphone_permission_status_impl() -> WindowsMicrophonePermissionStatus {
    const MICROPHONE_PATH: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone";
    const DESKTOP_APPS_PATH: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged";

    let device_access = read_registry_permission_access(HKEY_LOCAL_MACHINE, MICROPHONE_PATH);
    let app_access = read_registry_permission_access(HKEY_CURRENT_USER, MICROPHONE_PATH);
    let desktop_app_access = read_registry_permission_access(HKEY_CURRENT_USER, DESKTOP_APPS_PATH);

    let overall_access = if [device_access, app_access, desktop_app_access]
        .into_iter()
        .any(|access| access == PermissionAccess::Denied)
    {
        PermissionAccess::Denied
    } else if [device_access, app_access, desktop_app_access]
        .into_iter()
        .all(|access| access == PermissionAccess::Allowed)
    {
        PermissionAccess::Allowed
    } else {
        PermissionAccess::Unknown
    };

    WindowsMicrophonePermissionStatus {
        supported: true,
        overall_access,
        device_access,
        app_access,
        desktop_app_access,
    }
}

pub fn microphone_permission_status() -> WindowsMicrophonePermissionStatus {
    #[cfg(target_os = "windows")]
    {
        microphone_permission_status_impl()
    }

    #[cfg(not(target_os = "windows"))]
    {
        WindowsMicrophonePermissionStatus {
            supported: false,
            overall_access: PermissionAccess::Unknown,
            device_access: PermissionAccess::Unknown,
            app_access: PermissionAccess::Unknown,
            desktop_app_access: PermissionAccess::Unknown,
        }
    }
}

pub fn open_microphone_privacy_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", "ms-settings:privacy-microphone"])
            .spawn()
            .map_err(|e| format!("Failed to open Windows microphone privacy settings: {}", e))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Opening microphone privacy settings is only supported on Windows".to_string())
    }
}

pub fn resolve_input_device(settings: &AppSettings) -> Option<cpal::Device> {
    let use_clamshell_mic = clamshell::is_clamshell()
        .map(|is_clamshell| is_clamshell && settings.clamshell_microphone.is_some())
        .unwrap_or(false);

    let device_name = if use_clamshell_mic {
        settings.clamshell_microphone.as_ref()?
    } else {
        settings.selected_microphone.as_ref()?
    };

    #[cfg(target_os = "windows")]
    {
        resolve_input_device_with_timeout(device_name)
    }

    #[cfg(not(target_os = "windows"))]
    {
        resolve_input_device_by_name(device_name)
    }
}

pub fn uses_recording_vad() -> bool {
    !cfg!(target_os = "windows")
}

#[cfg(target_os = "windows")]
fn resolve_input_device_with_timeout(device_name: &str) -> Option<cpal::Device> {
    use std::sync::mpsc;
    use std::time::Duration;

    let device_name = device_name.to_string();
    let device_name_for_thread = device_name.clone();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let device = resolve_input_device_by_name(&device_name_for_thread);
        let _ = tx.send(device);
    });

    match rx.recv_timeout(Duration::from_millis(1500)) {
        Ok(device) => device,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            log::warn!(
                "Timed out resolving selected microphone '{device_name}'. Using system default microphone."
            );
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            log::warn!(
                "Failed to resolve selected microphone '{device_name}'. Using system default microphone."
            );
            None
        }
    }
}

fn resolve_input_device_by_name(device_name: &str) -> Option<cpal::Device> {
    match list_input_devices() {
        Ok(devices) => devices
            .into_iter()
            .find(|d| d.name == device_name)
            .map(|d| d.device),
        Err(e) => {
            log::debug!("Failed to list input devices, using default microphone: {e}");
            None
        }
    }
}

/// Volume control of the default output device.
#[cfg(target_os = "windows")]
fn default_output_volume() -> Option<windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume>
{
    use windows::Win32::{
        Media::Audio::{
            eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        },
        System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let all_devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let default_device = all_devices
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()?;
        default_device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .ok()
    }
}

/// Whether the system output is currently muted. `None` when it cannot be read.
pub fn is_output_muted() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        let volume = default_output_volume()?;
        unsafe { volume.GetMute().ok().map(|muted| muted.as_bool()) }
    }

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("osascript")
            .args(["-e", "output muted of (get volume settings)"])
            .output()
            .ok()?;
        match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

pub fn set_output_muted(mute: bool) {
    // Expected behavior:
    // - Windows: standard endpoint volume API.
    // - Linux: PipeWire/PulseAudio/ALSA command fallbacks.
    // - macOS: AppleScript system volume mute.
    // Unsupported environments fail silently.

    #[cfg(target_os = "windows")]
    {
        if let Some(volume) = default_output_volume() {
            let _ = unsafe { volume.SetMute(mute, std::ptr::null()) };
        }
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        let mute_val = if mute { "1" } else { "0" };
        let amixer_state = if mute { "mute" } else { "unmute" };

        if Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        if Command::new("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        let _ = Command::new("amixer")
            .args(["set", "Master", amixer_state])
            .output();
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        let script = format!(
            "set volume output muted {}",
            if mute { "true" } else { "false" }
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

pub fn open_recorder(
    recorder: &mut AudioRecorder,
    selected_device: Option<cpal::Device>,
) -> Result<(), anyhow::Error> {
    let has_explicit_device = selected_device.is_some();
    let selected_device_name = selected_device
        .as_ref()
        .and_then(|device| device.name().ok())
        .unwrap_or_else(|| "system default".to_string());

    log::debug!("Opening microphone stream for device: {selected_device_name}");

    #[cfg(target_os = "windows")]
    {
        match recorder.open(selected_device) {
            Ok(()) => Ok(()),
            Err(e) if has_explicit_device => {
                log::warn!(
                    "Failed to open selected microphone '{selected_device_name}': {e}. Retrying system default microphone."
                );
                recorder.open(None).map_err(|fallback_err| {
                    anyhow::anyhow!(
                        "Failed to open recorder with selected microphone ({e}) and default microphone ({fallback_err})"
                    )
                })
            }
            Err(e) => Err(anyhow::anyhow!("Failed to open recorder: {e}")),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = has_explicit_device;
        recorder
            .open(selected_device)
            .map_err(|e| anyhow::anyhow!("Failed to open recorder: {e}"))
    }
}
