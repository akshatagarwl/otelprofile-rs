//! A sampling global allocator: heap profiles for any allocator, with span
//! links.
//!
//! ```
//! #[global_allocator]
//! static ALLOC: otelprofile::alloc::Sampled<std::alloc::System> =
//!     otelprofile::alloc::Sampled::new(std::alloc::System);
//! ```
//!
//! On average one allocation per 512 KiB allocated is sampled (Poisson by
//! bytes, as in Go and jemalloc). A sampled allocation records its stack and
//! the active span. Every other allocation costs one thread-local counter
//! update, and every free one probe of a lock-free table of the sampled
//! allocations that are still live.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::model::SpanLink;

/// The default mean sampling interval: 512 KiB.
pub const DEFAULT_RATE: usize = 512 * 1024;
const MAX_DEPTH: usize = 64;
const SLOTS: usize = 1 << 16;
const PROBES: usize = 64;
const EMPTY: usize = 0;
const DELETED: usize = 1;
const RESERVED: usize = 2;

/// Wraps an allocator and samples its allocations for heap profiles.
pub struct Sampled<A> {
    inner: A,
    rate: usize,
}

impl<A> Sampled<A> {
    /// Samples one allocation per 512 KiB on average.
    pub const fn new(inner: A) -> Self {
        Self::with_rate(inner, DEFAULT_RATE)
    }

    /// Samples one allocation per `rate` bytes on average. Lower is more
    /// precise and costs more.
    pub const fn with_rate(inner: A, rate: usize) -> Self {
        Self {
            inner,
            rate: if rate == 0 { 1 } else { rate },
        }
    }
}

// SAFETY: every method forwards to `inner` with the same arguments and
// returns its result unchanged; the bookkeeping never touches the memory.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Sampled<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let ptr = unsafe { self.inner.alloc(layout) };
        self.allocated(ptr, layout.size());
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        self.allocated(ptr, layout.size());
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        freed(ptr);
        // SAFETY: forwarded as is.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded as is.
        let moved = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            freed(ptr);
            self.allocated(moved, new_size);
        }
        moved
    }
}

impl<A> Sampled<A> {
    #[inline]
    fn allocated(&self, ptr: *mut u8, size: usize) {
        if ptr.is_null() {
            return;
        }
        let left = COUNTDOWN.get() - size as isize;
        if left > 0 {
            COUNTDOWN.set(left);
            return;
        }
        self.sample(ptr as usize, size);
    }

    #[cold]
    fn sample(&self, ptr: usize, size: usize) {
        COUNTDOWN.set(next_interval(self.rate));
        if !INSTALLED.load(Ordering::Relaxed) {
            RATE.store(self.rate, Ordering::Relaxed);
            INSTALLED.store(true, Ordering::Relaxed);
        }
        if BUSY.get() {
            return;
        }
        BUSY.set(true);
        record(ptr, size, self.rate);
        BUSY.set(false);
    }
}

thread_local! {
    /// Bytes left before this thread's next sample.
    static COUNTDOWN: Cell<isize> = const { Cell::new(0) };
    /// The thread is inside the sampler (or holds its lock); its own
    /// allocations are not sampled.
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static SEED: Cell<u64> = const { Cell::new(0) };
}

static INSTALLED: AtomicBool = AtomicBool::new(false);
/// A profiler is collecting `alloc_*` windows; without one, only live
/// allocations are tracked, so memory stays bounded.
static COLLECTING: AtomicBool = AtomicBool::new(false);
static RATE: AtomicUsize = AtomicUsize::new(DEFAULT_RATE);

/// The sampling rate when `Sampled` is the global allocator.
pub(crate) fn installed_rate() -> Option<usize> {
    INSTALLED
        .load(Ordering::Relaxed)
        .then(|| RATE.load(Ordering::Relaxed))
}

/// An exponential draw with mean `rate`, so samples form a Poisson process
/// over allocated bytes.
fn next_interval(rate: usize) -> isize {
    let mut x = SEED.get();
    if x == 0 {
        x = (&SEED as *const _ as u64) ^ 0x9E37_79B9_7F4A_7C15 ^ u64::from(std::process::id());
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.set(x);
    let uniform = ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    (-uniform.ln() * rate as f64).min(isize::MAX as f64) as isize + 1
}

struct LiveSlot {
    ptr: AtomicUsize,
    stack: AtomicU32,
    size: AtomicUsize,
    trace_hi: AtomicU64,
    trace_lo: AtomicU64,
    span: AtomicU64,
}

static LIVE: [LiveSlot; SLOTS] = [const {
    LiveSlot {
        ptr: AtomicUsize::new(EMPTY),
        stack: AtomicU32::new(0),
        size: AtomicUsize::new(0),
        trace_hi: AtomicU64::new(0),
        trace_lo: AtomicU64::new(0),
        span: AtomicU64::new(0),
    }
}; SLOTS];
static LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static LOST: AtomicU64 = AtomicU64::new(0);

fn home(ptr: usize) -> usize {
    (ptr.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize) >> 16) % SLOTS
}

