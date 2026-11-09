//! A small checkout service: each request prices a cart (CPU), renders a
//! receipt (allocation churn) and caches it (retained memory), inside
//! tracing spans exported as OTLP traces. otelprofile sends CPU and heap
//! profiles linked to those spans.
//!
//! ```sh
//! OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run -p otelprofile-demo -- 120
//! ```

use std::collections::VecDeque;
use std::hint::black_box;
use std::time::{Duration, Instant};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::{info_span, instrument};
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
#[cfg(feature = "jemalloc")]
otelprofile::jemalloc_profiling!();

#[cfg(not(feature = "jemalloc"))]
#[global_allocator]
static ALLOC: otelprofile::alloc::Sampled<std::alloc::System> =
    otelprofile::alloc::Sampled::new(std::alloc::System);

const SERVICE: &str = "checkout";

fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4318".into());
    let trace_endpoint = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .unwrap_or_else(|_| format!("{}/v1/traces", endpoint.trim_end_matches('/')));
    let traces = SdkTracerProvider::builder()
        .with_batch_exporter(
            SpanExporter::builder()
                .with_http()
                .with_endpoint(trace_endpoint)
                .build()
                .expect("trace exporter"),
        )
        .with_resource(Resource::builder().with_service_name(SERVICE).build())
        .build();
    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(traces.tracer(SERVICE)))
        .with(tracing_subscriber::fmt::layer().with_filter(LevelFilter::INFO))
        .init();

    let profiler = otelprofile::start(otelprofile::Config {
        service_name: SERVICE.into(),
        ..Default::default()
    })
    .expect("start profiling");

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut cache = VecDeque::new();
    let mut order = 0u64;
    while Instant::now() < deadline {
        order += 1;
        let endpoint = if order.is_multiple_of(4) {
            "/express"
        } else {
            "/checkout"
        };
        otelprofile::tag_wrapper(&[("http.route", endpoint)], || {
            checkout(order, &mut cache);
        });
        std::thread::sleep(Duration::from_millis(20));
    }

    drop(profiler);
    let _ = traces.shutdown();
}

#[instrument(skip(cache))]
fn checkout(order: u64, cache: &mut VecDeque<Vec<u8>>) {
    let total = price_cart(order);
    let receipt = render_receipt(order, total);
    cache_receipt(cache, receipt);
}

#[inline(never)]
#[instrument]
fn price_cart(order: u64) -> u64 {
    let items = 40 + order % 20;
    (0..items).map(|item| black_box(price(order, item))).sum()
}

#[inline(never)]
fn price(order: u64, item: u64) -> u64 {
    (0..50_000u64).fold(order ^ item, |x, i| {
        x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i)
    }) % 10_000
}

#[inline(never)]
#[instrument]
fn render_receipt(order: u64, total: u64) -> Vec<u8> {
    let lines: Vec<String> = (0..2_000)
        .map(|i| format!("order {order} line {i}: {} cents", (total + i) % 997))
        .collect();
    lines.join("\n").into_bytes()
}

#[inline(never)]
fn cache_receipt(cache: &mut VecDeque<Vec<u8>>, receipt: Vec<u8>) {
    let _span = info_span!("cache_receipt").entered();
    cache.push_back(receipt);
    while cache.len() > 512 {
        cache.pop_front();
    }
}
