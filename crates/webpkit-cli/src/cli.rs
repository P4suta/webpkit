//! Command-line argument vocabulary shared by the binaries.

pub(crate) mod brand;
pub(crate) mod cwebp;
pub(crate) mod dwebp;

use clap::ValueEnum;

/// Pixel byte order, mirroring [`webpkit::PixelLayout`].
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(crate) enum Layout {
    /// `R, G, B, A`.
    #[default]
    Rgba8,
    /// `A, R, G, B`.
    Argb8,
    /// `B, G, R, A`.
    Bgra8,
}

impl From<Layout> for webpkit::PixelLayout {
    fn from(layout: Layout) -> Self {
        match layout {
            Layout::Rgba8 => Self::Rgba8,
            Layout::Argb8 => Self::Argb8,
            Layout::Bgra8 => Self::Bgra8,
        }
    }
}

/// Encoder effort, mirroring [`webpkit::Effort`].
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(crate) enum Method {
    /// Fastest: literal + subtract-green only.
    Fast,
    /// Balanced (the default): LZ77 + color cache.
    #[default]
    Balanced,
    /// Smallest: adds Tier 3 forward transforms and meta-Huffman on top of Balanced.
    Best,
}

// Both codecs share one `Effort` dial, so this single conversion covers
// `--lossless` and `--lossy` alike.
impl From<Method> for webpkit::Effort {
    fn from(method: Method) -> Self {
        match method {
            Method::Fast => Self::Fast,
            Method::Balanced => Self::Balanced,
            Method::Best => Self::Best,
        }
    }
}

// `Effort` is a closed set, so this exhaustive match is the drift gate: a preset
// added to `webpkit` fails to compile here until `Method` mirrors it.
impl From<webpkit::Effort> for Method {
    fn from(effort: webpkit::Effort) -> Self {
        match effort {
            webpkit::Effort::Fast => Self::Fast,
            webpkit::Effort::Balanced => Self::Balanced,
            webpkit::Effort::Best => Self::Best,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use webpkit::Effort as LosslessMethod;

    use super::Method;

    /// Every [`Method`] maps to a distinct preset and survives the round trip, so
    /// a collapsed or transposed mapping trips this test. Coverage of *new*
    /// presets is a compile error at `From<Effort> for Method`, not a case here.
    #[test]
    fn cli_method_mirrors_every_lossless_preset() {
        let mapped: Vec<LosslessMethod> = Method::value_variants()
            .iter()
            .map(|&m| LosslessMethod::from(m))
            .collect();

        for &effort in &mapped {
            assert_eq!(
                mapped.iter().filter(|&&m| m == effort).count(),
                1,
                "preset {effort:?} must be covered by exactly one cli::Method",
            );
            assert_eq!(
                LosslessMethod::from(Method::from(effort)),
                effort,
                "{effort:?} must survive Effort -> Method -> Effort",
            );
        }
    }
}
