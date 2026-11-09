//! Heap profiles from jemalloc's sampler (`tikv-jemallocator` built with the
//! `profiling` feature and started with `prof:true`).
//!
//! `prof.dump` writes a `heap_v2` text file; each stack has
//! `t*: curobjs: curbytes [accumobjs: accumbytes]`. The bracket is filled
//! only with `prof_accum:true`; we send its growth per window as `alloc_*`.
//! jemalloc keeps no per-allocation user data, so heap samples carry no
//! span links.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Arc;

use super::{HeapRow, trim_allocator_frames, unbias};
use crate::model::Profile;
use crate::symbolize::Symbolizer;

/// One stack of a parsed dump. Addresses are return addresses, leaf first.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DumpStack {
    pub addrs: Vec<usize>,
    pub cur_objects: u64,
    pub cur_bytes: u64,
    pub accum_objects: u64,
    pub accum_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dump {
    /// Mean sampling interval in bytes (`2^lg_prof_sample`).
    pub rate: u64,
    pub stacks: Vec<DumpStack>,
}

/// Parses a jemalloc `heap_v2` dump.
pub(crate) fn parse(text: &str) -> Result<Dump, String> {
    let mut lines = text.lines();
    let header = lines.next().ok_or("empty heap dump")?;
    let rate = header
        .strip_prefix("heap_v2/")
        .and_then(|r| r.trim().parse().ok())
        .ok_or_else(|| format!("not a heap_v2 dump: {header:?}"))?;
    let mut stacks = Vec::new();
    let mut current: Option<DumpStack> = None;
    for line in lines {
        let line = line.trim();
        if line == "MAPPED_LIBRARIES:" {
            break;
        }
        if let Some(addrs) = line.strip_prefix('@') {
            stacks.extend(current.take());
            let addrs = addrs
                .split_whitespace()
                .map(|a| usize::from_str_radix(a.trim_start_matches("0x"), 16))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("bad address in {line:?}: {e}"))?;
            current = Some(DumpStack {
                addrs,
                cur_objects: 0,
                cur_bytes: 0,
                accum_objects: 0,
                accum_bytes: 0,
            });
        } else if let (Some(stack), Some(counts)) = (current.as_mut(), line.strip_prefix("t*:")) {
            let numbers: Vec<u64> = counts
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .map(|s| s.parse().unwrap_or(0))
                .collect();
            if let [cur_objects, cur_bytes, accum_objects, accum_bytes] = numbers[..] {
                *stack = DumpStack {
                    cur_objects,
                    cur_bytes,
                    accum_objects,
                    accum_bytes,
                    ..std::mem::take(stack)
                };
            } else {
                return Err(format!("bad counts line {line:?}"));
            }
        }
    }
    stacks.extend(current);
    Ok(Dump { rate, stacks })
}

/// Whether the process runs with jemalloc's heap sampler on.
pub(crate) fn active() -> bool {
    // SAFETY: both names are boolean mallctls in jemalloc 5.
    unsafe {
        tikv_jemalloc_ctl::raw::read::<bool>(b"opt.prof\0").unwrap_or(false)
            && tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0").unwrap_or(false)
    }
}

/// Asks jemalloc to dump its heap profile and returns the text.
pub(crate) fn dump() -> Result<String, String> {
    let file = DumpFile::create()?;
    let path = CString::new(file.path.clone()).map_err(|e| e.to_string())?;
    // SAFETY: prof.dump takes a `const char *` path that stays valid for the
    // call.
    unsafe { tikv_jemalloc_ctl::raw::write(b"prof.dump\0", path.as_ptr()) }
        .map_err(|e| format!("prof.dump: {e}"))?;
    file.read()
}

/// Where jemalloc writes the dump: an in-memory file on Linux, a temporary
/// file elsewhere. Removed on drop.
struct DumpFile {
    path: String,
    #[cfg(target_os = "linux")]
    fd: libc::c_int,
}

impl DumpFile {
    #[cfg(target_os = "linux")]
    fn create() -> Result<Self, String> {
        // SAFETY: valid NUL-terminated name.
        let fd = unsafe { libc::memfd_create(c"otelprofile-heap".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
        }
        Ok(Self {
            path: format!("/proc/self/fd/{fd}"),
            fd,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn create() -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "otelprofile-heap-{}-{}.prof",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
        })
    }

    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path).map_err(|e| format!("read heap dump: {e}"))
    }
}

