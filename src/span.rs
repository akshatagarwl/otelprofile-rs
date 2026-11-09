//! Which span is active on this thread, for span links on samples.
//!
//! With the `opentelemetry` feature, [`crate::start`] installs an
//! OpenTelemetry context observer, so every `Context::attach` (including the
//! one `tracing-opentelemetry` does when a span is entered) updates the link
//! automatically. [`enter`] does the same by hand for any other tracer.

use crate::context;
use crate::model::SpanLink;

/// Marks `link` as the active span on this thread until the guard drops.
pub fn enter(link: SpanLink) -> SpanGuard {
    let (previous, labels) = context::get();
    context::set(Some(link), labels);
    SpanGuard { previous }
}

/// Restores the previously active span when dropped.
#[must_use = "the span is only active while the guard lives"]
pub struct SpanGuard {
    previous: Option<SpanLink>,
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        let (_, labels) = context::get();
        context::set(self.previous, labels);
    }
}

#[cfg(feature = "opentelemetry")]
pub(crate) mod otel {
    use std::sync::Arc;

    use opentelemetry::Context;
    use opentelemetry::context::{ContextObserver, GlobalContextObserver};
    use opentelemetry::trace::TraceContextExt;

    use crate::context;
    use crate::model::SpanLink;

    impl From<&opentelemetry::trace::SpanContext> for SpanLink {
        fn from(span: &opentelemetry::trace::SpanContext) -> Self {
            SpanLink {
                trace_id: span.trace_id().to_bytes(),
                span_id: span.span_id().to_bytes(),
            }
        }
    }

    /// The span of `cx`, if it has a valid one. Uses only getters on `cx`,
    /// as the observer contract requires.
    fn link(cx: &Context) -> Option<SpanLink> {
        let span = cx.span();
        let span = span.span_context();
        span.is_valid().then(|| SpanLink::from(span))
    }

    struct Observer;

    impl ContextObserver for Observer {
        fn on_context_enter(&self, _from: &Context, to: &Context) {
            let (_, labels) = context::get();
            context::set(link(to), labels);
        }

        fn on_context_exit(&self, _from: &Context, to: &Context) {
            let (_, labels) = context::get();
            context::set(link(to), labels);
        }
    }

    /// Installs the observer once per process. OpenTelemetry keeps a single
    /// global observer; if another was set first, it logs a warning and
    /// samples carry no links.
    pub(crate) fn install() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| GlobalContextObserver::set(Arc::new(Observer)));
    }
}

#[cfg(all(test, feature = "opentelemetry"))]
mod tests {
    use std::time::Duration;

    use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::cpu::test_support::{cpu_samples_of, in_function};
    use crate::model::SpanLink;
    use crate::testing::burn;

    #[inline(never)]
    fn inside_the_span(budget: Duration) -> u64 {
        burn(budget)
    }

    #[inline(never)]
    fn outside_the_span(budget: Duration) -> u64 {
        burn(budget)
    }

    #[test]
    fn samples_taken_inside_a_tracing_span_link_to_its_otel_ids() {
        super::otel::install();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let mut expected = None;
        let samples = tracing::subscriber::with_default(subscriber, || {
            cpu_samples_of(|| {
                let span = tracing::info_span!("checkout");
                expected = Some(SpanLink::from(span.context().span().span_context()));
                span.in_scope(|| inside_the_span(Duration::from_millis(600)));
                outside_the_span(Duration::from_millis(300));
            })
        });
        let expected = expected.unwrap();
        assert_ne!(expected.span_id, [0; 8]);
        let linked: i64 = samples
            .iter()
            .filter(|s| {
                s.link == Some(expected)
                    && s.stack
                        .iter()
                        .any(|f| f.function.contains("inside_the_span"))
            })
            .map(|s| s.value)
            .sum();
        let inside = in_function(&samples, "inside_the_span");
        assert!(
            inside >= 400_000_000 && linked * 10 >= inside * 9,
            "{linked} of {inside} ns linked"
        );
        let outside_linked = samples
            .iter()
            .filter(|s| {
                s.link.is_some()
                    && s.stack
                        .iter()
                        .any(|f| f.function.contains("outside_the_span"))
            })
            .count();
        assert_eq!(outside_linked, 0, "the link outlived the span");
    }

    #[test]
    fn enter_links_samples_for_any_tracer() {
        let link = SpanLink {
            trace_id: [3; 16],
            span_id: [4; 8],
        };
        let samples = cpu_samples_of(|| {
            let _guard = super::enter(link);
            inside_the_span(Duration::from_millis(400));
        });
        let linked: i64 = samples
            .iter()
            .filter(|s| s.link == Some(link))
            .map(|s| s.value)
            .sum();
        assert!(linked >= 250_000_000, "{linked} ns linked");
    }
}
