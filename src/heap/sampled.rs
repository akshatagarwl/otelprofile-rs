//! Heap profiles from the `Sampled` allocator wrapper.

use std::collections::HashMap;
use std::sync::Arc;

use super::{HeapRow, trim_allocator_frames};
use crate::alloc;
use crate::model::{Frame, Profile, SpanLink};
use crate::symbolize::Symbolizer;

pub(crate) struct SampledHeap {
    rate: usize,
}

impl SampledHeap {
    pub(crate) fn new(rate: usize) -> Self {
        alloc::set_collecting(true);
        Self { rate }
    }

    pub(crate) fn collect(
        &mut self,
        symbolizer: &mut Symbolizer,
        start: u64,
        now: u64,
    ) -> Vec<Profile> {
        let live = alloc::live();
        let (stacks, window) =
            alloc::with_registry(|r| (r.stacks.clone(), std::mem::take(&mut r.window)));
        let lost = alloc::take_lost();
        if lost > 0 {
            log::warn!("otelprofile: {lost} heap samples not tracked (live table full)");
        }
        let mut totals: HashMap<(u32, Option<SpanLink>), [f64; 4]> = HashMap::new();
        for allocation in &live {
            let w = alloc::weight(allocation.size, self.rate);
            let t = totals
                .entry((allocation.stack, allocation.link))
                .or_default();
            t[0] += w;
            t[1] += w * allocation.size as f64;
        }
        for (key, (objects, bytes)) in window {
            let t = totals.entry(key).or_default();
            t[2] += objects;
            t[3] += bytes;
        }
        let mut frames_of: HashMap<u32, Arc<[Frame]>> = HashMap::new();
        let mut rows = Vec::with_capacity(totals.len());
        for ((stack, link), [inuse_objects, inuse_bytes, alloc_objects, alloc_bytes]) in totals {
            let Some(addrs) = stacks.get(stack as usize) else {
                continue;
            };
            let frames = frames_of
                .entry(stack)
                .or_insert_with(|| {
                    let mut frames = symbolizer.stack(addrs, false);
                    trim_allocator_frames(&mut frames);
                    frames.into()
                })
                .clone();
            if frames.is_empty() {
                continue;
            }
            rows.push(HeapRow {
                frames,
                link,
                inuse_objects,
                inuse_bytes,
                alloc_objects,
                alloc_bytes,
            });
        }
        super::profiles(&rows, self.rate as i64, start, now, true)
    }
}

impl Drop for SampledHeap {
    fn drop(&mut self) {
        alloc::set_collecting(false);
    }
}
