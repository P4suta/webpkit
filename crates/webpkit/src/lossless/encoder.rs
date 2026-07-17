//! Public encoding configuration: the effort [`Effort`] and the [`EncoderConfig`]
//! that also carries the metadata to embed.

use crate::Effort;
use crate::image::Metadata;
use crate::lossless::histogram;
use crate::lossless::prelude::*;
use crate::lossless::vp8l;

/// Encode `argb` (native ARGB, `width * height` pixels, row-major) as a raw VP8L
/// payload at `effort`.
///
/// Every effort routes through the breadth-parameterized Tier 3 search
/// ([`vp8l::encode::encode_best_at`]), so the spatial predictor, palette,
/// cross-color and subtract-green transforms are always evaluated — no path skips
/// them. An explicit [`Effort::level`] fixes the breadth (`0..=9`); [`Effort::AUTO`]
/// picks one from a cheap content + pixel-count pre-analysis
/// ([`histogram::auto_level`]). The transform-free floor stays in every candidate
/// set and the winner is ranked by real emitted bytes, so the result is never
/// larger than the plain LZ77 floor at any effort.
pub(crate) fn encode_payload(effort: Effort, width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
    let level = effort
        .explicit_level()
        .unwrap_or_else(|| histogram::auto_level(width, argb));
    vp8l::encode::encode_best_at(level, width, height, argb)
}

/// How [`crate::lossless::encode_image`] treats the metadata inherited from the source
/// [`crate::Image`] — re-exported from the core shell, the single home of the
/// fold logic ([`Metadata::resolve`]).
pub use crate::image::MetadataPolicy;

/// Configuration for [`crate::lossless::encode`]: the effort method and any sidecar
/// metadata to embed in an extended (`VP8X`) container.
#[derive(Clone, Debug, Default)]
pub struct EncoderConfig {
    pub(crate) effort: Effort,
    pub(crate) metadata: Metadata,
    pub(crate) policy: MetadataPolicy,
    pub(crate) near_lossless: Option<u8>,
}

