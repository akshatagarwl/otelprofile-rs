//! `start`: the sampler thread (drains the CPU ring, builds a batch per
//! interval) and the exporter thread (posts batches), joined on drop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::config::{Env, export_settings, parse_pairs, process_env};
use crate::export::Exporter;
use crate::model::{AttributeValue, Origin, Profile};
use crate::symbolize::Symbolizer;
use crate::{Config, Error, ProfileType};

/// How often the sampler moves CPU samples out of the signal ring.
const DRAIN_EVERY: Duration = Duration::from_millis(100);
/// One profiler per process: the CPU timer and signal ring are global.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// How long dropping a `Profiler` waits for the final upload.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Settings after merging `Config` with the environment.
pub(crate) struct Settings {
    pub types: Vec<ProfileType>,
    pub interval: Duration,
    pub origin: Origin,
}

pub(crate) fn settings(
    config: &Config,
    env: Env<'_>,
) -> Result<Option<(Settings, crate::export::ExportSettings)>, Error> {
    if env("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        return Ok(None);
    }
    let export = export_settings(&config.endpoint, &config.headers, env)?;
    let types = if !config.profile_types.is_empty() {
        config.profile_types.clone()
    } else if let Some(raw) = env("OTEL_RUST_X_PROFILES_TYPES") {
        parse_types(&raw)?
    } else {
        vec![ProfileType::Cpu, ProfileType::Heap]
    };
    let interval = if !config.interval.is_zero() {
        config.interval
    } else if let Some(raw) = env("OTEL_RUST_X_PROFILES_INTERVAL") {
        parse_duration(&raw)?
    } else {
        Duration::from_secs(15)
    };
    let origin = Origin {
        resource: resource(config, env)?,
        scope_name: "otelprofile".into(),
        scope_version: env!("CARGO_PKG_VERSION").into(),
    };
    Ok(Some((
        Settings {
            types,
            interval,
            origin,
        },
        export,
    )))
}

fn parse_types(raw: &str) -> Result<Vec<ProfileType>, Error> {
    let mut types = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match name.to_ascii_lowercase().as_str() {
            "all" => return Ok(vec![ProfileType::Cpu, ProfileType::Heap]),
            "cpu" => types.push(ProfileType::Cpu),
            "heap" | "alloc_objects" | "alloc_space" | "inuse_objects" | "inuse_space" => {
                if !types.contains(&ProfileType::Heap) {
                    types.push(ProfileType::Heap);
                }
            }
            other => {
                return Err(Error::config(format!(
                    "unknown profile type {other:?} (cpu, heap or all)"
                )));
            }
        }
    }
    Ok(types)
}

/// `15s`, `500ms`, `2m` or bare seconds.
fn parse_duration(raw: &str) -> Result<Duration, Error> {
    let raw = raw.trim();
    let (number, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: u64 = number
        .parse()
        .map_err(|_| Error::config(format!("invalid duration {raw:?}")))?;
    let d = match unit {
        "" | "s" => Duration::from_secs(n),
        "ms" => Duration::from_millis(n),
        "m" => Duration::from_secs(n * 60),
        _ => return Err(Error::config(format!("invalid duration {raw:?}"))),
    };
    if d < Duration::from_millis(100) {
        return Err(Error::config(format!("interval {raw:?} is below 100ms")));
    }
    Ok(d)
}

/// Defaults, then `OTEL_RESOURCE_ATTRIBUTES`, then `Config::tags`, then the
/// service name (`Config::service_name`, else `OTEL_SERVICE_NAME`), each
/// overriding the one before.
fn resource(config: &Config, env: Env<'_>) -> Result<Vec<(String, AttributeValue)>, Error> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".into());
    let mut attributes: Vec<(String, AttributeValue)> = vec![
        (
            "service.name".into(),
            format!("unknown_service:{exe}").into(),
        ),
        ("service.instance.id".into(), instance_id().into()),
        ("host.name".into(), hostname().into()),
        ("host.arch".into(), arch().into()),
        ("os.type".into(), os_type().into()),
        ("process.pid".into(), i64::from(std::process::id()).into()),
        ("process.executable.name".into(), exe.into()),
        ("telemetry.sdk.name".into(), "otelprofile".into()),
        ("telemetry.sdk.language".into(), "rust".into()),
        (
            "telemetry.sdk.version".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ];
    let mut set = |key: String, value: AttributeValue| {
        attributes.retain(|(k, _)| *k != key);
        attributes.push((key, value));
    };
    if let Some(raw) = env("OTEL_RESOURCE_ATTRIBUTES") {
        for (key, value) in parse_pairs(&raw, "resource attribute")? {
            set(key, value.into());
        }
    }
    for (key, value) in &config.tags {
        set(key.clone(), value.clone());
    }
    let service = Some(config.service_name.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| env("OTEL_SERVICE_NAME"));
    if let Some(service) = service {
        set("service.name".into(), service.into());
    }
    Ok(attributes)
}

fn instance_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut halves = [0u64; 2];
    for (i, half) in halves.iter_mut().enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.write_u32(std::process::id());
        hasher.write_usize(i);
        *half = hasher.finish();
    }
    format!("{:016x}{:016x}", halves[0], halves[1])
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: valid buffer and length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn os_type() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// A running profiler. Dropping it (or calling [`Profiler::stop`]) stops
/// sampling and sends the last window, waiting at most five seconds.
#[must_use = "dropping the Profiler stops profiling"]
pub struct Profiler {
    owns_process: bool,
    stop: Option<mpsc::Sender<()>>,
    sampler: Option<JoinHandle<()>>,
    exported: Option<mpsc::Receiver<()>>,
}

impl Profiler {
    pub(crate) fn disabled() -> Self {
        Self {
            owns_process: false,
            stop: None,
            sampler: None,
            exported: None,
        }
    }

    /// Stops profiling and flushes the last window.
    pub fn stop(self) {}
}

impl Drop for Profiler {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(sampler) = self.sampler.take() {
            let _ = sampler.join();
        }
        if let Some(exported) = self.exported.take() {
            let _ = exported.recv_timeout(STOP_WAIT);
        }
        if self.owns_process {
            RUNNING.store(false, Ordering::Release);
        }
    }
}

