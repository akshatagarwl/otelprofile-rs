use std::collections::VecDeque;
use std::hint::black_box;
use std::time::{Duration, Instant};

use otelprofile::ProfileType;
use pyroscope::backend::{BackendConfig, PprofConfig, jemalloc_backend, pprof_backend};
use pyroscope::pyroscope::PyroscopeAgentBuilder;

#[cfg(all(feature = "jemalloc", not(feature = "sampled")))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
#[cfg(all(feature = "jemalloc", not(feature = "sampled")))]
otelprofile::jemalloc_profiling!();

#[cfg(feature = "sampled")]
#[global_allocator]
static ALLOC: otelprofile::alloc::Sampled<std::alloc::System> =
    otelprofile::alloc::Sampled::new(std::alloc::System);

#[derive(Clone, Copy)]
enum Mode {
    Off,
    OtelCpu,
    OtelAll,
    PyroscopeCpu,
    PyroscopeAll,
}

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "otel-cpu" => Self::OtelCpu,
            "otel-all" => Self::OtelAll,
            "pyroscope-cpu" => Self::PyroscopeCpu,
            "pyroscope-all" => Self::PyroscopeAll,
            _ => panic!("expected off, otel-cpu, otel-all, pyroscope-cpu or pyroscope-all"),
        }
    }
}

#[inline(never)]
fn checkout_work(order: u64, kept: &mut VecDeque<Vec<u8>>) -> u64 {
    let mut x = order;
    for i in 0..20_000 {
        x = black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i));
    }
    let receipt = format!("order {order} costs {x} cents ")
        .repeat(256)
        .into_bytes();
    black_box(&receipt);
    kept.push_back(receipt);
    if kept.len() > 512 {
        kept.pop_front();
    }
    x
}

fn usage() -> libc::rusage {
    let mut usage = std::mem::MaybeUninit::uninit();
    // SAFETY: getrusage initializes the provided rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // SAFETY: getrusage succeeded.
    unsafe { usage.assume_init() }
}

fn cpu(usage: &libc::rusage) -> f64 {
    let user = usage.ru_utime;
    let system = usage.ru_stime;
    (user.tv_sec + system.tv_sec) as f64 + (user.tv_usec + system.tv_usec) as f64 / 1e6
}

fn main() {
    env_logger::init();
    let args: Vec<_> = std::env::args().collect();
    let mode_name = args.get(1).expect("mode");
    let mode = Mode::parse(mode_name);
    let duration = Duration::from_secs(args.get(2).expect("seconds").parse().expect("seconds"));
    assert!(
        duration.as_secs() >= 20,
        "use at least 20s so an export window completes"
    );
    let otlp = std::env::var("SOAK_OTLP").expect("SOAK_OTLP receiver");
    let pyro = std::env::var("SOAK_PYROSCOPE").expect("SOAK_PYROSCOPE receiver");
    let service = format!("rust-soak-{}", std::process::id());
    let interval = Duration::from_secs(15);

    #[cfg(all(feature = "jemalloc", not(feature = "sampled")))]
    if matches!(mode, Mode::Off | Mode::OtelCpu | Mode::PyroscopeCpu) {
        // SAFETY: prof.active is a writable boolean mallctl.
        unsafe { tikv_jemalloc_ctl::raw::write(b"prof.active\0", false).expect("prof.active") };
    }

    let profiler = match mode {
        Mode::OtelCpu | Mode::OtelAll => Some(
            otelprofile::start(otelprofile::Config {
                service_name: service.clone(),
                endpoint: otlp,
                interval,
                profile_types: if matches!(mode, Mode::OtelAll) {
                    vec![ProfileType::Cpu, ProfileType::Heap]
                } else {
                    vec![ProfileType::Cpu]
                },
                ..Default::default()
            })
            .expect("otelprofile start"),
        ),
        _ => None,
    };
    let mut agents = Vec::new();
    if matches!(mode, Mode::PyroscopeCpu | Mode::PyroscopeAll) {
        agents.push(
            PyroscopeAgentBuilder::new(
                &pyro,
                &service,
                100,
                "pyroscope-rs",
                "2.1.5",
                pprof_backend(PprofConfig { sample_rate: 100 }, BackendConfig::default()),
            )
            .upload_interval(interval)
            .build()
            .expect("pyroscope CPU build")
            .start()
            .expect("pyroscope CPU start"),
        );
    }
    #[cfg(not(feature = "jemalloc"))]
    if matches!(mode, Mode::PyroscopeAll) {
        panic!("pyroscope heap needs jemalloc");
    }
    if matches!(mode, Mode::PyroscopeAll) {
        agents.push(
            PyroscopeAgentBuilder::new(
                &pyro,
                &service,
                100,
                "pyroscope-rs",
                "2.1.5",
                jemalloc_backend(),
            )
            .upload_interval(interval)
            .build()
            .expect("pyroscope heap build")
            .start()
            .expect("pyroscope heap start"),
        );
    }

    let before = usage();
    let start = Instant::now();
    let mut ops = 0u64;
    let mut checksum = 0u64;
    let mut kept = VecDeque::new();
    while start.elapsed() < duration {
        for _ in 0..64 {
            ops += 1;
            checksum = checksum.wrapping_add(checkout_work(ops, &mut kept));
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let after = usage();
    black_box((&kept, checksum));
    let stop = Instant::now();
    drop(profiler);
    std::thread::scope(|scope| {
        for agent in agents {
            scope.spawn(move || agent.stop().expect("pyroscope stop").shutdown());
        }
    });
    let stop_s = stop.elapsed().as_secs_f64();
    let rss = after.ru_maxrss as f64
        / if cfg!(target_os = "macos") {
            1024.0 * 1024.0
        } else {
            1024.0
        };
    println!(
        "{}",
        serde_json::json!({
            "mode": mode_name, "service": service, "wall_s": wall,
            "cpu_s": cpu(&after) - cpu(&before), "ops": ops,
            "ops_per_s": ops as f64 / wall, "checksum": checksum,
            "peak_rss_mib": rss, "shutdown_s": stop_s,
            "allocator": if cfg!(feature = "sampled") { "sampled-system" } else if cfg!(feature = "jemalloc") { "jemalloc" } else { "system" },
        })
    );
}
