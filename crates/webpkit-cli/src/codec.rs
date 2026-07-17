//! Thin glue over the codec public API, shared by every binary.
//!
//! Both decode and encode route through the umbrella `webpkit` crate: [`decode`]
//! inspects the container and handles **either** VP8L (lossless) or VP8 (lossy)
//! input, and a single [`EncodeMode`] selects the encoder.

use webpkit::{DecodeOptions, Effort, Encoder, Image, LossyTuning, Metadata, PixelLayout};

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
    /// Lossless VP8L at the given effort [`Effort`], with optional near-lossless
    /// preprocessing (`0..=100`, lower = stronger; `None` is plain lossless).
    Lossless {
        /// Encoder effort tier.
        effort: Effort,
        /// Near-lossless level, or `None` for exact lossless.
        near_lossless: Option<u8>,
    },
    /// Lossy VP8 at the given quality (`0..=100`) and effort [`Effort`].
    Lossy {
        /// Encode quality, higher = larger and closer to the source.
        quality: u8,
        /// Encoder effort tier.
        method: Effort,
        /// Psychovisual tuning knobs (SNS, segments, filter strength/sharpness).
        tuning: LossyTuning,
    },
}

impl EncodeMode {
    /// The encoder effort this mode selects, either codec.
    #[cfg_attr(
        not(feature = "formats"),
        expect(
            dead_code,
            reason = "only the formats-gated GIF animation path reads the effort tier"
        )
    )]
    #[must_use]
    pub(crate) const fn effort(self) -> Effort {
        match self {
            Self::Lossless { effort, .. } | Self::Lossy { method: effort, .. } => effort,
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
    /// `--near-lossless N` was passed (implies lossless).
    pub(crate) near_lossless: Option<u8>,
}

/// Resolve an [`EncodeMode`], returning whether the codec was source-derived.
///
/// `--lossless`/`--near-lossless`/`--lossy`/`--quality` force the codec. Absent all
/// of them, the codec is derived from the source: a JPEG (already lossy) re-encodes
/// lossy at q75, and everything else stays lossless. The returned `bool` is true
/// when derived, so a caller can report the reason.
///
/// # Errors
///
/// [`CliError::Usage`] if `--lossless`/`--near-lossless` is combined with
/// `--lossy`/`--quality`.
pub(crate) fn resolve_mode(
    format: InputFormat,
    flags: CodecFlags,
) -> Result<(EncodeMode, bool), CliError> {
    // Near-lossless is a lossless-only preprocessing step, so it forces the lossless
    // codec just as `--lossless` does.
    let force_lossless = flags.lossless || flags.near_lossless.is_some();
    if force_lossless && (flags.lossy || flags.quality.is_some()) {
        return Err(CliError::Usage(
            "`--lossless`/`--near-lossless` cannot be combined with `--lossy`/`--quality`"
                .to_owned(),
        ));
    }
    if force_lossless {
        return Ok((
            EncodeMode::Lossless {
                effort: flags.effort,
                near_lossless: flags.near_lossless,
            },
            false,
        ));
    }
    if flags.lossy || flags.quality.is_some() {
        return Ok((
            EncodeMode::Lossy {
                quality: flags.quality.unwrap_or(75),
                method: flags.effort,
                tuning: LossyTuning::default(),
            },
            false,
        ));
    }
    let mode = if format == InputFormat::Jpeg {
        EncodeMode::Lossy {
            quality: 75,
            method: flags.effort,
            tuning: LossyTuning::default(),
        }
    } else {
        EncodeMode::Lossless {
            effort: flags.effort,
            near_lossless: flags.near_lossless,
        }
    };
    Ok((mode, true))
}

/// Configure the process-wide rayon pool from a `--threads` count.
///
/// `0` (or an absent flag) leaves rayon's default of one worker per core. A
/// positive count builds the single global pool that both the parallel bulk
/// convert and the encoder's parallel deep-effort search then draw from, so one
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
        EncodeMode::Lossless {
            effort,
            near_lossless,
        } => {
            let encoder = Encoder::lossless().effort(effort).metadata(metadata);
            let encoder = match near_lossless {
                Some(level) => encoder.near_lossless(level),
                None => encoder,
            };
            encoder.encode_ref(image.as_image_ref())?
        },
        EncodeMode::Lossy {
            quality,
            method,
            tuning,
        } => Encoder::lossy()
            .quality(quality)
            .effort(method)
            .tuning(tuning)
            .metadata(metadata)
            .encode_ref(image.as_image_ref())?,
    };
    Ok(bytes)
}

/// Inter-frame animation optimization (gif2webp `-mixed`/`-kmin`/`-kmax`/`-min_size`),
/// applied when a GIF is transcoded as an animation.
///
/// [`enabled`](Self::enabled) is off by default, which leaves the full-frame GIF
/// output byte-identical to the naive path; the other fields shape the optimizer
/// only once it is on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AnimOptimize {
    /// Encode each frame as a minimal delta against the composited canvas.
    pub(crate) enabled: bool,
    /// Trial-encode each delta lossy and lossless, keeping the smaller (`-mixed`).
    pub(crate) mixed: bool,
    /// Exhaustively search blend/dispose/codec per delta (`-min_size`).
    pub(crate) min_size: bool,
    /// Force a keyframe at least every `kmax` frames (`0` = only the first).
    pub(crate) kmax: u32,
    /// Never place keyframes closer than `kmin` frames apart.
    pub(crate) kmin: u32,
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
/// A GIF becomes an animated WebP — lossless or lossy per `mode`, honoring the
/// GIF's own loop count — when `gif_as_animation` is set (the `webp` tool's
/// behavior). `cwebp`, a still encoder, passes `false` and gets the GIF's first
/// frame as a still. The GIF animation path passes no metadata; a still honors
/// `selection`.
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
    optimize: AnimOptimize,
) -> Result<Encoded, CliError> {
    #[cfg(feature = "formats")]
    if gif_as_animation && format == InputFormat::Gif {
        let (bytes, width, height) = encode_gif_animation(bytes, mode, optimize)?;
        return Ok(Encoded {
            bytes,
            width,
            height,
            animation: true,
        });
    }
    #[cfg(not(feature = "formats"))]
    let _ = (gif_as_animation, optimize);

    let image = format::read_image(bytes, format, None)?;
    let metadata = selection.apply(image.metadata());
    Ok(Encoded {
        width: image.width(),
        height: image.height(),
        bytes: encode(&image, mode, metadata)?,
        animation: false,
    })
}

