//! The shared, content-adaptive encoder effort, consumed by both WebP codecs.

/// How hard an encoder searches for a smaller result.
///
/// [`Effort::AUTO`] (the default) adapts the search depth to the image's content
/// and pixel count. An explicit [`Effort::level`] fixes the breadth on a `0..=9`
/// scale: a higher level searches more families and never yields a *larger* file
/// (each level's candidate set is a superset of the one below, ranked by real
/// emitted bytes). Both the lossless (VP8L) and lossy (VP8) encoders consume the
/// same value.
///
/// This is a semver-safe newtype: new adaptive behavior can be added behind
/// [`AUTO`](Effort::AUTO) without changing the type or its `0..=9` contract.
///
/// The lossless encoder always applies its spatial transforms, so its peak
/// working memory scales with the pixel count at every level — a large image
/// costs the same order of memory under [`AUTO`](Effort::AUTO) as under
/// [`level(9)`](Effort::level), the inherent cost of a best-in-class default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Effort(Depth);

/// The effort's private resolution: adapt to the content, or a fixed `0..=9` level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Depth {
    /// Adapt the search breadth to the content and pixel count.
    Auto,
    /// A fixed search breadth, clamped to `0..=9`.
    Level(u8),
}

/// The highest explicit effort level; the breadth schedule saturates here.
pub(crate) const MAX_LEVEL: u8 = 9;

impl Effort {
    /// Adapt the search breadth to the image's content and size — the default.
    ///
    /// The lossless encoder chooses a level from a cheap fixed-point pre-analysis
    /// (content compressibility and pixel count); the lossy encoder picks a search
    /// tier from the frame size. The real-byte ranking still guarantees the result
    /// is never larger than the transform-free floor, so `AUTO` never regresses.
    pub const AUTO: Self = Self(Depth::Auto);

    /// A fixed effort `level` (`0..=9`; values above `9` clamp to `9`).
    ///
    /// Higher searches more (wider transform-tile sweep, cross-color, optimal-parse
    /// and meta-Huffman shots) and, by real-byte ranking, never produces a larger
    /// file than a lower level. Level `9` is the most thorough search.
    #[must_use]
    pub const fn level(n: u8) -> Self {
        Self(Depth::Level(if n > MAX_LEVEL { MAX_LEVEL } else { n }))
    }

    /// The explicit level, or `None` when adapting automatically.
    pub(crate) const fn explicit_level(self) -> Option<u8> {
        match self.0 {
            Depth::Auto => None,
            Depth::Level(n) => Some(n),
        }
    }
}

impl Default for Effort {
    /// [`Effort::AUTO`].
    fn default() -> Self {
        Self::AUTO
    }
}

#[cfg(test)]
mod tests {
    use super::{Effort, MAX_LEVEL};

    #[test]
    fn default_is_auto() {
        assert_eq!(Effort::default(), Effort::AUTO);
        assert_eq!(Effort::AUTO.explicit_level(), None);
    }

    #[test]
    fn level_clamps_to_the_maximum() {
        assert_eq!(Effort::level(0).explicit_level(), Some(0));
        assert_eq!(Effort::level(9).explicit_level(), Some(9));
        assert_eq!(Effort::level(200).explicit_level(), Some(MAX_LEVEL));
        assert_eq!(Effort::level(3).explicit_level(), Some(3));
    }

    #[test]
    fn auto_and_a_level_are_distinct() {
        assert_ne!(Effort::AUTO, Effort::level(9));
        assert_ne!(Effort::level(0), Effort::level(9));
    }
}
