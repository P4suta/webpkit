//! The shared encoder effort preset, used by both WebP codecs.

/// How hard an encoder searches for a smaller result.
///
/// Higher effort spends more time to produce a smaller (and, for lossy, closer to
/// the source) file; the output is deterministic per effort. The lossless (VP8L)
/// and lossy (VP8) encoders share these three tiers — see each codec's `encode`
/// entry point for exactly what a tier turns on.
///
/// A closed, stable set (the well-established Fast / Balanced / Best trichotomy),
/// so this is deliberately *not* `#[non_exhaustive]`: both codecs match it
/// exhaustively.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Effort {
    /// Fastest: the least search, largest output.
    Fast,
    /// Balanced (the default): the full practical search.
    #[default]
    Balanced,
    /// Best: the most thorough search; never larger than [`Effort::Balanced`].
    Best,
}