/// Encode every GIF frame as a frame of an animated WebP, honoring the GIF's own
/// loop count and encoding each frame with `mode`'s codec — lossless `VP8L`, or
/// lossy `VP8 ` when `--lossy`/`--quality` selected it. The GIF path embeds no
/// metadata (though [`AnimationEncoder`] itself supports it via
/// [`AnimationEncoder::metadata`](webpkit::AnimationEncoder::metadata)).
#[cfg(feature = "formats")]
fn encode_gif_animation(
    bytes: &[u8],
    mode: EncodeMode,
    optimize: AnimOptimize,
) -> Result<(Vec<u8>, u32, u32), CliError> {
    use webpkit::{
        AnimCodec, AnimationEncoder, BlendMode, Dimensions, DisposalMode, FrameMeta, ImageRef,
        LossyParams, PixelLayout,
    };

    // Near-lossless has no per-frame animation analog, so a lossless mode yields
    // lossless (`VP8L`) frames regardless of its still-only preprocessing knob.
    let codec = match mode {
        EncodeMode::Lossless { .. } => AnimCodec::Lossless,
        EncodeMode::Lossy {
            quality, tuning, ..
        } => AnimCodec::Lossy {
            params: LossyParams::new(quality).with_tuning(tuning),
        },
    };
    let frames = format::image_input::read_gif_frames(bytes)?;
    let loop_count = format::image_input::read_gif_loop_count(bytes)?;
    let first = frames
        .first()
        .ok_or_else(|| CliError::Format("GIF has no frames".to_owned()))?;
    let (width, height) = (first.image.width(), first.image.height());
    let canvas = Dimensions::new(width, height)?;

    // `--optimize` diffs each full-canvas GIF frame into a minimal delta; the result
    // composites pixel-identically to the naive full-frame animation below, only
    // smaller. Off by default, so the naive path stays byte-identical.
    if optimize.enabled {
        let bytes =
            optimize_gif_frames(&frames, canvas, loop_count, codec, mode.effort(), optimize)?;
        return Ok((bytes, width, height));
    }

    // The `image` crate hands back full-canvas frames with the GIF's own disposal
    // already applied to the pixels (see `read_gif_frames`), so each frame's real
    // per-frame delay is honored via `duration_ms`, and its compositing is a plain
    // full-canvas replace: `Overwrite` (do not alpha-blend over the previous, now
    // stale, canvas — which would leak transparency) and `Keep` (the next frame is
    // itself a full canvas that overwrites everything). Deriving the meta from that
    // invariant, rather than a bare literal, is what keeps a transparent-GIF frame
    // from compositing wrong.
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
        .loop_count(loop_count)
        .codec(codec)
        .effort(mode.effort())
        .add_frame(first_ref, meta_for(first))?;
    for frame in &frames[1..] {
        let frame_ref = ImageRef::new(canvas, PixelLayout::Rgba8, frame.image.as_bytes())?;
        encoder = encoder.add_frame(frame_ref, meta_for(frame))?;
    }
    Ok((encoder.finish(), width, height))
}

/// Assemble the full-canvas GIF `frames` through the inter-frame optimizer, which
/// derives each frame's minimal delta rectangle, blend, dispose, and codec while
/// reproducing every frame exactly.
///
/// The lossy trial that `-mixed` / `-min_size` run uses the mode's own quality when
/// the mode is lossy, else a neutral default — the same quality gif2webp trials with.
#[cfg(feature = "formats")]
fn optimize_gif_frames(
    frames: &[format::image_input::AnimFrame],
    canvas: webpkit::Dimensions,
    loop_count: u16,
    codec: webpkit::AnimCodec,
    effort: Effort,
    optimize: AnimOptimize,
) -> Result<Vec<u8>, CliError> {
    use webpkit::{AnimCodec, AnimationOptimizer, ImageRef, LossyParams, PixelLayout};

    let lossy_params = match codec {
        AnimCodec::Lossy { params } => params,
        _ => LossyParams::new(75),
    };
    let mut it = frames.iter();
    let first = it
        .next()
        .ok_or_else(|| CliError::Format("GIF has no frames".to_owned()))?;
    let first_ref = ImageRef::new(canvas, PixelLayout::Rgba8, first.image.as_bytes())?;
    let mut opt = AnimationOptimizer::new(canvas)
        .loop_count(loop_count)
        .effort(effort)
        .codec(codec)
        .lossy_params(lossy_params)
        .mixed(optimize.mixed)
        .min_size(optimize.min_size)
        .keyframe_interval(optimize.kmin, optimize.kmax)
        .add_frame(first_ref, first.duration_ms)?;
    for frame in it {
        let frame_ref = ImageRef::new(canvas, PixelLayout::Rgba8, frame.image.as_bytes())?;
        opt = opt.add_frame(frame_ref, frame.duration_ms)?;
    }
    Ok(opt.optimize()?)
}
