//! A fixed, statically allocated multi-producer ring of raw CPU samples.
//! Signal handlers write into it without allocating or locking (Dmitry
//! Vyukov's bounded queue); one sampler thread reads it.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::model::SpanLink;

pub(crate) const MAX_DEPTH: usize = 128;
pub(crate) const THREAD_NAME_LEN: usize = 32;
const CAPACITY: usize = 1024;

/// One sample as the signal handler records it: unresolved addresses, leaf
/// first, the leaf being the interrupted instruction itself.
#[derive(Clone, Copy)]
pub(crate) struct RawSample {
    pub depth: u16,
    pub addrs: [usize; MAX_DEPTH],
    pub thread_name: [u8; THREAD_NAME_LEN],
    pub link: Option<SpanLink>,
    pub labels: u32,
    /// 1 + the blocklist index of the library the thread was interrupted
    /// in, when it was not unwound; 0 otherwise.
    pub blocked: u8,
}

impl RawSample {
    pub(crate) const EMPTY: RawSample = RawSample {
        depth: 0,
        addrs: [0; MAX_DEPTH],
        thread_name: [0; THREAD_NAME_LEN],
        link: None,
        labels: 0,
        blocked: 0,
    };

    pub(crate) fn addrs(&self) -> &[usize] {
        &self.addrs[..usize::from(self.depth)]
    }

    pub(crate) fn thread_name(&self) -> &[u8] {
        let end = self
            .thread_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(THREAD_NAME_LEN);
        &self.thread_name[..end]
    }
}

struct Slot {
    sequence: AtomicUsize,
    sample: UnsafeCell<RawSample>,
}

pub(crate) struct Ring {
    slots: [Slot; CAPACITY],
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
    dropped: AtomicU64,
}

// SAFETY: a slot's sample is written only by the producer that won it through
// `enqueue` and read only by the single consumer after the producer published
// it through `sequence`; the sequence protocol orders the two.
unsafe impl Sync for Ring {}

impl Ring {
    pub(crate) const fn new() -> Self {
        let mut slots = [const {
            Slot {
                sequence: AtomicUsize::new(0),
                sample: UnsafeCell::new(RawSample::EMPTY),
            }
        }; CAPACITY];
        let mut i = 0;
        while i < CAPACITY {
            slots[i].sequence = AtomicUsize::new(i);
            i += 1;
        }
        Self {
            slots,
            enqueue: AtomicUsize::new(0),
            dequeue: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Claims a slot, lets `fill` write the sample in place, and publishes
    /// it. Lock-free and allocation-free, so it is safe in a signal handler.
    /// When the ring is full the sample is counted as dropped.
    pub(crate) fn push(&self, fill: impl FnOnce(&mut RawSample)) {
        let mut position = self.enqueue.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position % CAPACITY];
            let sequence = slot.sequence.load(Ordering::Acquire);
            match (sequence as isize).wrapping_sub(position as isize) {
                0 => match self.enqueue.compare_exchange_weak(
                    position,
                    position.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: winning the CAS gives this producer the
                        // slot until it stores the next sequence below.
                        let sample = unsafe { &mut *slot.sample.get() };
                        fill(sample);
                        slot.sequence
                            .store(position.wrapping_add(1), Ordering::Release);
                        return;
                    }
                    Err(current) => position = current,
                },
                lag if lag < 0 => {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                _ => position = self.enqueue.load(Ordering::Relaxed),
            }
        }
    }

    /// Hands every published sample to `each`. Only one thread may drain.
    pub(crate) fn drain(&self, mut each: impl FnMut(&RawSample)) {
        let mut position = self.dequeue.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[position % CAPACITY];
            if slot.sequence.load(Ordering::Acquire) != position.wrapping_add(1) {
                break;
            }
            // SAFETY: the producer published this slot (sequence ==
            // position + 1) and no producer reuses it until the store below.
            each(unsafe { &*slot.sample.get() });
            slot.sequence
                .store(position.wrapping_add(CAPACITY), Ordering::Release);
            position = position.wrapping_add(1);
        }
        self.dequeue.store(position, Ordering::Relaxed);
    }

    /// Samples dropped because the ring was full, since the last call.
    pub(crate) fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_producers_lose_nothing_below_capacity_and_count_overflow() {
        static RING: Ring = Ring::new();
        std::thread::scope(|s| {
            for t in 0..4u16 {
                s.spawn(move || {
                    for i in 0..300 {
                        RING.push(|sample| {
                            sample.depth = 1;
                            sample.addrs[0] = usize::from(t) * 1000 + i;
                        });
                    }
                });
            }
        });
        let mut seen = Vec::new();
        RING.drain(|sample| seen.push(sample.addrs()[0]));
        assert_eq!(seen.len(), CAPACITY);
        assert_eq!(RING.take_dropped(), 4 * 300 - CAPACITY as u64);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), CAPACITY, "no slot was handed out twice");

        RING.push(|sample| sample.depth = 0);
        let mut again = 0;
        RING.drain(|_| again += 1);
        assert_eq!(again, 1, "slots are reusable after a drain");
    }
}
