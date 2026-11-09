//! The active span and label set of each thread, readable from a signal
//! handler.
//!
//! Each thread that ever enters a span or a `tag_wrapper` claims a slot in a
//! fixed global table keyed by its `pthread_t`. Only that thread writes its
//! slot, as a sequence lock (odd while writing); a SIGPROF handler running on
//! the same thread reads it with plain atomic loads. Nothing here allocates
//! on the read side and nothing touches a thread-local there: the first
//! touch of a thread-local can allocate on macOS and in `dlopen`ed code.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::model::{Labels, SpanLink};

const SLOTS: usize = 4096;
const PROBES: usize = 64;

struct Slot {
    owner: AtomicUsize,
    sequence: AtomicU32,
    trace_hi: AtomicU64,
    trace_lo: AtomicU64,
    span: AtomicU64,
    labels: AtomicU32,
}

static TABLE: [Slot; SLOTS] = [const {
    Slot {
        owner: AtomicUsize::new(0),
        sequence: AtomicU32::new(0),
        trace_hi: AtomicU64::new(0),
        trace_lo: AtomicU64::new(0),
        span: AtomicU64::new(0),
        labels: AtomicU32::new(0),
    }
}; SLOTS];

fn thread_key() -> usize {
    // SAFETY: always valid for the calling thread. `pthread_t` is never 0.
    unsafe { libc::pthread_self() as usize }
}

fn home(key: usize) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize) >> 7) % SLOTS
}

/// The calling thread's span and label-set id. Signal-safe: atomic loads
/// only. A read that races the thread's own write (a signal landing
/// mid-update) reports nothing rather than a torn value.
pub(crate) fn current() -> (Option<SpanLink>, u32) {
    let key = thread_key();
    let start = home(key);
    for probe in 0..PROBES {
        let slot = &TABLE[(start + probe) % SLOTS];
        if slot.owner.load(Ordering::Acquire) != key {
            continue;
        }
        let before = slot.sequence.load(Ordering::Acquire);
        if before % 2 == 1 {
            return (None, 0);
        }
        let hi = slot.trace_hi.load(Ordering::Relaxed);
        let lo = slot.trace_lo.load(Ordering::Relaxed);
        let span = slot.span.load(Ordering::Relaxed);
        let labels = slot.labels.load(Ordering::Relaxed);
        let link = (span != 0).then(|| SpanLink {
            trace_id: ((u128::from(hi) << 64) | u128::from(lo)).to_be_bytes(),
            span_id: span.to_be_bytes(),
        });
        return (link, labels);
    }
    (None, 0)
}

/// Releases the thread's slot when the thread exits, so a later thread that
/// reuses the same `pthread_t` does not inherit its span.
struct Claim(usize);

impl Drop for Claim {
    fn drop(&mut self) {
        let slot = &TABLE[self.0];
        slot.span.store(0, Ordering::Relaxed);
        slot.labels.store(0, Ordering::Relaxed);
        slot.owner.store(0, Ordering::Release);
    }
}

thread_local! {
    static CLAIM: Cell<Option<usize>> = const { Cell::new(None) };
    static RELEASE: std::cell::RefCell<Option<Claim>> = const { std::cell::RefCell::new(None) };
    static STATE: Cell<(Option<SpanLink>, u32)> = const { Cell::new((None, 0)) };
}

