//! Continuous in-process profiling for Rust that pushes standard OTLP
//! Profiles to any receiver.
//!
//! ```no_run
//! let _profiler = otelprofile::start(otelprofile::Config {
//!     service_name: "checkout".into(),
//!     endpoint: "http://localhost:4318".into(),
//!     ..Default::default()
//! })?;
//! # Ok::<(), otelprofile::Error>(())
//! ```
//!
//! Wrap code in [`tag_wrapper`] to label its samples. With the
//! `opentelemetry` feature, samples taken while an OpenTelemetry span is
//! active (including `tracing` spans bridged by `tracing-opentelemetry`)
//! carry that span's trace and span IDs as OTLP sample links.
//!
//! Experimental. The OTLP Profiles signal is still in development
//! (`/v1development/profiles`), so the wire format and this API will change.

mod config;
mod context;
mod cpu;
mod encode;
mod export;
mod labels;
mod model;
mod profiler;
mod proto;
pub mod span;
mod symbolize;
#[cfg(test)]
mod testing;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("otelprofile supports Linux and macOS only");

use std::time::Duration;

pub use config::ConfigError;
pub use labels::{Tagged, tag_future, tag_wrapper};
pub use model::{AttributeValue, SpanLink};
pub use profiler::Profiler;

/// What to profile and where to send it. Every field can be left at its
/// default and set through the environment instead; explicit fields win.
///
/// | Field | Environment | Default |
/// |---|---|---|
/// | `service_name` | `OTEL_SERVICE_NAME` | `unknown_service:<executable>` |
/// | `endpoint` | `OTEL_EXPORTER_OTLP_PROFILES_ENDPOINT` (full URL), `OTEL_EXPORTER_OTLP_ENDPOINT` (base URL) | `http://localhost:4318` |
/// | `headers` | `OTEL_EXPORTER_OTLP_[PROFILES_]HEADERS` | none |
/// | `tags` | `OTEL_RESOURCE_ATTRIBUTES` | host, OS, process, SDK |
/// | `profile_types` | `OTEL_RUST_X_PROFILES_TYPES` (`cpu,heap` or `all`) | CPU and heap |
/// | `interval` | `OTEL_RUST_X_PROFILES_INTERVAL` (`15s`) | 15 s |
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub service_name: String,
    /// A base URL (`http://collector:4318`) or a full profiles URL.
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    /// Extra resource attributes on every profile.
    pub tags: Vec<(String, AttributeValue)>,
    pub profile_types: Vec<ProfileType>,
    pub interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileType {
    /// `cpu` (nanoseconds) and `samples`, at 100 Hz.
    Cpu,
    /// `inuse_*` and `alloc_*` from the active heap backend.
    Heap,
}

/// Why profiling could not start.
#[derive(Debug)]
pub enum Error {
    Config(ConfigError),
    Start(String),
}

impl Error {
    pub(crate) fn config(message: String) -> Self {
        Error::Config(ConfigError(message))
    }
}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Self {
        Error::Config(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Config(e) => write!(f, "otelprofile configuration: {e}"),
            Error::Start(e) => write!(f, "otelprofile could not start: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Starts continuous profiling. Export errors are logged through the `log`
/// crate and never returned or fatal. With `OTEL_SDK_DISABLED=true` this
/// returns a profiler that does nothing.
pub fn start(config: Config) -> Result<Profiler, Error> {
    profiler::start(config)
}
