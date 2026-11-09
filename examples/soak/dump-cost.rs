//! Raw synchronous jemalloc prof.dump latency, not parser or export latency.
use std::ffi::CString;
use std::hint::black_box;
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    // SAFETY: prof.active is a writable boolean mallctl.
    unsafe { tikv_jemalloc_ctl::raw::write(b"prof.active\0", true).unwrap() };
    let held: Vec<_> = (0..128).map(|_| vec![1u8; 512 * 1024]).collect();
    black_box(&held);
    let start = Instant::now();
    let mut samples = Vec::new();
    for index in 1..=5 {
        let next = Duration::from_secs(index * 15);
        while start.elapsed() < next {
            black_box(vec![2u8; black_box(16 * 1024)]);
        }
        let file = std::env::temp_dir().join(format!(
            "otelprofile-dump-{}-{index}.heap",
            std::process::id()
        ));
        assert!(!file.exists(), "refuse to overwrite a dump");
        let path = CString::new(file.to_str().unwrap()).unwrap();
        let before = Instant::now();
        // SAFETY: prof.dump expects a pointer to a nul-terminated path,
        // which remains alive through the synchronous mallctl.
        unsafe { tikv_jemalloc_ctl::raw::write(b"prof.dump\0", path.as_ptr()).unwrap() };
        let elapsed = before.elapsed().as_secs_f64();
        let bytes = std::fs::metadata(&file).unwrap().len();
        std::fs::remove_file(file).unwrap();
        samples
            .push(serde_json::json!({"at_s": index * 15, "dump_s": elapsed, "dump_bytes": bytes}));
    }
    println!(
        "{}",
        serde_json::json!({"held_bytes": 64 * 1024 * 1024, "dumps": samples})
    );
}
