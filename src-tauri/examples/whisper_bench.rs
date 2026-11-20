// Temporary diagnostic: time whisper inference variants on real recordings.
// Usage: whisper_bench <model> <prompt_file> <wav>...
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn read_wav(path: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("wav");
    let spec = r.spec();
    match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|x| x.unwrap()).collect(),
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|x| x.unwrap() as f32 / 32768.0)
            .collect(),
    }
}

#[repr(C)]
struct PowerThrottlingState {
    version: u32,
    control_mask: u32,
    state_mask: u32,
}

extern "system" {
    fn GetCurrentProcess() -> isize;
    fn SetProcessInformation(
        h: isize,
        class: i32,
        info: *const PowerThrottlingState,
        size: u32,
    ) -> i32;
}

fn main() {
    // ECO=1: EcoQoS + ignore timer resolution (what Windows applies to background apps)
    if let Ok(mask) = std::env::var("ECO") {
        let mask: u32 = mask.trim().parse().unwrap();
        let st = PowerThrottlingState {
            version: 1,
            control_mask: mask,
            state_mask: mask,
        };
        let ok = unsafe { SetProcessInformation(GetCurrentProcess(), 4, &st, 12) };
        println!(
            "ECO {mask} applied: {ok} err={}",
            std::io::Error::last_os_error()
        );
    }
    let args: Vec<String> = std::env::args().collect();
    let model = &args[1];
    let prompt = std::fs::read_to_string(&args[2]).unwrap();
    let wavs: Vec<Vec<f32>> = args[3..].iter().map(|p| read_wav(p)).collect();

    let ctx = WhisperContext::new_with_params(
        model,
        WhisperContextParameters {
            use_gpu: true,
            flash_attn: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut st = ctx.create_state().unwrap();

    // (name, app_flags, abort_cb, prompt)
    let variants = [
        ("warmup", false, false, false),
        ("plain", false, false, false),
        ("plain+prompt", false, false, true),
        ("flags+prompt", true, false, true),
        ("abort+prompt", false, true, true),
        ("app-exact", true, true, true),
    ];
    // IDLE=<secs>: leave the GPU idle before each variant, like the app between dictations.
    let idle_secs: u64 = std::env::var("IDLE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    for (name, flags, abort, with_prompt) in variants {
        if idle_secs > 0 && name != "warmup" {
            std::thread::sleep(std::time::Duration::from_secs(idle_secs));
        }
        for (i, audio) in wavs.iter().enumerate() {
            let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            p.set_language(None);
            p.set_n_threads(8);
            p.set_print_progress(false);
            p.set_print_realtime(false);
            p.set_print_special(false);
            p.set_print_timestamps(false);
            if flags {
                p.set_suppress_blank(true);
                p.set_suppress_nst(true);
                p.set_no_speech_thold(0.2);
            }
            if abort {
                let cb: Box<dyn FnMut() -> bool> = Box::new(|| false);
                p.set_abort_callback_safe::<Option<Box<dyn FnMut() -> bool>>, Box<dyn FnMut() -> bool>>(
                    Some(cb),
                );
            }
            if with_prompt {
                p.set_initial_prompt(&prompt);
            }
            let t = Instant::now();
            st.full(p, audio).unwrap();
            let mut text = String::new();
            for s in 0..st.full_n_segments() {
                text.push_str(st.get_segment(s).unwrap().to_str().unwrap());
            }
            println!(
                "RESULT {name:<14} wav{i} {:>8.0?}ms | {}",
                t.elapsed().as_millis(),
                text.trim()
            );
        }
    }
}
