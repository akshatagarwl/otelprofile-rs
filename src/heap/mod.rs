//! Heap profiles, from jemalloc's own sampler or from the `Sampled`
//! allocator wrapper. Both produce sampled stacks; this module un-biases them
//! and builds the four pprof heap types.

#[cfg(feature = "jemalloc")]
pub(crate) mod jemalloc;

use std::sync::Arc;

use crate::model::{Frame, Kind, Labels, Profile, Sample, SpanLink};

/// Sampled allocations at one stack, already scaled to estimated totals.
pub(crate) struct HeapRow {
    pub frames: Arc<[Frame]>,
    pub link: Option<SpanLink>,
    pub inuse_objects: f64,
    pub inuse_bytes: f64,
    /// Allocated during the window.
    pub alloc_objects: f64,
    pub alloc_bytes: f64,
}

/// The estimated number of allocations and bytes behind `objects` sampled
/// allocations totalling `bytes`, when allocations are sampled with mean
/// interval `rate` bytes (Poisson). jeprof's and Go's formula: an
/// allocation of size `s` is sampled with probability `1 - e^(-s/rate)`,
/// applied here to the stack's average size.
pub(crate) fn unbias(objects: f64, bytes: f64, rate: f64) -> (f64, f64) {
    if objects <= 0.0 || bytes <= 0.0 || rate <= 0.0 {
        return (0.0, 0.0);
    }
    let average = bytes / objects;
    let scale = 1.0 / (1.0 - (-average / rate).exp());
    (objects * scale, bytes * scale)
}

/// `inuse_*` (a snapshot at `now`) and `alloc_*` (the window) profiles.
pub(crate) fn profiles(
    rows: &[HeapRow],
    rate: i64,
    start: u64,
    now: u64,
    alloc: bool,
) -> Vec<Profile> {
    let kinds: &[Kind] = if alloc {
        &[
            Kind::InuseObjects,
            Kind::InuseSpace,
            Kind::AllocObjects,
            Kind::AllocSpace,
        ]
    } else {
        &[Kind::InuseObjects, Kind::InuseSpace]
    };
    let no_labels: Labels = Arc::from([]);
    kinds
        .iter()
        .map(|&kind| {
            let samples = rows
                .iter()
                .filter_map(|row| {
                    let value = match kind {
                        Kind::InuseObjects => row.inuse_objects,
                        Kind::InuseSpace => row.inuse_bytes,
                        Kind::AllocObjects => row.alloc_objects,
                        _ => row.alloc_bytes,
                    }
                    .round() as i64;
                    (value > 0).then(|| Sample {
                        stack: row.frames.clone(),
                        value,
                        labels: no_labels.clone(),
                        link: row.link,
                    })
                })
                .collect();
            let snapshot = matches!(kind, Kind::InuseObjects | Kind::InuseSpace);
            Profile {
                kind,
                time_unix_nano: if snapshot { now } else { start },
                duration_nano: if snapshot {
                    0
                } else {
                    now.saturating_sub(start)
                },
                period: rate,
                samples,
            }
        })
        .filter(|p| !p.samples.is_empty())
        .collect()
}

/// Leaf frames that belong to the allocator or the sampler, matched after
/// leading underscores (macOS prefixes C symbols with one).
const ALLOCATOR_PREFIXES: &[&str] = &[
    "backtrace::",
    "<backtrace::",
    "rjem_",
    "je_",
    "prof_",
    "malloc",
    "mallocx",
    "rallocx",
    "do_rallocx",
    "calloc",
    "realloc",
    "posix_memalign",
    "aligned_alloc",
    "rust_alloc",
    "rust_realloc",
    "rdl_",
    "tikv_jemallocator::",
    "<tikv_jemallocator::",
    "otelprofile::alloc::",
    "<otelprofile::alloc::",
    "otelprofile::heap::",
];

/// Inlined standard-library allocation frames, by short name.
const ALLOCATOR_FUNCTIONS: &[&str] = &[
    "alloc",
    "alloc_zeroed",
    "alloc_impl",
    "alloc_impl_runtime",
    "allocate",
    "allocate_in",
    "try_allocate_in",
    "realloc",
    "grow_impl",
    "grow_one",
    "grow_amortized",
    "finish_grow",
    "box_new_uninit",
];

/// Drops allocator and sampler frames from the leaf end.
pub(crate) fn trim_allocator_frames(frames: &mut Vec<Frame>) {
    let keep_from = frames
        .iter()
        .position(|frame| {
            let name = frame.function.trim_start_matches('_');
            let short = name.split('<').next().unwrap_or(name);
            let short = short.rsplit("::").next().unwrap_or(short);
            !(ALLOCATOR_FUNCTIONS.contains(&short) && name.contains("alloc")
                || ALLOCATOR_FUNCTIONS.contains(&name)
                || ALLOCATOR_PREFIXES.iter().any(|p| name.starts_with(p)))
        })
        .unwrap_or(frames.len());
    frames.drain(..keep_from);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(function: &str) -> Frame {
        Frame {
            function: function.into(),
            file: "".into(),
            line: 0,
        }
    }

    #[test]
    fn unbiasing_scales_small_allocations_up_and_leaves_large_ones() {
        let rate = 524_288.0;
        let (objects, bytes) = unbias(1.0, 64.0, rate);
        assert!((objects - 8192.5).abs() < 1.0, "{objects}");
        assert!((bytes / objects - 64.0).abs() < 1e-9);
        let (objects, _) = unbias(1.0, 64.0 * 1024.0 * 1024.0, rate);
        assert!((objects - 1.0).abs() < 1e-6);
        assert_eq!(unbias(0.0, 0.0, rate), (0.0, 0.0));
    }

    #[test]
    fn allocator_frames_are_trimmed_from_the_leaf_only() {
        let mut frames = vec![
            frame("backtrace::backtrace::trace_unsynchronized"),
            frame("__rjem_je_prof_backtrace"),
            frame("__rjem_je_malloc_default"),
            frame("alloc"),
            frame("__rust_alloc"),
            frame("alloc::raw_vec::finish_grow<alloc::alloc::Global>"),
            frame(
                "<otelprofile::alloc::Sampled<std::alloc::System> as core::alloc::global::GlobalAlloc>::alloc",
            ),
            frame("app::load"),
            frame("je_malloc_default"),
            frame("main"),
        ];
        trim_allocator_frames(&mut frames);
        assert_eq!(
            frames,
            [
                frame("app::load"),
                frame("je_malloc_default"),
                frame("main")
            ]
        );
    }
}
