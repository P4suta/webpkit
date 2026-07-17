//! Thin glue over the codec public API, shared by every binary.
//!
//! Both decode and encode route through the umbrella `webpkit` crate: [`decode`]
//! inspects the container and handles **either** VP8L (lossless) or VP8 (lossy)
//! input, and a single [`EncodeMode`] selects the encoder.

use webpkit::{DecodeOptions, Effort, Encoder, Image, Metadata, PixelLayout};

use crate::{
    error::CliError,
    format::{self, InputFormat},
    metadata::Selection,
};

/// Which codec (and its knobs) [`encode`] should use.
///
/// The three shared binaries build this once from their own flag grammar and hand
/// it to [`encode`], so the lossless/lossy fork lives in exactly one place.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EncodeMode {
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

impl EncodeMode {
    /// The encoder effort this mode selects, either codec.
    #[must_use]
    pub(crate) const fn effort(self) -> Effort {
        match self {
            Self::Lossless(effort) | Self::Lossy { method: effort, .. } => effort,
        }
    }
}

/// The user's codec choice before it is resolved against a source format.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodecFlags {
    /// `--lossless` was passed.
    pub(crate) lossless: bool,
    /// `--lossy` was passed.
    pub(crate) lossy: bool,
    /// `--quality N` was passed (also selects lossy).
    pub(crate) quality: Option<u8>,
    /// Encoder effort.
    pub(crate) effort: Effort,
}

/// Resolve an [`EncodeMode`], returning whether the codec was source-derived.
///
/// `--lossless`/`--lossy`/`--quality` force the codec. Absent all three, the codec
/// is derived from the source: a JPEG (already lossy) re-encodes lossy at q75, and
/// everything else stays lossless. The returned `bool` is true when derived, so a
/// caller can report the reason.
///
/// # Errors
///
/// [`CliError::Usage`] if `--lossless` is combined with `--lossy`/`--quality`.
pub(crate) fn resolve_mode(
    format: InputFormat,
    flags: CodecFlags,
) -> Result<(EncodeMode, bool), CliError> {
    if flags.lossless && (flags.lossy || flags.quality.is_some()) {
        return Err(CliError::Usage(
            "`--lossless` cannot be combined with `--lossy`/`--quality`".to_owned(),
        ));
    }
    if flags.lossless {
        return Ok((EncodeMode::Lossless(flags.effort), false));
    }
    if flags.lossy || flags.quality.is_some() {
        return Ok((
            EncodeMode::Lossy {
                quality: flags.quality.unwrap_or(75),
                method: flags.effort,
            },
            false,
        ));
    }
    let mode = if format == InputFormat::Jpeg {
        EncodeMode::Lossy {
            quality: 75,
            method: flags.effort,
        }
    } else {
        EncodeMode::Lossless(flags.effort)
    };
    Ok((mode, true))
}

/// Configure the process-wide rayon pool from a `--threads` count.
///
/// `0` (or an absent flag) leaves rayon's default of one worker per core. A
/// positive count builds the single global pool that both the parallel bulk
/// convert and the encoder's parallel `Effort::Best` search then draw from, so one
/// flag bounds every layer of parallelism. Building the global pool can only fail
/// if one already exists; this runs once at startup, so that case is ignored.
pub(crate) fn configure_threads(threads: Option<u16>) {
    if let Some(count) = threads.filter(|&n| n > 0) {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(usize::from(count))
            .build_global();
    }
}

/// Whether these bytes are a WebP file (`RIFF....WEBP`), by content not extension.
#[must_use]
pub(crate) fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
}

/// Build [`DecodeOptions`] for `layout` with the decode pixel cap.
///
/// `None` opts out of the cap ([`DecodeOptions::unbounded`]); `Some(n)` caps the
/// canvas at `n` pixels. The single place the CLI's `max_pixels` setting becomes
/// codec options, so the two decode paths cannot disagree.
pub(crate) fn decode_options(layout: PixelLayout, max_pixels: Option<u64>) -> DecodeOptions {
    let options = DecodeOptions::new().layout(layout);
    match max_pixels {
        Some(max) => options.max_pixels(max),
        None => options.unbounded(),
    }
}

/// Decode a still WebP, or the first composited frame of an animation, to an
/// [`Image`] in RGBA8. The decode is bounded by `max_pixels` (`None` opts out).
///
/// # Errors
///
/// [`CliError::Codec`] if the bytes are not a decodable WebP, or
/// [`CliError::Format`] if an animation carries no frames.
pub(crate) fn decode_still_or_first_frame(
    bytes: &[u8],
    max_pixels: Option<u64>,
) -> Result<Image, CliError> {
    let options = decode_options(PixelLayout::Rgba8, max_pixels);
    if webpkit::is_animated(bytes)? {
        let frames = webpkit::decode_frames_with(bytes, &options)?;
        let first = frames
            .composited()
            .next()
            .ok_or_else(|| CliError::Format("animation has no frames".to_owned()))??;
        return Ok(first.into_image());
    }
    Ok(webpkit::decode_with(bytes, &options)?)
}

