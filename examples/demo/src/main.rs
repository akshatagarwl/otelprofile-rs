//! A fixed workload to profile: CPU-heavy hashing and parsing plus steady
//! allocation. Configure the destination with the `OTEL_*` variables.

use std::hint::black_box;
use std::time::{Duration, Instant};

fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let _profiler = otelprofile::start(otelprofile::Config {
        service_name: "otelprofile-demo".into(),
        ..Default::default()
    })
    .expect("start profiling");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut kept = Vec::new();
    while Instant::now() < deadline {
        black_box(hash(400_000));
        black_box(parse(2_000));
        kept.push(allocate(64 * 1024));
        if kept.len() > 64 {
            kept.clear();
        }
    }
}

#[inline(never)]
fn hash(n: u64) -> u64 {
    (0..n).fold(1u64, |x, i| {
        x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i)
    })
}

#[inline(never)]
fn parse(lines: usize) -> usize {
    (0..lines)
        .map(|i| format!("{i},{},{}", i * 7, i % 13))
        .map(|line| {
            line.split(',')
                .filter_map(|f| f.parse::<u64>().ok())
                .count()
        })
        .sum()
}

#[inline(never)]
fn allocate(bytes: usize) -> Vec<u8> {
    vec![1u8; bytes]
}
