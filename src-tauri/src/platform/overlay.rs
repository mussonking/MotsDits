use tauri::webview::WebviewWindow;

pub fn after_show(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    force_topmost(window);
    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

/// Hides the window once the fade-out animation has played, unless
/// `still_wanted` says the overlay was shown again in the meantime.
pub fn hide_after_fade(window: WebviewWindow, still_wanted: impl Fn() -> bool + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(220));
        if !still_wanted() {
            return;
        }
        let window_for_ui = window.clone();
        if let Err(e) = window.run_on_main_thread(move || {
            let _ = window_for_ui.hide();
        }) {
            log::debug!("Failed to schedule recording overlay hide: {e}");
        }
    });
}

#[cfg(target_os = "windows")]
fn force_topmost(window: &WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    let window_for_ui = window.clone();

    let _ = window_for_ui.clone().run_on_main_thread(move || {
        if let Ok(hwnd) = window_for_ui.hwnd() {
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    });
}
