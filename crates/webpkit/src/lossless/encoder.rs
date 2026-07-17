//! Public encoding configuration: the effort [`Effort`] and the [`EncoderConfig`]
//! that also carries the metadata to embed.

use crate::Effort;
use crate::image::Metadata;
use crate::lossless::prelude::*;
use crate::lossless::vp8l;

/// Encode `argb` (native ARGB, `width * height` pixels, row-major) as a raw VP8L
/// payload at `effort`: [`Effort::Fast`] emits only the literal + subtract-green
/// tiers, [`Effort::Balanced`] runs the full Tier 0/1/2 LZ77 + color-cache search,
/// and [`Effort::Best`] additionally tries the forward-transform families and keeps
/// the smallest. Only `Best` diverges from `Balanced`, so the `Balanced`/`Fast`
/// bytes are unchanged from the earlier tiers.
pub(crate) fn encode_payload(effort: Effort, width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
    match effort {
        Effort::Fast => vp8l::encode::encode_with(width, height, argb, false),
        Effort::Balanced => vp8l::encode::encode(width, height, argb),
        Effort::Best => vp8l::encode::encode_best(width, height, argb),
    }
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
    /// Default configuration: [`Effort::Balanced`], no metadata.
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
    fn default_is_balanced_no_metadata() {
        let config = EncoderConfig::new();
        assert_eq!(config.effort, Effort::Balanced);
        assert!(config.metadata.is_empty());
    }

    #[test]
    fn effort_routes_to_the_expected_encoder() {
        // Each effort tier dispatches to its VP8L encoder entry point: Fast to the
        // literal-only path, Balanced to the Tier 2 search, Best to the tiered
        // transform search. Best must never be larger than Balanced (it keeps the
        // Balanced result as its floor).
        let argb = [0xff10_2030u32, 0xff40_5060, 0xff70_8090, 0xffa0_b0c0];
        assert_eq!(
            encode_payload(Effort::Fast, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode_with(2, 2, &argb, false),
        );
        assert_eq!(
            encode_payload(Effort::Balanced, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode(2, 2, &argb),
        );
        assert_eq!(
            encode_payload(Effort::Best, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode_best(2, 2, &argb),
        );
        assert!(
            encode_payload(Effort::Best, 2, 2, &argb).len()
                <= encode_payload(Effort::Balanced, 2, 2, &argb).len()
        );
    }

    #[test]
    fn builder_sets_fields() {
        let config = EncoderConfig::new()
            .with_effort(Effort::Fast)
            .with_metadata(Metadata {
                exif: Some(vec![1, 2, 3]),
                ..Metadata::none()
            });
        assert_eq!(config.effort, Effort::Fast);
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