fn claim() -> Option<usize> {
    if let Some(index) = CLAIM.with(Cell::get) {
        return Some(index);
    }
    let key = thread_key();
    let start = home(key);
    for probe in 0..PROBES {
        let index = (start + probe) % SLOTS;
        let owner = &TABLE[index].owner;
        let won = owner.load(Ordering::Relaxed) == key
            || owner
                .compare_exchange(0, key, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok();
        if won {
            CLAIM.with(|c| c.set(Some(index)));
            RELEASE.with(|r| *r.borrow_mut() = Some(Claim(index)));
            return Some(index);
        }
    }
    None
}

/// The calling thread's span and label-set id, from its own (non-signal)
/// context.
pub(crate) fn get() -> (Option<SpanLink>, u32) {
    STATE.with(Cell::get)
}

/// Sets the calling thread's span and label-set id; returns the previous.
pub(crate) fn set(link: Option<SpanLink>, labels: u32) -> (Option<SpanLink>, u32) {
    let previous = STATE.with(|s| s.replace((link, labels)));
    if previous == (link, labels) {
        return previous;
    }
    let Some(index) = claim() else {
        return previous;
    };
    let slot = &TABLE[index];
    let sequence = slot.sequence.load(Ordering::Relaxed);
    slot.sequence
        .store(sequence.wrapping_add(1), Ordering::Relaxed);
    std::sync::atomic::fence(Ordering::Release);
    let (trace, span) = link.map_or((0u128, 0u64), |l| {
        (
            u128::from_be_bytes(l.trace_id),
            u64::from_be_bytes(l.span_id),
        )
    });
    slot.trace_hi.store((trace >> 64) as u64, Ordering::Relaxed);
    slot.trace_lo.store(trace as u64, Ordering::Relaxed);
    slot.span.store(span, Ordering::Relaxed);
    slot.labels.store(labels, Ordering::Relaxed);
    slot.sequence
        .store(sequence.wrapping_add(2), Ordering::Release);
    previous
}

type LabelPairs = Vec<(Arc<str>, Arc<str>)>;

/// Interned label sets. Id 0 is the empty set.
struct Registry {
    ids: HashMap<LabelPairs, u32>,
    sets: Vec<Labels>,
    warned: bool,
}

/// Distinct label sets kept; beyond this new sets are not recorded.
const MAX_LABEL_SETS: usize = 16_384;

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let registry = guard.get_or_insert_with(|| Registry {
        ids: HashMap::new(),
        sets: vec![Arc::from([])],
        warned: false,
    });
    f(registry)
}

/// The id of `parent`'s labels overridden by `labels`.
pub(crate) fn intern(parent: u32, labels: &[(&str, &str)]) -> u32 {
    with_registry(|registry| {
        let mut merged: LabelPairs = registry
            .sets
            .get(parent as usize)
            .map(|set| set.to_vec())
            .unwrap_or_default();
        for &(key, value) in labels {
            merged.retain(|(k, _)| &**k != key);
            merged.push((Arc::from(key), Arc::from(value)));
        }
        merged.sort();
        if let Some(&id) = registry.ids.get(&merged) {
            return id;
        }
        if registry.sets.len() >= MAX_LABEL_SETS {
            if !registry.warned {
                registry.warned = true;
                log::warn!(
                    "otelprofile: over {MAX_LABEL_SETS} distinct label sets; new ones are not recorded"
                );
            }
            return parent;
        }
        let id = registry.sets.len() as u32;
        registry.sets.push(merged.clone().into());
        registry.ids.insert(merged, id);
        id
    })
}

pub(crate) fn labels(id: u32) -> Labels {
    if id == 0 {
        return Arc::from([]);
    }
    with_registry(|registry| {
        registry
            .sets
            .get(id as usize)
            .cloned()
            .unwrap_or_else(|| Arc::from([]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: SpanLink = SpanLink {
        trace_id: [7; 16],
        span_id: [9; 8],
    };

    #[test]
    fn the_signal_side_reads_what_the_thread_set() {
        std::thread::spawn(|| {
            assert_eq!(current(), (None, 0));
            let id = intern(0, &[("endpoint", "/pay")]);
            set(Some(LINK), id);
            assert_eq!(current(), (Some(LINK), id));
            assert_eq!(&*labels(id), &[(Arc::from("endpoint"), Arc::from("/pay"))]);
            set(None, 0);
            assert_eq!(current(), (None, 0));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn threads_do_not_see_each_others_spans_and_exits_release_slots() {
        let handles: Vec<_> = (0..32u8)
            .map(|i| {
                std::thread::spawn(move || {
                    let link = SpanLink {
                        trace_id: [i; 16],
                        span_id: [i + 1; 8],
                    };
                    set(Some(link), 0);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    assert_eq!(current().0, Some(link));
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let owned = TABLE
            .iter()
            .filter(|s| s.owner.load(Ordering::Relaxed) != 0)
            .count();
        assert!(
            owned < 8,
            "{owned} slots still owned after their threads exited"
        );
    }

    #[test]
    fn nested_labels_override_and_inherit() {
        let outer = intern(0, &[("endpoint", "/pay"), ("tier", "gold")]);
        let inner = intern(outer, &[("endpoint", "/refund")]);
        let mut got: Vec<_> = labels(inner)
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("endpoint".to_string(), "/refund".to_string()),
                ("tier".into(), "gold".into())
            ]
        );
        assert_eq!(
            intern(0, &[("tier", "gold"), ("endpoint", "/pay")]),
            outer,
            "order does not matter"
        );
    }
}
