//! GPU memory diagnostics for the log file.
//!
//! When GPU inference silently degrades (VRAM exhausted and spilling into
//! shared system memory, or the wrong adapter being picked), transcription
//! gets many times slower with no error. These helpers record enough GPU state
//! in the log to tell those cases apart after the fact.

/// One-line, human-readable snapshot of every hardware GPU adapter: total
/// dedicated memory, the budget the OS currently grants this process, and how
/// much this process uses in dedicated vs shared memory.
///
/// A small budget, or a large "shared" figure, means the GPU is saturated and
/// inference is spilling into system RAM.
#[cfg(target_os = "windows")]
pub fn gpu_memory_summary() -> String {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        DXGI_MEMORY_SEGMENT_GROUP, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
        DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
    };

    const MB: u64 = 1024 * 1024;

    fn query(
        adapter: &IDXGIAdapter3,
        group: DXGI_MEMORY_SEGMENT_GROUP,
    ) -> Option<DXGI_QUERY_VIDEO_MEMORY_INFO> {
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: `info` is a valid, writable struct for the duration of the call.
        unsafe { adapter.QueryVideoMemoryInfo(0, group, &mut info) }
            .ok()
            .map(|()| info)
    }

    // SAFETY: plain DXGI factory creation; no preconditions.
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(factory) => factory,
        Err(e) => return format!("GPU memory unavailable (DXGI factory: {e})"),
    };

    let mut parts = Vec::new();
    let mut index = 0;
    // SAFETY: EnumAdapters1 returns an error once `index` is past the last adapter.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;

        // SAFETY: `adapter` is a live COM interface.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
            continue;
        }

        let name_len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..name_len]);
        let dedicated_mb = desc.DedicatedVideoMemory as u64 / MB;

        let memory = adapter.cast::<IDXGIAdapter3>().ok().and_then(|adapter3| {
            let local = query(&adapter3, DXGI_MEMORY_SEGMENT_GROUP_LOCAL)?;
            let shared = query(&adapter3, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL)?;
            Some((local, shared))
        });

        let part = match memory {
            Some((local, shared)) => format!(
                "{name}: {dedicated_mb} MB dedicated, app budget {} MB, app uses {} MB dedicated + {} MB shared",
                local.Budget / MB,
                local.CurrentUsage / MB,
                shared.CurrentUsage / MB
            ),
            None => format!("{name}: {dedicated_mb} MB dedicated, usage unavailable"),
        };
        // Virtual display adapters (Parsec, DisplayLink) re-expose the physical
        // GPU they render on, so the same card can be enumerated several times.
        if !parts.contains(&part) {
            parts.push(part);
        }
    }

    if parts.is_empty() {
        "no hardware GPU adapter found".to_string()
    } else {
        parts.join(" | ")
    }
}

#[cfg(not(target_os = "windows"))]
pub fn gpu_memory_summary() -> String {
    "GPU memory snapshot not available on this platform".to_string()
}

#[cfg(test)]
mod tests {
    use super::gpu_memory_summary;

    #[test]
    fn summary_is_never_empty() {
        let summary = gpu_memory_summary();
        println!("{summary}");
        assert!(!summary.is_empty());
    }
}
