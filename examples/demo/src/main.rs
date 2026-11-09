use std::hint::black_box;
use std::time::{Duration, Instant};

fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut kept = Vec::new();
    while Instant::now() < deadline {
        black_box(burn(200_000));
        kept.push(allocate(64 * 1024));
        if kept.len() > 64 {
            kept.clear();
        }
    }
}

fn burn(n: u64) -> u64 {
    (0..n).fold(1u64, |x, i| {
        x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i)
    })
}

fn allocate(bytes: usize) -> Vec<u8> {
    vec![1u8; bytes]
}
