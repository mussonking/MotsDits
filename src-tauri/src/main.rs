// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;
use motsdits_app_lib::CliArgs;

fn main() {
    let cli_args = CliArgs::parse();

    #[cfg(target_os = "linux")]
    {
        // DMABUF renderer causes crashes on various GPU/display server configurations
        // See: https://github.com/tauri-apps/tauri/issues/9394
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    #[cfg(target_os = "windows")]
    {
        // ggml-vulkan prefers host-visible VRAM (ReBAR) for its buffers. On Windows,
        // WDDM is free to place those allocations in system RAM, and after long
        // uptimes it does: Whisper's compute buffers ended up in shared memory and
        // every GPU op ran ~30x slower (3.6 s instead of 0.13 s per encoder pass).
        // Plain device-local memory stays in VRAM. Must be set before Vulkan init.
        //
        // ggml reads it with the C runtime's getenv(), which keeps its own copy of
        // the environment: std::env::set_var (SetEnvironmentVariableW) is invisible
        // to it. _putenv_s updates the CRT copy (and the process environment).
        if std::env::var_os("GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM").is_none() {
            extern "C" {
                fn _putenv_s(name: *const std::ffi::c_char, value: *const std::ffi::c_char) -> i32;
            }
            // SAFETY: both arguments are valid NUL-terminated strings, and this runs
            // at startup before any other thread exists.
            let rc = unsafe {
                _putenv_s(
                    c"GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM".as_ptr(),
                    c"1".as_ptr(),
                )
            };
            if rc != 0 {
                eprintln!("Failed to set GGML_VK_DISABLE_HOST_VISIBLE_VIDMEM (errno {rc})");
            }
        }
    }

    motsdits_app_lib::run(cli_args)
}