/// The sources that produce profiles each window.
struct Sources {
    cpu: Option<crate::cpu::CpuSampler>,
    heap: Option<Heap>,
}

enum Heap {
    Sampled(crate::heap::sampled::SampledHeap),
    #[cfg(feature = "jemalloc")]
    Jemalloc(crate::heap::jemalloc::JemallocHeap),
}

impl Sources {
    fn start(types: &[ProfileType]) -> Self {
        let cpu = if types.contains(&ProfileType::Cpu) {
            crate::cpu::CpuSampler::start()
                .map_err(|e| log::error!("otelprofile: CPU profiling is off: {e}"))
                .ok()
        } else {
            None
        };
        let heap = if types.contains(&ProfileType::Heap) {
            heap_backend()
        } else {
            None
        };
        Self { cpu, heap }
    }

    fn drain(&mut self) {
        if let Some(cpu) = &mut self.cpu {
            cpu.drain();
        }
    }

    fn collect(
        &mut self,
        symbolizer: &mut Symbolizer,
        start: SystemTime,
        now: SystemTime,
    ) -> Vec<Profile> {
        let start_nanos = unix_nanos(start);
        let duration = now.duration_since(start).unwrap_or_default().as_nanos() as u64;
        let mut profiles = Vec::new();
        if let Some(cpu) = &mut self.cpu {
            profiles.extend(cpu.collect(
                symbolizer,
                &crate::context::labels,
                start_nanos,
                duration,
            ));
        }
        let heap: Result<Vec<Profile>, String> = match &mut self.heap {
            #[cfg(feature = "jemalloc")]
            Some(Heap::Jemalloc(heap)) => heap.collect(symbolizer, start_nanos, unix_nanos(now)),
            Some(Heap::Sampled(heap)) => Ok(heap.collect(symbolizer, start_nanos, unix_nanos(now))),
            None => Ok(Vec::new()),
        };
        match heap {
            Ok(heap) => profiles.extend(heap),
            Err(error) => log::warn!("otelprofile: heap profile failed: {error}"),
        }
        profiles
    }
}

/// The heap backend this process can use, if any.
fn heap_backend() -> Option<Heap> {
    if let Some(rate) = crate::alloc::installed_rate() {
        return Some(Heap::Sampled(crate::heap::sampled::SampledHeap::new(rate)));
    }
    #[cfg(feature = "jemalloc")]
    if crate::heap::jemalloc::active() {
        return Some(Heap::Jemalloc(Default::default()));
    }
    log::info!(
        "otelprofile: heap profiling is off: install otelprofile::alloc::Sampled as the global allocator, \
         or use tikv-jemallocator with the `jemalloc` feature and jemalloc_profiling!()"
    );
    None
}

