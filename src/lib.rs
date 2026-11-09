//! Continuous in-process profiling for Rust that pushes standard OTLP Profiles.
//!
//! Experimental. The OTLP Profiles signal is still in development
//! (`/v1development/profiles`), so the wire format and this API will change.

// Wired into the public API by the exporter and samplers that follow.
#![allow(dead_code)]

mod config;
mod encode;
mod export;
mod model;
mod proto;
#[cfg(test)]
mod testing;

pub use model::{AttributeValue, SpanLink};
