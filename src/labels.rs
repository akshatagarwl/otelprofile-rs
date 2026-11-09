//! Dynamic labels: every CPU sample taken inside a tagged region carries the
//! labels as OTLP sample attributes, so a backend can filter by them.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::context;

/// Runs `f` with `labels` added to (or overriding) the labels already active
/// on this thread.
///
/// ```
/// let total = otelprofile::tag_wrapper(&[("endpoint", "/checkout")], || 2 + 2);
/// assert_eq!(total, 4);
/// ```
pub fn tag_wrapper<R>(labels: &[(&str, &str)], f: impl FnOnce() -> R) -> R {
    let (_, parent) = context::get();
    let _restore = Restore::to(parent, context::intern(parent, labels));
    f()
}

/// Labels for a future: they are active on whatever thread polls it, for
/// the duration of each poll. See [`tag_future`].
pub fn tag_future<F: Future>(labels: &[(&str, &str)], future: F) -> Tagged<F> {
    let (_, parent) = context::get();
    Tagged {
        labels: context::intern(parent, labels),
        future,
    }
}

pin_project_lite::pin_project! {
    /// A future with labels; see [`tag_future`].
    #[must_use = "futures do nothing unless polled"]
    pub struct Tagged<F> {
        labels: u32,
        #[pin]
        future: F,
    }
}

impl<F: Future> Future for Tagged<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        let (_, parent) = context::get();
        let _restore = Restore::to(parent, *this.labels);
        this.future.poll(cx)
    }
}

/// Sets the thread's label set and restores the previous one on drop, also
/// when the tagged code panics.
struct Restore(u32);

impl Restore {
    fn to(previous: u32, labels: u32) -> Self {
        let (link, _) = context::get();
        context::set(link, labels);
        Restore(previous)
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        let (link, _) = context::get();
        context::set(link, self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cpu::test_support::cpu_samples_of;
    use crate::testing::burn;

    fn label<'a>(sample: &'a crate::model::Sample, key: &str) -> Option<&'a str> {
        sample
            .labels
            .iter()
            .find(|(k, _)| &**k == key)
            .map(|(_, v)| &**v)
    }

    #[test]
    fn samples_inside_tag_wrapper_carry_its_labels_and_others_do_not() {
        let samples = cpu_samples_of(|| {
            tag_wrapper(&[("endpoint", "/checkout")], || {
                burn(Duration::from_millis(600))
            });
            burn(Duration::from_millis(300));
        });
        let tagged: i64 = samples
            .iter()
            .filter(|s| label(s, "endpoint") == Some("/checkout"))
            .map(|s| s.value)
            .sum();
        let untagged: i64 = samples
            .iter()
            .filter(|s| label(s, "endpoint").is_none())
            .map(|s| s.value)
            .sum();
        assert!(tagged >= 400_000_000, "tagged {tagged} ns");
        assert!(
            untagged >= 150_000_000,
            "untagged {untagged} ns; labels leaked past tag_wrapper"
        );
    }

    #[test]
    fn a_tagged_future_carries_its_labels_while_polled() {
        let samples = cpu_samples_of(|| {
            let future = tag_future(&[("job", "reindex")], async {
                burn(Duration::from_millis(500))
            });
            let mut future = std::pin::pin!(future);
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(future.as_mut().poll(&mut cx).is_ready());
            burn(Duration::from_millis(200));
        });
        let tagged: i64 = samples
            .iter()
            .filter(|s| label(s, "job") == Some("reindex"))
            .map(|s| s.value)
            .sum();
        assert!(tagged >= 300_000_000, "tagged {tagged} ns");
        assert!(
            samples.iter().any(|s| label(s, "job").is_none()),
            "labels leaked past the poll"
        );
    }
}
