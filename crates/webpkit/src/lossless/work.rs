//! Feature-gated bridge to the deterministic work counters (L0).
//!
//! With the `work-count` feature OFF this compiles to nothing: the [`work!`]
//! macro drops its argument tokens, [`crate::work_count`] is not a dependency, and
//! production output and behavior are unchanged. With it ON, each `work!` site
//! is an `#[inline]` relaxed atomic add.
//!
//! Instrumented modules bring the macro into scope with `use crate::lossless::work::work;`
//! and write `work!(HistogramPass);` (bump by one) or `work!(MatchCompare, n);`
//! (add `n`). The macro expands through the absolute `$crate::lossless::work::Counter`
//! path, so it resolves identically from any call-site module.

/// The counter vocabulary, re-exported so `$crate::lossless::work::Counter` resolves.
#[cfg(feature = "work-count")]
pub(crate) use crate::work_count::Counter;

/// Bump a work counter. `work!(Slot)` adds one; `work!(Slot, n)` adds `n`.
///
/// Expands to nothing (and never type-checks its arguments) when the
/// `work-count` feature is off.
#[cfg(feature = "work-count")]
macro_rules! work {
    ($slot:ident) => {
        $crate::lossless::work::Counter::$slot.bump()
    };
    ($slot:ident, $n:expr) => {
        $crate::lossless::work::Counter::$slot.add($n)
    };
}

#[cfg(not(feature = "work-count"))]
macro_rules! work {
    ($($tt:tt)*) => {{}};
}

pub(crate) use work;