impl Drop for DumpFile {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        // SAFETY: we own the descriptor.
        unsafe {
            libc::close(self.fd);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Turns successive dumps into heap profiles, keeping the previous dump's
/// cumulative counts to send `alloc_*` as deltas.
#[derive(Default)]
pub(crate) struct JemallocHeap {
    previous: HashMap<Vec<usize>, (u64, u64)>,
    primed: bool,
}

impl JemallocHeap {
    pub(crate) fn collect(
        &mut self,
        symbolizer: &mut Symbolizer,
        start: u64,
        now: u64,
    ) -> Result<Vec<Profile>, String> {
        let dump = parse(&dump()?)?;
        Ok(self.profiles(&dump, symbolizer, start, now))
    }

    pub(crate) fn profiles(
        &mut self,
        dump: &Dump,
        symbolizer: &mut Symbolizer,
        start: u64,
        now: u64,
    ) -> Vec<Profile> {
        let rate = dump.rate as f64;
        let accumulating = dump.stacks.iter().any(|s| s.accum_objects > 0);
        let mut current = HashMap::with_capacity(dump.stacks.len());
        let mut merged: HashMap<Vec<crate::model::Frame>, [f64; 4]> = HashMap::new();
        for stack in &dump.stacks {
            let (before_objects, before_bytes) =
                self.previous.get(&stack.addrs).copied().unwrap_or((0, 0));
            current.insert(
                stack.addrs.clone(),
                (stack.accum_objects, stack.accum_bytes),
            );
            let (inuse_objects, inuse_bytes) =
                unbias(stack.cur_objects as f64, stack.cur_bytes as f64, rate);
            let (alloc_objects, alloc_bytes) = if self.primed {
                unbias(
                    stack.accum_objects.saturating_sub(before_objects) as f64,
                    stack.accum_bytes.saturating_sub(before_bytes) as f64,
                    rate,
                )
            } else {
                (0.0, 0.0)
            };
            if inuse_bytes <= 0.0 && alloc_bytes <= 0.0 {
                continue;
            }
            let mut frames = symbolizer.stack(&stack.addrs, false);
            trim_allocator_frames(&mut frames);
            if frames.is_empty() {
                continue;
            }
            let totals = merged.entry(frames).or_default();
            totals[0] += inuse_objects;
            totals[1] += inuse_bytes;
            totals[2] += alloc_objects;
            totals[3] += alloc_bytes;
        }
        self.previous = current;
        let alloc = accumulating && self.primed;
        self.primed = true;
        let rows: Vec<HeapRow> = merged
            .into_iter()
            .map(
                |(frames, [inuse_objects, inuse_bytes, alloc_objects, alloc_bytes])| HeapRow {
                    frames: Arc::from(frames),
                    link: None,
                    inuse_objects,
                    inuse_bytes,
                    alloc_objects,
                    alloc_bytes,
                },
            )
            .collect();
        super::profiles(&rows, dump.rate as i64, start, now, alloc)
    }
}

/// The value of jemalloc's `malloc_conf` symbol, read once at startup.
#[repr(transparent)]
pub struct MallocConf(*const std::ffi::c_char);

// SAFETY: points at a static, immutable, NUL-terminated string.
unsafe impl Sync for MallocConf {}

impl MallocConf {
    #[doc(hidden)]
    pub const fn new(conf: &'static std::ffi::CStr) -> Self {
        MallocConf(conf.as_ptr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "heap_v2/524288
  t*: 3: 4096 [10: 20480]
  t0: 3: 4096 [10: 20480]
@ 0x10a 0x20b 0x30c
  t*: 2: 1024 [8: 4096]
  t0: 2: 1024 [8: 4096]
@ 0x40d
  t*: 1: 3072 [2: 16384]

MAPPED_LIBRARIES:
55d0-55e0 r-xp 00000000 08:01 1 /app
";

    #[test]
    fn a_heap_v2_dump_parses_into_stacks_with_current_and_accumulated_counts() {
        let dump = parse(DUMP).unwrap();
        assert_eq!(dump.rate, 524_288);
        assert_eq!(
            dump.stacks,
            [
                DumpStack {
                    addrs: vec![0x10a, 0x20b, 0x30c],
                    cur_objects: 2,
                    cur_bytes: 1024,
                    accum_objects: 8,
                    accum_bytes: 4096
                },
                DumpStack {
                    addrs: vec![0x40d],
                    cur_objects: 1,
                    cur_bytes: 3072,
                    accum_objects: 2,
                    accum_bytes: 16384
                },
            ]
        );
        assert!(parse("heap_v1/1\n").is_err());
    }

    #[test]
    fn alloc_profiles_are_the_growth_since_the_previous_dump() {
        let mut heap = JemallocHeap::default();
        let mut symbolizer = Symbolizer::default();
        let first = parse(DUMP).unwrap();
        let kinds = |profiles: &[Profile]| {
            profiles
                .iter()
                .map(|p| p.kind.spec().sample.0)
                .collect::<Vec<_>>()
        };
        let profiles = heap.profiles(&first, &mut symbolizer, 0, 10);
        assert_eq!(
            kinds(&profiles),
            ["inuse_objects", "inuse_space"],
            "no delta on the first dump"
        );

        let mut second = first.clone();
        second.stacks[0].accum_objects += 4;
        second.stacks[0].accum_bytes += 2048;
        let profiles = heap.profiles(&second, &mut symbolizer, 10, 20);
        assert_eq!(
            kinds(&profiles),
            [
                "inuse_objects",
                "inuse_space",
                "alloc_objects",
                "alloc_space"
            ]
        );
        let alloc_space = profiles
            .iter()
            .find(|p| p.kind.spec().sample.0 == "alloc_space")
            .unwrap();
        let expected = unbias(4.0, 2048.0, 524_288.0).1.round() as i64;
        assert_eq!(
            alloc_space
                .samples
                .iter()
                .map(|s| s.value)
                .collect::<Vec<_>>(),
            [expected]
        );
        assert_eq!(
            (alloc_space.time_unix_nano, alloc_space.duration_nano),
            (10, 10)
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[inline(never)]
    fn retain_for_the_jemalloc_test(chunks: usize, size: usize) -> Vec<Vec<u8>> {
        (0..chunks).map(|i| vec![i as u8; size]).collect()
    }

    /// Real jemalloc, sampling as configured by `jemalloc_profiling!()` in
    /// this test binary: 64 MiB kept alive by one function shows up under
    /// that function, within sampling error.
    #[test]
    fn retained_memory_is_reported_under_the_function_that_allocated_it() {
        assert!(active(), "the test binary runs jemalloc with prof:true");
        let started = std::time::Instant::now();
        let text = dump().unwrap();
        let dump_cost = started.elapsed();
        assert!(parse(&text).is_ok());
        let kept = retain_for_the_jemalloc_test(1024, 64 * 1024);
        let mut heap = JemallocHeap::default();
        let profiles = heap.collect(&mut Symbolizer::default(), 0, 1).unwrap();
        drop(kept);
        let inuse = profiles
            .iter()
            .find(|p| p.kind.spec().sample.0 == "inuse_space")
            .expect("inuse_space");
        let mine: i64 = inuse
            .samples
            .iter()
            .filter(|s| {
                s.stack
                    .iter()
                    .any(|f| f.function.contains("retain_for_the_jemalloc_test"))
            })
            .map(|s| s.value)
            .sum();
        let expected = 64 * 1024 * 1024;
        let ratio = mine as f64 / expected as f64;
        eprintln!(
            "jemalloc: {mine} of {expected} bytes attributed (ratio {ratio:.2}), dump took {dump_cost:?}"
        );
        assert!(
            (0.7..1.3).contains(&ratio),
            "attributed {mine} bytes, expected about {expected}"
        );
        let leaf = &inuse
            .samples
            .iter()
            .find(|s| s.stack.iter().any(|f| f.function.contains("retain_for")))
            .unwrap()
            .stack[0];
        assert!(
            !leaf.function.contains("je_") && !leaf.function.contains("prof_"),
            "allocator frames trimmed: {}",
            leaf.function
        );
    }
}
