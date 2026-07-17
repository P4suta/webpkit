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

/// A named anchor on the continuous [`webpkit::Effort`] scale.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(crate) enum Method {
    /// Adapt the search depth to the image's content and size (the default).
    #[default]
    Auto,
    /// Fastest: the shallowest fixed search.
    Fast,
    /// Smallest: the deepest fixed search.
    Best,
}

// Both codecs share one `Effort` dial, so this single conversion covers
// `--lossless` and `--lossy` alike: each preset anchors a point on the `0..=9`
// breadth scale, with `Auto` the adaptive default.
impl From<Method> for webpkit::Effort {
    fn from(method: Method) -> Self {
        match method {
            Method::Auto => Self::AUTO,
            Method::Fast => Self::level(0),
            Method::Best => Self::level(9),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use webpkit::Effort;

    use super::Method;

    /// Each preset anchors a *distinct* point on the shared effort scale, the
    /// default is the adaptive `AUTO`, and no two presets collapse together — a
    /// transposed or flattened mapping trips this test.
    #[test]
    fn presets_anchor_distinct_efforts() {
        assert_eq!(Effort::from(Method::Auto), Effort::AUTO);
        assert_eq!(Effort::from(Method::Fast), Effort::level(0));
        assert_eq!(Effort::from(Method::Best), Effort::level(9));
        assert_eq!(Effort::from(Method::default()), Effort::AUTO);

        let efforts: Vec<Effort> = Method::value_variants()
            .iter()
            .map(|&m| Effort::from(m))
            .collect();
        for (i, &a) in efforts.iter().enumerate() {
            for &b in &efforts[i + 1..] {
                assert_ne!(a, b, "presets must map to distinct efforts");
            }
        }
    }
}
