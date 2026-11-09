//! In-process CPU sampling at 100 Hz with our own SIGPROF handler, so each
//! sample can carry the span and labels that were active when it was taken.

mod ring;
mod signal;

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{Frame, Kind, Labels, Profile, Sample, SpanLink};
use crate::symbolize::Symbolizer;

pub(crate) const HZ: u32 = 100;
pub(crate) const PERIOD_NANOS: i64 = 1_000_000_000 / HZ as i64;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    addrs: Box<[usize]>,
    blocked: u8,
    thread: Arc<str>,
    link: Option<SpanLink>,
    labels: u32,
}

/// Owns the process's CPU timer while it lives.
pub(crate) struct CpuSampler {
    counts: HashMap<Key, i64>,
    threads: HashMap<Vec<u8>, Arc<str>>,
}

impl CpuSampler {
    pub(crate) fn start() -> Result<Self, String> {
        signal::start(HZ)?;
        Ok(Self {
            counts: HashMap::new(),
            threads: HashMap::new(),
        })
    }

    /// Moves samples from the ring into this window's counts. Call often
    /// enough that the ring (1024 samples) does not fill.
    pub(crate) fn drain(&mut self) {
        let Self { counts, threads } = self;
        signal::RING.drain(|raw| {
            let thread = threads
                .entry(raw.thread_name().to_vec())
                .or_insert_with_key(|name| Arc::from(String::from_utf8_lossy(name)))
                .clone();
            let key = Key {
                addrs: raw.addrs().into(),
                blocked: raw.blocked,
                thread,
                link: raw.link,
                labels: raw.labels,
            };
            *counts.entry(key).or_default() += 1;
        });
        if threads.len() > 4096 {
            threads.clear();
        }
    }

    /// The window's `cpu` and `samples` profiles; resets the counts.
    pub(crate) fn collect(
        &mut self,
        symbolizer: &mut Symbolizer,
        labels: &dyn Fn(u32) -> Labels,
        time_unix_nano: u64,
        duration_nano: u64,
    ) -> Vec<Profile> {
        self.drain();
        let dropped = signal::RING.take_dropped();
        if dropped > 0 {
            log::warn!("otelprofile: {dropped} CPU samples dropped (sample buffer full)");
        }
        let mut samples = Vec::with_capacity(self.counts.len());
        for (key, count) in self.counts.drain() {
            let stack: Vec<Frame> = if key.blocked > 0 {
                let name = format!("[{}]", signal::blocked_library(key.blocked - 1));
                vec![Frame {
                    function: symbolizer.intern(&name),
                    file: symbolizer.intern(""),
                    line: 0,
                }]
            } else {
                symbolizer.stack(&key.addrs, true)
            };
            let mut attributes: Vec<(Arc<str>, Arc<str>)> = labels(key.labels).to_vec();
            if !key.thread.is_empty() {
                attributes.push((symbolizer.intern("thread.name"), key.thread.clone()));
            }
            samples.push(Sample {
                stack: stack.into(),
                value: count,
                labels: attributes.into(),
                link: key.link,
            });
        }
        if samples.is_empty() {
            return Vec::new();
        }
        let cpu = samples
            .iter()
            .map(|s| Sample {
                value: s.value * PERIOD_NANOS,
                ..s.clone()
            })
            .collect();
        let profile = |kind, samples| Profile {
            kind,
            time_unix_nano,
            duration_nano,
            period: PERIOD_NANOS,
            samples,
        };
        vec![profile(Kind::Cpu, cpu), profile(Kind::Samples, samples)]
    }
}

impl Drop for CpuSampler {
    fn drop(&mut self) {
        signal::stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn thread_cpu() -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid out pointer.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }

    #[inline(never)]
    fn spin_for_the_cpu_test(budget: Duration) -> u64 {
        let started = Instant::now();
        let mut x = 1u64;
        while started.elapsed() < budget {
            for _ in 0..1_000_000 {
                x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
            }
        }
        x
    }

    /// The real sampler end to end: CPU burned on a named thread shows up
    /// under that thread, in the burning function, at about the CPU time
    /// the thread really used.
    #[test]
    fn cpu_burned_on_a_thread_is_attributed_to_its_function_and_time() {
        let _sigprof = crate::testing::SIGPROF
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut sampler = CpuSampler::start().unwrap();
        let used = std::thread::Builder::new()
            .name("cpu-test-burner".into())
            .spawn(|| {
                let before = thread_cpu();
                std::hint::black_box(spin_for_the_cpu_test(Duration::from_millis(1500)));
                thread_cpu() - before
            })
            .unwrap()
            .join()
            .unwrap();
        let started = Instant::now();
        let profiles = sampler.collect(&mut Symbolizer::default(), &|_| Arc::from([]), 0, 0);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "symbolization is too slow"
        );
        let cpu = profiles
            .iter()
            .find(|p| p.kind == Kind::Cpu)
            .expect("a cpu profile");
        let mine: Vec<&Sample> = cpu
            .samples
            .iter()
            .filter(|s| {
                s.labels
                    .iter()
                    .any(|(k, v)| &**k == "thread.name" && &**v == "cpu-test-burner")
            })
            .collect();
        let total: i64 = mine.iter().map(|s| s.value).sum();
        let ratio = total as f64 / used.as_nanos() as f64;
        assert!(
            (0.7..1.3).contains(&ratio),
            "profiled {total} ns vs used {used:?} (ratio {ratio:.2})"
        );
        let in_spin: i64 = mine
            .iter()
            .filter(|s| {
                s.stack
                    .iter()
                    .any(|f| f.function.contains("spin_for_the_cpu_test"))
            })
            .map(|s| s.value)
            .sum();
        // A sample taken in a function prologue loses its caller frame (the
        // unwinder steps from the trampoline past it), so allow some slack.
        assert!(
            in_spin * 10 >= total * 8,
            "only {in_spin} of {total} ns in the spin function"
        );
        let unwound = mine
            .iter()
            .filter(|s| s.stack.len() > 2)
            .map(|s| s.value)
            .sum::<i64>();
        assert!(
            unwound * 10 >= total * 9,
            "only {unwound} of {total} ns have full stacks"
        );
        let leaf_first = mine.iter().all(|s| {
            !s.stack[0].function.contains("thread_start")
                && !s.stack[0].function.contains("on_sigprof")
        });
        assert!(leaf_first, "stacks must start at the interrupted frame");
        drop(sampler);
    }
}