/// Decode any still WebP file — lossless (`VP8L`) or lossy (`VP8 `) — into an
/// [`Image`] with the requested output `layout`, dispatching on the container. The
/// decode is bounded by `max_pixels` (`None` opts out).
///
/// # Errors
///
/// [`CliError::Codec`] if the input is not a decodable still WebP image.
pub(crate) fn decode(
    bytes: &[u8],
    layout: PixelLayout,
    max_pixels: Option<u64>,
) -> Result<Image, CliError> {
    Ok(webpkit::decode_with(
        bytes,
        &decode_options(layout, max_pixels),
    )?)
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
pub(crate) fn encode(
    image: &Image,
    mode: EncodeMode,
    metadata: Metadata,
) -> Result<Vec<u8>, CliError> {
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

/// The result of encoding one input file: the WebP bytes, the source dimensions,
/// and whether the codec produced an animation (a GIF encoded as `ANIM`).
pub(crate) struct Encoded {
    /// The complete WebP file.
    pub(crate) bytes: Vec<u8>,
    /// Canvas / image width in pixels.
    pub(crate) width: u32,
    /// Canvas / image height in pixels.
    pub(crate) height: u32,
    /// True when the output is an animated WebP (GIF path).
    pub(crate) animation: bool,
}

/// Encode an already-read input into a WebP file, choosing the still or the GIF
/// animation path from `format`.
///
/// A GIF becomes an animated WebP (lossless, one `VP8L` frame each) when
/// `gif_as_animation` is set — the `webp` tool's behavior. `cwebp`, a still
/// encoder, passes `false` and gets the GIF's first frame as a still. Animation
/// carries no metadata (the encoder does not model it); a still honors `selection`.
///
/// # Errors
///
/// [`CliError::Format`] for a malformed input, or [`CliError::Codec`] if the image
/// is out of range for WebP.
pub(crate) fn encode_input(
    bytes: &[u8],
    format: InputFormat,
    mode: EncodeMode,
    selection: Selection,
    gif_as_animation: bool,
) -> Result<Encoded, CliError> {
    #[cfg(feature = "formats")]
    if gif_as_animation && format == InputFormat::Gif {
        let (bytes, width, height) = encode_gif_animation(bytes, mode.effort())?;
        return Ok(Encoded {
            bytes,
            width,
            height,
            animation: true,
        });
    }
    #[cfg(not(feature = "formats"))]
    let _ = gif_as_animation;

    let image = format::read_image(bytes, format, None)?;
    let metadata = selection.apply(image.metadata());
    Ok(Encoded {
        width: image.width(),
        height: image.height(),
        bytes: encode(&image, mode, metadata)?,
        animation: false,
    })
}

/// Encode every GIF frame as a lossless `VP8L` frame of an animated WebP that
/// loops forever. Metadata is not carried — the animation encoder does not model
/// sidecar chunks.
#[cfg(feature = "formats")]
fn encode_gif_animation(bytes: &[u8], effort: Effort) -> Result<(Vec<u8>, u32, u32), CliError> {
    use webpkit::{
        AnimationEncoder, BlendMode, Dimensions, DisposalMode, FrameMeta, ImageRef, PixelLayout,
    };

    let frames = format::image_input::read_gif_frames(bytes)?;
    let first = frames
        .first()
        .ok_or_else(|| CliError::Format("GIF has no frames".to_owned()))?;
    let (width, height) = (first.image.width(), first.image.height());
    let canvas = Dimensions::new(width, height)?;

    let meta_for = |frame: &format::image_input::AnimFrame| {
        FrameMeta::new(
            0,
            0,
            canvas,
            frame.duration_ms,
            BlendMode::Overwrite,
            DisposalMode::Keep,
        )
    };

    let first_ref = ImageRef::new(canvas, PixelLayout::Rgba8, first.image.as_bytes())?;
    let mut encoder = AnimationEncoder::new(canvas)
        .with_loop_count(0)
        .with_effort(effort)
        .add_frame(first_ref, meta_for(first))?;
    for frame in &frames[1..] {
        let frame_ref = ImageRef::new(canvas, PixelLayout::Rgba8, frame.image.as_bytes())?;
        encoder = encoder.add_frame(frame_ref, meta_for(frame))?;
    }
    Ok((encoder.finish(), width, height))
}