fn unix_nanos(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

pub(crate) fn start(config: Config) -> Result<Profiler, Error> {
    start_with_env(config, &process_env)
}

pub(crate) fn start_with_env(config: Config, env: Env<'_>) -> Result<Profiler, Error> {
    start_with_symbolizer(config, env, Symbolizer::default())
}

fn start_with_symbolizer(
    config: Config,
    env: Env<'_>,
    symbolizer: Symbolizer,
) -> Result<Profiler, Error> {
    let Some((settings, export)) = settings(&config, env)? else {
        return Ok(Profiler::disabled());
    };
    #[cfg(feature = "opentelemetry")]
    crate::span::otel::install();
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err(Error::Start(
            "a profiler is already running in this process".into(),
        ));
    }
    let mut profiler = Profiler::disabled();
    profiler.owns_process = true;
    let (batches, incoming) = mpsc::sync_channel::<Vec<Profile>>(2);
    let (exported_tx, exported) = mpsc::channel();
    let exporter = Exporter::new(export);
    log::info!(
        "otelprofile: sending {:?} profiles to {} every {:?}",
        settings.types,
        exporter.url(),
        settings.interval
    );
    let origin = settings.origin.clone();
    std::thread::Builder::new()
        .name("otelprofile-export".into())
        .spawn(move || {
            let mut reporter = Reporter::default();
            for batch in incoming {
                for error in exporter.export(&origin, &batch) {
                    reporter.report(&error.to_string());
                }
            }
            let _ = exported_tx.send(());
        })
        .map_err(|e| Error::Start(e.to_string()))?;
    profiler.exported = Some(exported);
    let (stop, stopped) = mpsc::channel::<()>();
    let types = settings.types.clone();
    let interval = settings.interval;
    let sampler = std::thread::Builder::new()
        .name("otelprofile-sampler".into())
        .spawn(move || run(&types, interval, &stopped, &batches, symbolizer))
        .map_err(|e| Error::Start(e.to_string()))?;
    profiler.stop = Some(stop);
    profiler.sampler = Some(sampler);
    Ok(profiler)
}