impl EncoderConfig {
    /// Default configuration: [`Effort::AUTO`], no metadata.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the effort tier.
    #[must_use]
    pub const fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set the metadata to embed (upgrades the output to a `VP8X` container).
    ///
    /// Under [`crate::lossless::encode_image`] this is a per-field *override*: any field set
    /// here wins over the image's own metadata and survives even
    /// [`MetadataPolicy::StripPrivate`]. The policy in
    /// [`with_metadata_policy`](Self::with_metadata_policy) gates only the
    /// *inherited* image metadata, not what is set here.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set the [`MetadataPolicy`] consulted by [`crate::lossless::encode_image`]. Ignored by
    /// [`crate::lossless::encode`], which has no source image to inherit metadata from.
    ///
    /// The policy gates only the *inherited* image metadata; an explicit value set
    /// via [`with_metadata`](Self::with_metadata) still wins over it.
    #[must_use]
    pub const fn with_metadata_policy(mut self, policy: MetadataPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Enable near-lossless preprocessing at `level` (`0..=100`, lower = stronger
    /// quantization; `100` is a no-op). A lossy encode-side filter that snaps the low
    /// bits of pixels in busy regions to a coarser grid — trading a bounded
    /// per-channel error for a smaller VP8L payload — while leaving the bitstream
    /// exact, so the file still decodes without any special support.
    #[must_use]
    pub const fn with_near_lossless(mut self, level: u8) -> Self {
        self.near_lossless = Some(level);
        self
    }

    /// Fold the image's `inherited` metadata, this config's [`MetadataPolicy`], and
    /// any explicit [`with_metadata`](Self::with_metadata) override into the
    /// effective metadata to embed. Delegates to the shared [`Metadata::resolve`]
    /// fold (the config's own metadata is the per-field override).
    pub(crate) fn resolve_metadata(&self, inherited: &Metadata) -> Metadata {
        self.metadata.resolve(inherited, self.policy)
    }
}

#[cfg(test)]
mod tests {
    use super::{EncoderConfig, MetadataPolicy, encode_payload};
    use crate::Effort;
    use crate::image::Metadata;

    #[test]
    fn default_is_auto_no_metadata() {
        let config = EncoderConfig::new();
        assert_eq!(config.effort, Effort::AUTO);
        assert!(config.metadata.is_empty());
    }

    #[test]
    fn explicit_level_routes_through_encode_best_at() {
        // An explicit level dispatches straight to the breadth-parameterized Tier 3
        // search at that level; the transform families always run, so even level 0
        // evaluates the spatial predictor (no path skips it).
        use crate::lossless::vp8l::encode::encode_best_at;
        let argb = [0xff10_2030u32, 0xff40_5060, 0xff70_8090, 0xffa0_b0c0];
        for level in [0u8, 4, 9] {
            assert_eq!(
                encode_payload(Effort::level(level), 2, 2, &argb),
                encode_best_at(level, 2, 2, &argb),
                "level {level} must route to encode_best_at({level})"
            );
        }
    }

    #[test]
    fn auto_selects_a_level_and_never_regresses_the_floor() {
        // AUTO resolves to the pre-analysis level and, like every level, keeps the
        // transform-free floor as a candidate, so it is never larger than a plain
        // `encode`. A smooth gradient must reach a deep (Best-class) breadth.
        use crate::lossless::histogram::auto_level;
        use crate::lossless::vp8l::encode::{encode, encode_best_at};
        let gradient: Vec<u32> = (0..256u32)
            .map(|i| {
                let v = (i % 16 + i / 16) * 8;
                0xff00_0000 | (v << 16) | (v << 8) | v
            })
            .collect();
        let level = auto_level(16, &gradient);
        assert_eq!(
            encode_payload(Effort::AUTO, 16, 16, &gradient),
            encode_best_at(level, 16, 16, &gradient),
        );
        assert!(
            encode_payload(Effort::AUTO, 16, 16, &gradient).len()
                <= encode(16, 16, &gradient).len()
        );
        assert!(
            level >= 6,
            "a smooth gradient must earn a deep breadth, got {level}"
        );
    }

    #[test]
    fn auto_default_beats_the_old_transform_free_baseline() {
        // The keystone of the effort collapse: the AUTO default now always runs the
        // spatial transforms, so it is dramatically smaller than the historical
        // transform-free `Fast` path (literal + subtract-green only) — the structural
        // end of the weak lossless default. Proven here against that old baseline for
        // a smooth gradient (predictor-friendly) and a low-frequency "photo".
        use crate::lossless::vp8l::encode::encode_with;
        // A 16x16 planar gradient: the old baseline bloats it; AUTO collapses it to
        // Best class (the predictor zeroes the residual) — a many-to-one win.
        let gradient: Vec<u32> = (0..256u32)
            .map(|i| {
                let v = (i % 16 + i / 16) * 8;
                0xff00_0000 | (v << 16) | (v << 8) | v
            })
            .collect();
        let auto = encode_payload(Effort::AUTO, 16, 16, &gradient).len();
        let old_fast = encode_with(16, 16, &gradient, false).len();
        assert!(
            auto * 4 < old_fast,
            "gradient AUTO must crush the transform-free baseline: {auto} vs {old_fast}"
        );
        assert!(
            auto < 200,
            "gradient AUTO must be Best class, got {auto} bytes"
        );
        // A low-frequency color "photo": smoothly varying channels the predictor and
        // cross-color still shrink below the transform-free literal/SG baseline.
        let photo: Vec<u32> = (0..1024u32)
            .map(|i| {
                let (x, y) = (i % 32, i / 32);
                let (red, green, blue) = ((x * 3 + y) & 0xff, (x + y * 2) & 0xff, (x + y) & 0xff);
                0xff00_0000 | (red << 16) | (green << 8) | blue
            })
            .collect();
        let auto_photo = encode_payload(Effort::AUTO, 32, 32, &photo).len();
        let old_fast_photo = encode_with(32, 32, &photo, false).len();
        assert!(
            auto_photo < old_fast_photo,
            "photo AUTO must beat the transform-free baseline: {auto_photo} vs {old_fast_photo}"
        );
    }

    #[test]
    fn builder_sets_fields() {
        let config = EncoderConfig::new()
            .with_effort(Effort::level(0))
            .with_metadata(Metadata {
                exif: Some(vec![1, 2, 3]),
                ..Metadata::none()
            });
        assert_eq!(config.effort, Effort::level(0));
        assert_eq!(config.metadata.exif.as_deref(), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn default_policy_is_preserve() {
        assert_eq!(EncoderConfig::new().policy, MetadataPolicy::Preserve);
    }

    #[test]
    fn resolve_metadata_delegates_to_core() {
        // The config forwards its own metadata (as the per-field override) and its
        // policy to the shared `Metadata::resolve` fold; the fold's truth table is
        // owned and exhaustively tested in the core shell.
        let inherited = Metadata {
            icc_profile: Some(vec![1]),
            exif: Some(vec![2]),
            xmp: Some(vec![3]),
        };
        let override_meta = Metadata {
            exif: Some(vec![9]),
            ..Metadata::none()
        };
        let config = EncoderConfig::new()
            .with_metadata(override_meta.clone())
            .with_metadata_policy(MetadataPolicy::StripPrivate);
        assert_eq!(
            config.resolve_metadata(&inherited),
            override_meta.resolve(&inherited, MetadataPolicy::StripPrivate),
        );
    }
}
