//! Thin glue over the codec public API, shared by every binary.
//!
//! Both decode and encode route through the umbrella `webpkit` crate: [`decode`]
//! inspects the container and handles **either** VP8L (lossless) or VP8 (lossy)
//! input, and a single [`EncodeMode`] selects the encoder.

use webpkit::{DecodeOptions, Effort, Encoder, Image, Metadata, PixelLayout};

use crate::error::CliError;

/// Which codec (and its knobs) [`encode`] should use.
///
/// The three shared binaries build this once from their own flag grammar and hand
/// it to [`encode`], so the lossless/lossy fork lives in exactly one place.
#[derive(Debug, Clone, Copy)]
pub enum EncodeMode {
    /// Lossless VP8L at the given effort [`Effort`].
    Lossless(Effort),
    /// Lossy VP8 at the given quality (`0..=100`) and effort [`Effort`].
    Lossy {
        /// Encode quality, higher = larger and closer to the source.
        quality: u8,
        /// Encoder effort tier.
        method: Effort,
    },
}

/// Decode any still WebP file — lossless (`VP8L`) or lossy (`VP8 `) — into an
/// [`Image`] with the requested output `layout`, dispatching on the container.
///
/// # Errors
///
/// [`CliError::Codec`] if the input is not a decodable still WebP image.
pub fn decode(bytes: &[u8], layout: PixelLayout) -> Result<Image, CliError> {
    let options = DecodeOptions::default().layout(layout);
    Ok(webpkit::decode_with(bytes, &options)?)
}

/// Encode an [`Image`] into a complete WebP file — lossless or lossy per `mode` —
/// embedding exactly the given `metadata` (an empty [`Metadata`] yields a bare
/// `VP8L`/`VP8 ` file).
///
/// Uses the bare-[`ImageRef`](webpkit::ImageRef) [`Encoder::encode_ref`] path so
/// the CLI's per-field `-metadata` selection is honored precisely (the encoder's
/// metadata is embedded verbatim, with no policy-based inheritance from the image).
///
/// # Errors
///
/// [`CliError::Codec`] if the image is invalid (out-of-range dimensions or a
/// buffer-length mismatch).
pub fn encode(image: &Image, mode: EncodeMode, metadata: Metadata) -> Result<Vec<u8>, CliError> {
    let bytes = match mode {
        EncodeMode::Lossless(effort) => Encoder::lossless()
            .effort(effort)
            .metadata(metadata)
            .encode_ref(image.as_image_ref())?,
        EncodeMode::Lossy { quality, method } => Encoder::lossy()
            .quality(quality)
            .effort(method)
            .metadata(metadata)
            .encode_ref(image.as_image_ref())?,
    };
    Ok(bytes)
}
