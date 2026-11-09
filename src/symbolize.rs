//! Addresses to frames, in-process, with `backtrace`. Results are cached per
//! address: code addresses are finite, so the cache is bounded by the size
//! of the program.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Frame;

#[derive(Default)]
pub(crate) struct Symbolizer {
    addresses: HashMap<usize, Arc<[Frame]>>,
    strings: HashMap<String, Arc<str>>,
}

impl Symbolizer {
    /// A stack from raw addresses, leaf first. The leaf is an exact
    /// instruction when `exact_leaf` (a CPU sample's interrupted pc); every
    /// other address is a return address, so the call is the byte before.
    pub(crate) fn stack(&mut self, addrs: &[usize], exact_leaf: bool) -> Vec<Frame> {
        let mut frames = Vec::with_capacity(addrs.len() * 2);
        for (i, &addr) in addrs.iter().enumerate() {
            let lookup = if i == 0 && exact_leaf {
                addr
            } else {
                addr.saturating_sub(1)
            };
            frames.extend(self.address(lookup).iter().cloned());
        }
        frames
    }

    pub(crate) fn intern(&mut self, s: &str) -> Arc<str> {
        if let Some(interned) = self.strings.get(s) {
            return interned.clone();
        }
        let interned: Arc<str> = Arc::from(s);
        self.strings.insert(s.to_string(), interned.clone());
        interned
    }

    /// The frames at one address: inlined functions first, then the
    /// function they were inlined into.
    fn address(&mut self, addr: usize) -> Arc<[Frame]> {
        if let Some(frames) = self.addresses.get(&addr) {
            return frames.clone();
        }
        let mut resolved = Vec::new();
        backtrace::resolve(addr as *mut std::ffi::c_void, |symbol| {
            let function = match symbol.name() {
                Some(name) => format!("{name:#}"),
                None => format!("0x{addr:x}"),
            };
            let file = symbol
                .filename()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            resolved.push((function, file, symbol.lineno().unwrap_or(0)));
        });
        if resolved.is_empty() {
            resolved.push((format!("0x{addr:x}"), String::new(), 0));
        }
        let frames: Arc<[Frame]> = resolved
            .into_iter()
            .map(|(function, file, line)| Frame {
                function: self.intern(&function),
                file: self.intern(&file),
                line,
            })
            .collect();
        self.addresses.insert(addr, frames.clone());
        frames
    }
}
