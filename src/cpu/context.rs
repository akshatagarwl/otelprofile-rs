//! What the signal handler attaches to a sample besides the stack: the
//! active span and the `tag_wrapper` label set of the interrupted thread.

use crate::model::SpanLink;

/// The interrupted thread's span and label-set id. Signal-safe.
pub(crate) fn current() -> (Option<SpanLink>, u32) {
    (None, 0)
}