#[inline]
fn freed(ptr: *mut u8) {
    if LIVE_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let ptr = ptr as usize;
    let start = home(ptr);
    for probe in 0..PROBES {
        let slot = &LIVE[(start + probe) % SLOTS];
        match slot.ptr.load(Ordering::Acquire) {
            EMPTY => return,
            p if p == ptr => {
                if slot
                    .ptr
                    .compare_exchange(ptr, DELETED, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    LIVE_COUNT.fetch_sub(1, Ordering::Relaxed);
                }
                return;
            }
            _ => {}
        }
    }
}

/// Stacks seen by the sampler and the allocations counted per window.
#[derive(Default)]
pub(crate) struct Registry {
    ids: HashMap<Box<[usize]>, u32>,
    pub(crate) stacks: Vec<Box<[usize]>>,
    /// Estimated objects and bytes allocated since the last collection,
    /// per stack and span.
    pub(crate) window: HashMap<(u32, Option<SpanLink>), (f64, f64)>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

/// Runs `f` on the registry with this thread's sampling off, so nothing
/// allocated under the lock re-enters the sampler.
pub(crate) fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let was_busy = BUSY.replace(true);
    let result = {
        let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.get_or_insert_with(Registry::default))
    };
    BUSY.set(was_busy);
    result
}

/// The estimated number of allocations a sample of `size` bytes stands for.
pub(crate) fn weight(size: usize, rate: usize) -> f64 {
    crate::heap::unbias(1.0, size as f64, rate as f64).0
}

fn record(ptr: usize, size: usize, rate: usize) {
    let mut addrs = [0usize; MAX_DEPTH];
    let mut depth = 0;
    // SAFETY: unwinds the current thread; the BUSY flag keeps this thread
    // from re-entering through allocations the unwinder makes.
    unsafe {
        backtrace::trace_unsynchronized(|frame| {
            addrs[depth] = frame.ip() as usize;
            depth += 1;
            depth < MAX_DEPTH
        });
    }
    let (link, _) = crate::context::get();
    let stack = {
        let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let registry = guard.get_or_insert_with(Registry::default);
        let key = &addrs[..depth];
        let id = match registry.ids.get(key) {
            Some(&id) => id,
            None => {
                let id = registry.stacks.len() as u32;
                registry.stacks.push(key.into());
                registry.ids.insert(key.into(), id);
                id
            }
        };
        if COLLECTING.load(Ordering::Relaxed) {
            let w = weight(size, rate);
            let entry = registry.window.entry((id, link)).or_default();
            entry.0 += w;
            entry.1 += w * size as f64;
        }
        id
    };
    let start = home(ptr);
    for probe in 0..PROBES {
        let slot = &LIVE[(start + probe) % SLOTS];
        let current = slot.ptr.load(Ordering::Relaxed);
        if (current == EMPTY || current == DELETED)
            && slot
                .ptr
                .compare_exchange(current, RESERVED, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        {
            let (trace, span) = link.map_or((0u128, 0u64), |l| {
                (
                    u128::from_be_bytes(l.trace_id),
                    u64::from_be_bytes(l.span_id),
                )
            });
            slot.stack.store(stack, Ordering::Relaxed);
            slot.size.store(size, Ordering::Relaxed);
            slot.trace_hi.store((trace >> 64) as u64, Ordering::Relaxed);
            slot.trace_lo.store(trace as u64, Ordering::Relaxed);
            slot.span.store(span, Ordering::Relaxed);
            slot.ptr.store(ptr, Ordering::Release);
            LIVE_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    LOST.fetch_add(1, Ordering::Relaxed);
}

/// One live sampled allocation.
pub(crate) struct Live {
    pub stack: u32,
    pub size: usize,
    pub link: Option<SpanLink>,
}

/// Every sampled allocation that has not been freed.
pub(crate) fn live() -> Vec<Live> {
    let was_busy = BUSY.replace(true);
    let mut out = Vec::with_capacity(LIVE_COUNT.load(Ordering::Relaxed));
    for slot in &LIVE {
        if slot.ptr.load(Ordering::Acquire) <= RESERVED {
            continue;
        }
        let span = slot.span.load(Ordering::Relaxed);
        let hi = slot.trace_hi.load(Ordering::Relaxed);
        let lo = slot.trace_lo.load(Ordering::Relaxed);
        out.push(Live {
            stack: slot.stack.load(Ordering::Relaxed),
            size: slot.size.load(Ordering::Relaxed),
            link: (span != 0).then(|| SpanLink {
                trace_id: ((u128::from(hi) << 64) | u128::from(lo)).to_be_bytes(),
                span_id: span.to_be_bytes(),
            }),
        });
    }
    BUSY.set(was_busy);
    out
}

/// Turns per-window `alloc_*` accounting on or off.
pub(crate) fn set_collecting(on: bool) {
    with_registry(|registry| {
        COLLECTING.store(on, Ordering::Relaxed);
        registry.window.clear();
    });
}

/// Sampled allocations not tracked because the live table was full near
/// their slot, since the last call.
pub(crate) fn take_lost() -> u64 {
    LOST.swap(0, Ordering::Relaxed)
}
