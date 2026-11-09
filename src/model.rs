//! What the samplers produce and the encoder consumes: profiles of resolved
//! stacks, independent of the OTLP wire layout.

use std::sync::Arc;

/// One symbolized frame. An inlined call becomes several frames, innermost
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Frame {
    pub function: Arc<str>,
    pub file: Arc<str>,
    pub line: u32,
}

/// The span that was active when a sample was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanLink {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
}

/// Sample attributes, such as `thread.name` or `tag_wrapper` labels.
pub(crate) type Labels = Arc<[(Arc<str>, Arc<str>)]>;

#[derive(Debug, Clone)]
pub(crate) struct Sample {
    /// Leaf first.
    pub stack: Arc<[Frame]>,
    pub value: i64,
    pub labels: Labels,
    pub link: Option<SpanLink>,
}

/// Every sample type this crate sends. OTLP carries one per profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // the heap kinds arrive with the heap backends
pub(crate) enum Kind {
    Cpu,
    Samples,
    InuseObjects,
    InuseSpace,
    AllocObjects,
    AllocSpace,
}

/// A sample type and the period type it is measured against, as
/// `(type, unit)` pairs, matching what pprof and otelprofile-go send.
pub(crate) struct KindSpec {
    pub sample: (&'static str, &'static str),
    pub period: (&'static str, &'static str),
}

impl Kind {
    pub(crate) const fn spec(self) -> KindSpec {
        const CPU: (&str, &str) = ("cpu", "nanoseconds");
        const SPACE: (&str, &str) = ("space", "bytes");
        let sample = match self {
            Kind::Cpu => CPU,
            Kind::Samples => ("samples", "count"),
            Kind::InuseObjects => ("inuse_objects", "count"),
            Kind::InuseSpace => ("inuse_space", "bytes"),
            Kind::AllocObjects => ("alloc_objects", "count"),
            Kind::AllocSpace => ("alloc_space", "bytes"),
        };
        let period = match self {
            Kind::Cpu | Kind::Samples => CPU,
            _ => SPACE,
        };
        KindSpec { sample, period }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Profile {
    pub kind: Kind,
    pub time_unix_nano: u64,
    pub duration_nano: u64,
    /// Sampling period in the period type's unit (10 ms for CPU at 100 Hz,
    /// the mean sampling interval in bytes for heap).
    pub period: i64,
    pub samples: Vec<Sample>,
}

/// A resource or scope attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Bool(bool),
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        AttributeValue::String(value.to_string())
    }
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        AttributeValue::String(value)
    }
}

impl From<i64> for AttributeValue {
    fn from(value: i64) -> Self {
        AttributeValue::Int(value)
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        AttributeValue::Bool(value)
    }
}

/// Who produced a batch of profiles.
#[derive(Debug, Clone, Default)]
pub(crate) struct Origin {
    pub resource: Vec<(String, AttributeValue)>,
    pub scope_name: String,
    pub scope_version: String,
}