fn run(
    types: &[ProfileType],
    interval: Duration,
    stopped: &mpsc::Receiver<()>,
    batches: &SyncSender<Vec<Profile>>,
    mut symbolizer: Symbolizer,
) {
    let mut sources = Sources::start(types);
    let mut window_start = SystemTime::now();
    let mut next = Instant::now() + interval;
    loop {
        let last = !matches!(
            stopped.recv_timeout(DRAIN_EVERY),
            Err(RecvTimeoutError::Timeout)
        );
        sources.drain();
        if !last && Instant::now() < next {
            continue;
        }
        let now = SystemTime::now();
        let profiles = sources.collect(&mut symbolizer, window_start, now);
        window_start = now;
        next += interval;
        if !profiles.is_empty() {
            match batches.try_send(profiles) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    log::warn!("otelprofile: dropped a window; the exporter is behind")
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        if last {
            return;
        }
    }
}

/// Logs export errors, collapsing repeats of the same message within a
/// minute so an unreachable endpoint does not flood the log.
#[derive(Default)]
struct Reporter {
    last: Option<(String, Instant)>,
    repeats: u32,
}

impl Reporter {
    fn report(&mut self, message: &str) {
        if let Some((last, at)) = &self.last
            && last == message
            && at.elapsed() < Duration::from_secs(60)
        {
            self.repeats += 1;
            return;
        }
        if self.repeats > 0 {
            log::warn!(
                "otelprofile: previous export error repeated {} more times",
                self.repeats
            );
        }
        log::warn!("otelprofile: export failed: {message}");
        self.last = Some((message.to_string(), Instant::now()));
        self.repeats = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MockReceiver, SIGPROF, decode};

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn resource_attributes_layer_defaults_env_tags_and_service_name() {
        let env = env_of(&[
            (
                "OTEL_RESOURCE_ATTRIBUTES",
                "deployment.environment=prod,service.name=from-attrs,team=a",
            ),
            ("OTEL_SERVICE_NAME", "from-env"),
        ]);
        let config = Config {
            tags: vec![("team".into(), "b".into())],
            ..Config::default()
        };
        let (settings, _) = settings(&config, &env).unwrap().unwrap();
        let get = |key: &str| {
            settings
                .origin
                .resource
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("service.name"), Some("from-env".into()));
        assert_eq!(get("team"), Some("b".into()));
        assert_eq!(get("deployment.environment"), Some("prod".into()));
        assert_eq!(get("telemetry.sdk.language"), Some("rust".into()));
        assert_eq!(
            get("process.pid"),
            Some(AttributeValue::Int(i64::from(std::process::id())))
        );
        let named = Config {
            service_name: "explicit".into(),
            ..Config::default()
        };
        let (settings, _) = super::settings(&named, &env).unwrap().unwrap();
        assert!(
            settings
                .origin
                .resource
                .contains(&("service.name".into(), "explicit".into()))
        );
    }

    #[test]
    fn sdk_disabled_turns_start_into_a_no_op() {
        let env = env_of(&[("OTEL_SDK_DISABLED", "true")]);
        assert!(settings(&Config::default(), &env).unwrap().is_none());
    }

    #[test]
    fn types_and_interval_come_from_the_environment() {
        let env = env_of(&[
            ("OTEL_RUST_X_PROFILES_TYPES", "cpu"),
            ("OTEL_RUST_X_PROFILES_INTERVAL", "500ms"),
        ]);
        let (settings, _) = settings(&Config::default(), &env).unwrap().unwrap();
        assert_eq!(settings.types, [ProfileType::Cpu]);
        assert_eq!(settings.interval, Duration::from_millis(500));
        let bad = env_of(&[("OTEL_RUST_X_PROFILES_TYPES", "goroutine")]);
        assert!(super::settings(&Config::default(), &bad).is_err());
    }

    #[inline(never)]
    fn busy_in_the_end_to_end_test(budget: Duration) {
        std::hint::black_box(crate::testing::burn(budget));
    }

    fn wait_for_cpu_timer() {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: zero is a valid itimerval and getitimer receives a valid output pointer.
            let mut timer: libc::itimerval = unsafe { std::mem::zeroed() };
            // SAFETY: timer is a valid writable itimerval.
            assert_eq!(unsafe { libc::getitimer(libc::ITIMER_PROF, &mut timer) }, 0);
            if timer.it_interval.tv_sec != 0 || timer.it_interval.tv_usec != 0 {
                return;
            }
            assert!(Instant::now() < deadline, "CPU timer did not start");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[inline(never)]
    fn sample_in_fresh_instance(link: crate::SpanLink) {
        let _span = crate::span::enter(link);
        // SAFETY: the profiler installed its SIGPROF handler and is still active.
        assert_eq!(unsafe { libc::raise(libc::SIGPROF) }, 0);
    }

    #[test]
    fn final_symbolization_samples_cannot_leak_into_a_restarted_profiler() {
        let _sigprof = SIGPROF.lock().unwrap_or_else(|e| e.into_inner());
        let old = crate::SpanLink {
            trace_id: [0x11; 16],
            span_id: [0x22; 8],
        };
        let fresh = crate::SpanLink {
            trace_id: [0x33; 16],
            span_id: [0x44; 8],
        };
        let first_receiver = MockReceiver::start(vec![]);
        let config = |receiver: &MockReceiver, service: &str| Config {
            service_name: service.into(),
            endpoint: receiver.url.clone(),
            profile_types: vec![ProfileType::Cpu],
            interval: Duration::from_secs(3600),
            ..Default::default()
        };
        let mut symbolizer = Symbolizer::default();
        let (resolved_tx, resolved_rx) = mpsc::channel();
        symbolizer.before_next_resolve(move || {
            let _span = crate::span::enter(old);
            for _ in 0..3 {
                // SAFETY: invoked inside final symbolization before the first CPU sampler stops.
                assert_eq!(unsafe { libc::raise(libc::SIGPROF) }, 0);
            }
            resolved_tx.send(()).unwrap();
        });
        let first = start_with_symbolizer(
            config(&first_receiver, "first-session"),
            &|_| None,
            symbolizer,
        )
        .unwrap();
        wait_for_cpu_timer();
        sample_in_fresh_instance(fresh);
        first.stop();
        resolved_rx
            .try_recv()
            .expect("final symbolization hook did not run");

        let queued = crate::cpu::test_support::queued_samples();
        let stale: Vec<_> = queued
            .iter()
            .filter(|sample| sample.link == Some(old))
            .collect();
        assert!(
            stale.len() >= 3,
            "final symbolization did not enqueue real signal samples"
        );
        assert!(
            stale
                .iter()
                .all(|sample| sample.thread_name() == b"otelprofile-sampler")
        );
        let mut inspect = Symbolizer::default();
        let interrupted: Vec<_> = stale
            .iter()
            .flat_map(|sample| inspect.stack(sample.addrs(), true))
            .map(|frame| frame.function)
            .collect();
        assert!(
            interrupted
                .iter()
                .any(|name| name.contains("symbolize::Symbolizer") && name.ends_with("::address")),
            "queued samples did not interrupt address symbolization: {interrupted:?}"
        );

        let second_receiver = MockReceiver::start(vec![]);
        let second = start_with_env(config(&second_receiver, "second-session"), &|_| None).unwrap();
        wait_for_cpu_timer();
        std::thread::Builder::new()
            .name("fresh-instance".into())
            .spawn(move || sample_in_fresh_instance(fresh))
            .unwrap()
            .join()
            .unwrap();
        second.stop();
        let samples: Vec<_> = second_receiver
            .requests()
            .iter()
            .flat_map(|request| decode(&request.body).profiles)
            .filter(|profile| profile.sample_type.0 == "cpu")
            .flat_map(|profile| profile.samples)
            .collect();
        let old_link = format!("{}/{}", "11".repeat(16), "22".repeat(8));
        let fresh_link = format!("{}/{}", "33".repeat(16), "44".repeat(8));
        assert!(
            samples
                .iter()
                .any(|sample| sample.link.as_deref() == Some(&fresh_link)
                    && sample
                        .attributes
                        .iter()
                        .any(|(key, value)| key == "thread.name" && value == "fresh-instance")
                    && sample
                        .frames
                        .iter()
                        .any(|frame| frame.contains("sample_in_fresh_instance"))),
            "fresh thread attribution missing: {samples:?}"
        );
        let leaked = samples
            .iter()
            .filter(|sample| sample.link.as_deref() == Some(&old_link))
            .count();
        eprintln!(
            "lifecycle: {} queued signals during final symbolization; {leaked} stale encoded samples in second instance",
            stale.len()
        );
        assert_eq!(
            leaked, 0,
            "first profiler's final-symbolization samples leaked into second profiler: {samples:?}"
        );
    }

    #[cfg(feature = "jemalloc")]
    #[test]
    fn start_sends_jemalloc_heap_profiles() {
        let _sigprof = SIGPROF.lock().unwrap_or_else(|e| e.into_inner());
        let receiver = MockReceiver::start(vec![]);
        let config = Config {
            endpoint: receiver.url.clone(),
            profile_types: vec![ProfileType::Heap],
            interval: Duration::from_millis(300),
            ..Config::default()
        };
        let profiler = start_with_env(config, &|_| None).unwrap();
        let kept: Vec<Vec<u8>> = (0..256).map(|_| vec![0u8; 64 * 1024]).collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while receiver.requests().is_empty() {
            assert!(
                Instant::now() < deadline,
                "no initial heap snapshot received"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Allocation profiles are deltas; work before the first snapshot is only a baseline.
        let allocated: Vec<Vec<u8>> = (0..256).map(|_| vec![1u8; 64 * 1024]).collect();
        std::hint::black_box(&allocated);
        drop(profiler);
        drop((kept, allocated));
        let types: std::collections::BTreeSet<String> = receiver
            .requests()
            .iter()
            .flat_map(|r| decode(&r.body).profiles)
            .map(|p| p.sample_type.0)
            .collect();
        for expected in [
            "inuse_objects",
            "inuse_space",
            "alloc_objects",
            "alloc_space",
        ] {
            assert!(types.contains(expected), "no {expected} in {types:?}");
        }
    }

    #[test]
    fn start_sends_cpu_profiles_until_dropped_and_flushes_the_last_window() {
        let _sigprof = SIGPROF.lock().unwrap_or_else(|e| e.into_inner());
        let receiver = MockReceiver::start(vec![]);
        let config = Config {
            service_name: "e2e".into(),
            endpoint: receiver.url.clone(),
            profile_types: vec![ProfileType::Cpu],
            interval: Duration::from_millis(400),
            ..Config::default()
        };
        let profiler = start_with_env(config.clone(), &|_| None).unwrap();
        assert!(
            matches!(start_with_env(config, &|_| None), Err(Error::Start(_))),
            "one per process"
        );
        busy_in_the_end_to_end_test(Duration::from_millis(1000));
        drop(profiler);
        let requests: Vec<_> = receiver
            .requests()
            .iter()
            .map(|r| decode(&r.body))
            .collect();
        assert!(
            requests.len() >= 2,
            "expected a request per window, got {}",
            requests.len()
        );
        assert!(
            requests[0]
                .resource
                .contains(&("service.name".into(), "e2e".into()))
        );
        let busy: i64 = requests
            .iter()
            .flat_map(|r| &r.profiles)
            .filter(|p| p.sample_type.0 == "cpu")
            .flat_map(|p| &p.samples)
            .filter(|s| {
                s.frames
                    .iter()
                    .any(|f| f.contains("busy_in_the_end_to_end_test"))
            })
            .map(|s| s.value)
            .sum();
        assert!(
            busy >= 600_000_000,
            "only {busy} ns of the busy loop were reported"
        );
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "dropping releases the process slot"
        );
    }
}
