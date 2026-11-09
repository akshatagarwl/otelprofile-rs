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
// Without `jemalloc`, unused until the sampling allocator lands.
#[cfg_attr(not(feature = "jemalloc"), allow(dead_code))]
mod heap;
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
#[cfg(feature = "jemalloc")]
#[doc(hidden)]
pub use heap::jemalloc::MallocConf;
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

/// Turns on jemalloc's heap sampler from inside the program, so heap
/// profiles work without setting `MALLOC_CONF` (or `_RJEM_MALLOC_CONF`).
/// Use it once, next to `#[global_allocator]`:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
/// otelprofile::jemalloc_profiling!();
/// ```
///
/// The default samples on average one allocation per 512 KiB
/// (`lg_prof_sample:19`) and keeps cumulative counts (`prof_accum:true`) for
/// the `alloc_*` profiles. Pass your own string to change it, for example
/// `jemalloc_profiling!("prof:true,prof_active:true,lg_prof_sample:17")`.
/// The environment variable, when set, still applies on top.
#[cfg(feature = "jemalloc")]
#[macro_export]
macro_rules! jemalloc_profiling {
    () => {
        $crate::jemalloc_profiling!("prof:true,prof_active:true,prof_accum:true,lg_prof_sample:19");
    };
    ($conf:literal) => {
        const _: () = {
            const CONF: &::core::ffi::CStr = match ::core::ffi::CStr::from_bytes_with_nul(
                ::core::concat!($conf, "\0").as_bytes(),
            ) {
                ::core::result::Result::Ok(conf) => conf,
                ::core::result::Result::Err(_) => {
                    ::core::panic!("malloc_conf must not contain NUL")
                }
            };
            // tikv-jemallocator prefixes jemalloc's symbols with `_rjem_`
            // unless built with `unprefixed_malloc_on_supported_platforms`;
            // both names are set so either build reads it.
            #[unsafe(export_name = "_rjem_malloc_conf")]
            #[used]
            static PREFIXED: $crate::MallocConf = $crate::MallocConf::new(CONF);
            #[unsafe(export_name = "malloc_conf")]
            #[used]
            static UNPREFIXED: $crate::MallocConf = $crate::MallocConf::new(CONF);
        };
    };
}

#[cfg(all(test, feature = "jemalloc"))]
#[global_allocator]
static TEST_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(all(test, feature = "jemalloc"))]
jemalloc_profiling!();
