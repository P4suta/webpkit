//! Command-line argument vocabulary shared by the binaries.

pub mod brand;
pub mod cwebp;
pub mod dwebp;

use clap::ValueEnum;

/// Pixel byte order, mirroring [`webpkit::lossless::PixelLayout`].
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Layout {
    /// `R, G, B, A`.
    #[default]
    Rgba8,
    /// `A, R, G, B`.
    Argb8,
    /// `B, G, R, A`.
    Bgra8,
}

impl From<Layout> for webpkit::lossless::PixelLayout {
    fn from(layout: Layout) -> Self {
        match layout {
            Layout::Rgba8 => Self::Rgba8,
            Layout::Argb8 => Self::Argb8,
            Layout::Bgra8 => Self::Bgra8,
        }
    }
}

/// Encoder effort, mirroring [`webpkit::lossless::Effort`].
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Method {
    /// Fastest: literal + subtract-green only.
    Fast,
    /// Balanced (the default): LZ77 + color cache.
    #[default]
    Balanced,
    /// Smallest: adds Tier 3 forward transforms and meta-Huffman on top of Balanced.
    Best,
}

// The lossless and lossy encoders now share one `Effort` dial (re-exported from
// `webpkit` as `webpkit::Effort` / `webpkit::lossless::Effort`), so a single
// conversion covers both `--lossless` and `--lossy` requests. Adding a preset to
// `Effort` requires a matching variant here and an update to
// `tests::EXPECTED_PRESETS` below (it guards against drift).
impl From<Method> for webpkit::Effort {
    fn from(method: Method) -> Self {
        match method {
            Method::Fast => Self::Fast,
            Method::Balanced => Self::Balanced,
            Method::Best => Self::Best,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use webpkit::lossless::Effort as LosslessMethod;

    use super::Method;

    /// The lossless presets the CLI mirrors. `webpkit::lossless::Effort` is
    /// `#[non_exhaustive]`, so its variants cannot be enumerated from here — this
    /// explicit list is the synchronization anchor. A new lossless preset must be
    /// added both as a [`Method`] variant and to this array.
    const EXPECTED_PRESETS: &[LosslessMethod] = &[
        LosslessMethod::Fast,
        LosslessMethod::Balanced,
        LosslessMethod::Best,
    ];

    /// Every `cli::Method` maps onto a distinct lossless preset, and together they
    /// cover exactly the expected set — so a preset added to only one side (or a
    /// collapsed mapping) trips this test.
    #[test]
    fn cli_method_mirrors_every_lossless_preset() {
        let mapped: Vec<LosslessMethod> = Method::value_variants()
            .iter()
            .map(|&m| LosslessMethod::from(m))
            .collect();

        for preset in EXPECTED_PRESETS {
            assert_eq!(
                mapped.iter().filter(|&m| m == preset).count(),
                1,
                "preset {preset:?} must be covered by exactly one cli::Method",
            );
        }
        assert_eq!(mapped.len(), EXPECTED_PRESETS.len());
    }
}
