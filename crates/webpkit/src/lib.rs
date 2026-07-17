//! `webpkit` — a pure-Rust WebP codec: lossless (VP8L) and lossy (VP8) behind one API.
//!
//! This is the umbrella crate. [`decode`] reads any still WebP file, inspecting
//! the container to route `VP8L` payloads to the lossless decoder and
//! `VP8 ` payloads to the lossy decoder; both return the shared
//! [`Image`] type. A lossy image's sibling `ALPH` alpha chunk is composited here,
//! where both codecs are in scope. The type-state [`Encoder`] writes output with
//! either codec — [`Encoder::lossless`] or [`Encoder::lossy`] — sharing the
//! effort/metadata knobs (only the lossy builder has a `quality`). The container
//! framing, image model, and error type are defined once in [`crate`] and
//! re-exported here.
//!
//! Like the codec crates it wraps, this crate forbids `unsafe`, has zero required
//! runtime dependencies, and targets `no_std` (with `alloc`).
//!
//! # Status
//!
//! Lossless decode/encode is complete. Lossy (VP8) decoding
//! reconstructs baseline key frames, composites a separate
//! `ALPH` alpha channel, and decodes lossy animations (frames dispatched into
//! the lossless codec's compositor). The unified [`IncrementalDecoder`] streams any still or
//! animation, row-streaming a bare lossy `VP8 ` still. Lossy
//! **encoding** ([`Encoder::lossy`]) writes a baseline `VP8 ` key frame, carrying a
//! lossless `ALPH` alpha plane for non-opaque images and ICC/Exif/XMP [`Metadata`]
//! via the extended `VP8X` container ([`Encoder::encode`] preserves a source
//! [`Image`]'s metadata by default). [`read_metadata`]/[`write_metadata`] inspect and
//! rewrite that sidecar metadata without touching the image bitstream (the
//! `webpmux` half), [`decode_yuv`] recovers a lossy still's native YUV 4:2:0 planes,
//! and [`Encoder::lossless`]'s `near_lossless` preprocessing trades a bounded
//! per-channel error for a smaller VP8L payload.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]
#![deny(
    clippy::float_arithmetic,
    reason = "the codecs must be bit-deterministic across platforms; floating-point \
              rounding is not portable. Use fixed-point integer math."
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is our honest internal visibility; this nursery lint conflicts \
              with the rustc unreachable_pub lint that we also enable"
)]
// Without the `__internals` tooling feature the internal module tree is `pub(crate)`
// (see `internal_modules!` below). Its cross-module `pub` items are then no longer
// reachable from the crate root (`unreachable_pub`), and the items that exist only
// for the external tooling/test crates read as unused from webpkit's own side
// (`dead_code`). Both are the intended shape of a published build: the curated
// facade is the whole public API, and the tooling surface is exercised only under
// `__internals` (by the bench/oracle/conformance crates), never from within the lib.
#![cfg_attr(
    not(feature = "__internals"),
    allow(
        unreachable_pub,
        dead_code,
        unused_imports,
        reason = "internal modules are pub(crate) unless the `__internals` tooling \
                  feature is on; their cross-module `pub` items (and the `pub use` \
                  re-exports of them) are then unreachable externally and unused \
                  internally by design"
    )
)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(not(feature = "alloc"))]
compile_error!("webpkit requires an allocator: enable the `alloc` feature (implied by `std`)");

#[cfg(feature = "alloc")]
use alloc::boxed::Box;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

// ---- module tree ------------------------------------------------------------
// The bitstream-agnostic shell (formerly the `webpkit-core` crate): container
// framing, image model, error type, streaming vocabulary. Flattened at the crate
// root so its `crate::`-relative code is unchanged. NOT named `core` — that would
// shadow the `::core` std crate the no_std codecs use throughout.
//
// These shell and codec modules are `pub(crate)` in a normal build: the curated
// re-exports below are the *entire* public API. Under the non-default `__internals`
// feature they become `#[doc(hidden)] pub` so this workspace's own test & tooling
// crates can reach into them module-first (they used to be separate public crates).
// That feature is off on crates.io / docs.rs, so a downstream user never sees — and
// can never depend on — the internals. (`encoder`/`interop`/`prelude` stay private
// regardless: their public items are already re-exported through the facade.)
//
// The default-build declarations are literal `pub(crate) mod` items so the `syn`
// source walk in cargo-mutants descends into every codec file — a macro-hidden mod
// tree is invisible to the mutation gate and would silently leave the whole codec
// unmutated. The `__internals` re-exposure is instead macro-generated: it is only
// active under a feature the mutation sweep never builds, and routing it through a
// macro keeps the walk from seeing a *second* declaration of each file (which would
// double every codec mutant). So the sweep sees exactly one copy per module.
#[cfg(not(feature = "__internals"))]
pub(crate) mod alpha;
#[cfg(not(feature = "__internals"))]
pub(crate) mod anim;
#[cfg(not(feature = "__internals"))]
pub(crate) mod container;
#[cfg(not(feature = "__internals"))]
pub(crate) mod effort;
#[cfg(not(feature = "__internals"))]
pub(crate) mod error;
#[cfg(not(feature = "__internals"))]
pub(crate) mod geometry;
#[cfg(not(feature = "__internals"))]
pub(crate) mod image;
#[cfg(not(feature = "__internals"))]
pub(crate) mod lossless;
#[cfg(not(feature = "__internals"))]
pub(crate) mod lossy;
#[cfg(not(feature = "__internals"))]
pub(crate) mod mux;
#[cfg(not(feature = "__internals"))]
pub(crate) mod optimize;
#[cfg(not(feature = "__internals"))]
pub(crate) mod stream;
#[cfg(all(not(feature = "__internals"), feature = "work-count"))]
pub(crate) mod work_count;
#[cfg(not(feature = "__internals"))]
pub(crate) mod yuv;

#[cfg(feature = "__internals")]
macro_rules! expose_internals {
    ($($(#[$attr:meta])* $name:ident),* $(,)?) => { $(
        #[doc(hidden)]
        $(#[$attr])*
        pub mod $name;
    )* };
}

#[cfg(feature = "__internals")]
expose_internals! {
    alpha,
    anim,
    container,
    effort,
    error,
    geometry,
    image,
    lossless,
    lossy,
    mux,
    optimize,
    stream,
    yuv,
    #[cfg(feature = "work-count")]
    work_count,
}

// Public RIFF chunk-inspection facade (thin wrappers over `container::reader`).
// The module stays private; its public items are re-exported at the crate root.
mod chunk;
// The encode surface is public so the type-state markers keep a namespaced home
// (`webpkit::encoder::Lossless` etc.) instead of crowding the crate root, where
// `webpkit::Lossless` would collide with `webpkit::Codec::Lossless`.
pub mod encoder;
// Optional `image`-crate interop (TryFrom conversions on `Image`). The impls attach
// to the public types, so the module itself stays private.
#[cfg(feature = "image")]
mod interop;
mod prelude;

// ---- public facade re-exports ----------------------------------------------
pub use crate::anim::{AnimInfo, BlendMode, CompositedFrame, DisposalMode, Frame, FrameMeta};
pub use crate::chunk::{Chunk, Chunks, chunks};
pub use crate::effort::Effort;
#[cfg(feature = "std")]
pub use crate::error::IoError;
pub use crate::error::{Codec, Error, Result};
pub use crate::geometry::Rect;
pub use crate::image::{
    Dimensions, Image, ImageRef, MAX_DIMENSION, Metadata, MetadataPolicy, PixelLayout,
};
pub use crate::mux::{AnimationMux, MuxFrame};
pub use crate::optimize::AnimationOptimizer;
pub use crate::stream::{DEFAULT_MAX_PIXELS, DecodeOptions, ImageInfo, Progress, RowDrain};
pub use crate::yuv::YuvImage;
// Only the entry points and the codec selector reach the crate root; the type-state
// markers (`Empty`/`HasFrames`/`Lossless`/`Lossy`) stay in `webpkit::encoder`, named
// only in the rare code that spells out an encoder's type argument.
pub use encoder::{AnimCodec, AnimationEncoder, Encoder};
// The lossy psychovisual tuning surface, exposed at the crate root because it appears
// in the facade `Encoder::<Lossy>::tuning` signature (the `lossy` module itself is
// `pub(crate)` in a normal build).
pub use crate::lossy::{
    AlphaFilterMode, AlphaMethod, Attempt, LossyParams, LossyTuning, Preset, RateSearch, RateTarget,
};

// ---- imports used by the facade functions below -----------------------------
use crate::alpha::{AlphaCompression, parse_header, unfilter};
use crate::container::fourcc::FourCc;
use crate::container::reader::{ImageChunk, read_container};
use crate::container::scan::{declared_len, is_complete, scan_chunks};
use crate::container::vp8x::{VP8X_PAYLOAD_LEN, Vp8xInfo};

/// A lazy per-frame iterator over a WebP animation — each frame decoded by the
/// crate's internal both-codecs frame decoder (lossless `VP8L` or lossy `VP8 `).
#[cfg(feature = "alloc")]
pub type Frames<'a> = crate::anim::Frames<'a, WebpFrameDecoder>;
/// A compositing iterator that paints each animation frame onto the persistent
/// canvas.
#[cfg(feature = "alloc")]
pub type CompositedFrames<'a> = crate::anim::CompositedFrames<'a, WebpFrameDecoder>;

/// Decode a still WebP file — lossless (`VP8L`) or lossy (`VP8 `) — into an
/// [`Image`] (RGBA8 by default), dispatching on the container's image chunk.
///
/// A lossy `VP8 ` image accompanied by a sibling `ALPH` chunk is composited: the
/// opaque RGB is decoded by the lossy decoder, the alpha plane is decompressed (raw, or
/// a lossless `VP8L` stream) and spatially un-filtered, and the result is written
/// into the image's alpha channel. Lossless (`VP8L`) images carry their own alpha.
///
/// An animated file returns its **first composited frame** (matching libwebp's
/// `WebPDecode`); use [`decode_frames`] to walk every frame.
///
/// # Untrusted input
///
/// Safe by default: this caps the canvas at [`DEFAULT_MAX_PIXELS`] before any
/// buffer is allocated, so a hostile header cannot exhaust memory. Raise the cap
/// with [`decode_with`] + [`DecodeOptions::max_pixels`], or remove it for trusted
/// input with [`DecodeOptions::unbounded`].
///
/// # Errors
///
/// [`Error::NotWebp`]/[`Error::Truncated`] for a non-WebP or short input,
/// [`Error::MissingImage`] when the file has no image chunk, or a
/// bitstream/container error from the selected decoder or the `ALPH` alpha stream.
pub fn decode(input: &[u8]) -> Result<Image> {
    decode_with(input, &DecodeOptions::default())
}

/// Decode a still WebP file into an [`Image`] with explicit [`DecodeOptions`]
/// (output layout, pixel limit), dispatching on the container's image chunk.
///
/// Symmetric with the codec crates' `decode_with`: the selected decoder enforces
/// `options.max_pixels` against the peeked header dimensions *before* any pixel or
/// canvas buffer is allocated, and the limit is propagated into a lossy image's
/// `ALPH` alpha decode. A default [`DecodeOptions`] caps at [`DEFAULT_MAX_PIXELS`];
/// call [`DecodeOptions::unbounded`] to lift it for trusted input. An animated file
/// returns its **first composited frame** (matching [`decode`]).
///
/// # Errors
///
/// The same errors as [`decode`], plus [`Error::LimitExceeded`] when
/// `options.max_pixels` is exceeded.
#[cfg(feature = "alloc")]
pub fn decode_with(input: &[u8], options: &DecodeOptions) -> Result<Image> {
    // One container walk yields the image chunk, any sibling `ALPH`, the `VP8X`
    // header, and sidecar metadata — so the file is parsed exactly once (the codec
    // is handed the located payload, not the whole file to re-parse).
    let c = read_container(input, options.read_metadata)?;
    // A clearly-animated file routes to its first composited frame, leaving the
    // still-image path (and its exact error semantics) untouched otherwise.
    if c.animated {
        return first_composited_frame(input, options);
    }
    match c.image.ok_or(Error::MissingImage)? {
        // A `VP8L` image encodes its own alpha, so any sibling `ALPH` does not
        // apply; decode the located payload directly (no container re-parse).
        ImageChunk::Lossless(payload) => {
            let image = crate::lossless::decode_vp8l(payload, options)?;
            // A VP8X canvas, if present, must agree with the decoded dimensions.
            if c.vp8x.is_some_and(|vp8x| vp8x.canvas != image.dimensions()) {
                return Err(Error::InvalidContainer);
            }
            Ok(image.with_metadata(c.metadata))
        },
        ImageChunk::Lossy(payload) => {
            let mut image = crate::lossy::decode_with(payload, options)?;
            if let Some(alph) = c.alpha {
                let plane = alpha_plane_with(alph, image.width(), image.height(), options)?;
                image.apply_alpha_plane(&plane)?;
            }
            // Surface any `VP8X` sidecar metadata (ICCP/EXIF/XMP) so a lossy
            // decode → encode_image round trip preserves it, matching the lossless
            // path (a bare `VP8 ` yields no metadata, leaving the image unchanged).
            Ok(image.with_metadata(c.metadata))
        },
    }
}

/// Decode a still WebP straight to its **RGBA8** pixels and [`Dimensions`].
///
/// The raw-buffer companion to [`decode`], skipping the [`Image`] wrapper for
/// callers that only want the bytes (any embedded metadata is dropped).
///
/// # Errors
///
/// The same errors as [`decode`].
#[cfg(feature = "alloc")]
pub fn decode_rgba(input: &[u8]) -> Result<(Dimensions, Vec<u8>)> {
    let image = decode(input)?;
    Ok((image.dimensions(), image.into_pixels()))
}

/// Decode a **lossy** still WebP straight to its native **YUV 4:2:0** planes.
///
/// A lossy (`VP8 `) image is reconstructed in YUV before the YUV→RGB conversion
/// [`decode`] performs; this hands back those planes — byte-identical to libwebp's
/// `WebPDecodeYUV` — without that final step, for callers that consume YUV
/// directly (video pipelines, `dwebp -yuv`/`-pgm`). See [`YuvImage`] for the plane
/// shapes.
///
/// This is lossy-still only: lossless (`VP8L`) has no YUV form, and an animation is
/// not a single still — both return [`Error::UnsupportedFeature`]. Any sibling
/// `ALPH` alpha is ignored (YUV carries only luma and chroma).
///
/// # Untrusted input
///
/// Safe by default: like [`decode`], the canvas is capped at [`DEFAULT_MAX_PIXELS`]
/// before the reconstruction planes are allocated.
///
/// # Examples
///
/// ```
/// let rgba = vec![128u8; 4 * 4 * 4]; // a 4x4 mid-gray image
/// let webp = webpkit::encode_lossy_rgba(4, 4, &rgba, 90)?;
/// let yuv = webpkit::decode_yuv(&webp)?;
/// assert_eq!((yuv.width(), yuv.height()), (4, 4));
/// assert_eq!(yuv.y().len(), 4 * 4); // luma is full resolution
/// assert_eq!(yuv.u().len(), 2 * 2); // chroma is 4:2:0 subsampled
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] for a lossless or animated input, the same
/// container errors as [`decode`], or a lossy bitstream error.
#[cfg(feature = "alloc")]
pub fn decode_yuv(input: &[u8]) -> Result<YuvImage> {
    let options = DecodeOptions::default();
    let c = read_container(input, false)?;
    // Only a lossy still has a native YUV reconstruction; an animation is not a
    // single still and a lossless image never leaves RGBA.
    if c.animated {
        return Err(Error::UnsupportedFeature);
    }
    match c.image.ok_or(Error::MissingImage)? {
        ImageChunk::Lossy(payload) => crate::lossy::decode_yuv_with(payload, &options),
        ImageChunk::Lossless(_) => Err(Error::UnsupportedFeature),
    }
}

/// Decode a still WebP read from any [`std::io::Read`] source into an [`Image`].
///
/// The reader-based companion to [`decode`] (RGBA8 by default). The whole stream
/// is buffered before decoding; for push-based streaming use
/// [`incremental_decoder`].
///
/// # Errors
///
/// [`Error::Io`] if reading `reader` fails, otherwise the same errors as [`decode`].
#[cfg(feature = "std")]
pub fn decode_reader<R: std::io::Read>(mut reader: R) -> Result<Image> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    decode(&buf)
}

/// Encode an **RGBA8** pixel buffer as a lossless (`VP8L`) WebP file.
///
/// The one-call companion to [`decode`], mirroring [`decode_rgba`] on the encode
/// side. Uses the default [`Effort`]; for a different effort tier, embedded
/// metadata, or a non-RGBA input layout, use the [`Encoder`] builder. `rgba` must
/// be exactly `width * height * 4` bytes.
///
/// # Examples
///
/// ```
/// let rgba = vec![0u8; 4 * 4 * 4]; // a 4x4 RGBA image
/// let webp = webpkit::encode_lossless_rgba(4, 4, &rgba)?;
/// let (dims, pixels) = webpkit::decode_rgba(&webp)?;
/// assert_eq!((dims.width(), dims.height()), (4, 4));
/// assert_eq!(pixels, rgba); // lossless is byte-exact
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::InvalidDimensions`] for a zero or over-large canvas,
/// [`Error::PixelBufferMismatch`] if `rgba`'s length is wrong, otherwise any
/// encode error.
#[cfg(feature = "alloc")]
pub fn encode_lossless_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let image = ImageRef::new(Dimensions::new(width, height)?, PixelLayout::Rgba8, rgba)?;
    Encoder::lossless().encode_ref(image)
}

/// Encode an **RGBA8** pixel buffer as a lossy (`VP8 `) WebP file at `quality`.
///
/// The one-call companion to [`decode`]; `quality` is `0..=100` (clamped). For a
/// different effort tier, embedded metadata, or a non-RGBA input layout, use the
/// [`Encoder`] builder. `rgba` must be exactly `width * height * 4` bytes.
///
/// # Errors
///
/// The same errors as [`encode_lossless_rgba`].
#[cfg(feature = "alloc")]
pub fn encode_lossy_rgba(width: u32, height: u32, rgba: &[u8], quality: u8) -> Result<Vec<u8>> {
    let image = ImageRef::new(Dimensions::new(width, height)?, PixelLayout::Rgba8, rgba)?;
    Encoder::lossy().quality(quality).encode_ref(image)
}

/// Decode an `ALPH` chunk payload (including its 1-byte header) into a
/// `width * height` alpha plane.
///
/// Parses the header, decompresses the plane (raw bytes, or a lossless `VP8L`
/// stream via [`crate::lossless::decode_alpha`]), then reverses its spatial filter. The
/// un-filter is applied identically for both compression methods — the filter is
/// orthogonal to how the plane was stored.
#[cfg(feature = "alloc")]
fn alpha_plane(alph: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    alpha_plane_with(alph, width, height, &DecodeOptions::default())
}

/// Like [`alpha_plane`] but threading `options` so [`decode_with`] can propagate
/// its `max_pixels` limit into the lossless `ALPH` alpha decode (checked before the
/// plane is allocated). The plane is the same size as the already-limited image, so
/// this is defense in depth rather than the primary guard.
#[cfg(feature = "alloc")]
fn alpha_plane_with(
    alph: &[u8],
    width: u32,
    height: u32,
    options: &DecodeOptions,
) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    // A `width * height` that overflows `usize` is an out-of-range size, not a short
    // input — the already-validated image dimensions keep this unreachable in practice.
    let count = w.checked_mul(h).ok_or(Error::InvalidDimensions)?;
    let (header, data) = parse_header(alph)?;
    let mut plane = match header.compression {
        AlphaCompression::None => data.get(..count).ok_or(Error::Truncated)?.to_vec(),
        AlphaCompression::Lossless => {
            crate::lossless::decode_alpha_with(data, width, height, options)?
        },
    };
    unfilter(header.filter, &mut plane, w, h);
    Ok(plane)
}

/// Decode an animated WebP into a lazy [`Frames`] iterator.
///
/// Handles both lossless (`VP8L`) and lossy (`VP8 ` + optional `ALPH`) frames —
/// the latter via the lossy decoder with alpha compositing, injected into the
/// lossless codec's animation walker.
///
/// # Untrusted input
///
/// Safe by default: like [`decode`], each frame's canvas is capped at
/// [`DEFAULT_MAX_PIXELS`] before allocation. Use [`decode_frames_with`] with
/// [`DecodeOptions::max_pixels`] to choose another cap, or
/// [`DecodeOptions::unbounded`] for trusted input.
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] if `input` is not an animation, or a
/// container/bitstream error from a frame.
#[cfg(feature = "alloc")]
pub fn decode_frames(input: &[u8]) -> Result<Frames<'_>> {
    decode_frames_with(input, &DecodeOptions::default())
}

/// Like [`decode_frames`] with explicit [`DecodeOptions`] (output layout,
/// per-frame pixel limit); the crate's both-codecs frame decoder is always wired in.
///
/// # Errors
///
/// The same as [`decode_frames`], plus [`Error::LimitExceeded`] when a frame
/// exceeds `options.max_pixels`.
#[cfg(feature = "alloc")]
pub fn decode_frames_with<'a>(input: &'a [u8], options: &DecodeOptions) -> Result<Frames<'a>> {
    crate::anim::decode_frames_with_decoder(input, options, WebpFrameDecoder)
}

/// A push-based [`IncrementalDecoder`] for still images and animations, with the
/// lossy-frame hook wired so animated `VP8 ` frames decode.
#[cfg(feature = "alloc")]
#[must_use]
pub fn incremental_decoder() -> IncrementalDecoder {
    IncrementalDecoder::new()
}

/// The chosen streaming back end once the container kind is known.
#[cfg(feature = "alloc")]
enum Backend {
    /// Not yet classified — buffering until the first image chunk is reachable.
    Undecided,
    /// Lossless still, or any animation: the `lossless` codec's incremental decoder (with the
    /// lossy-frame hook injected so animated `VP8 ` frames decode). Boxed to keep the
    /// enum small — the lossless decoder dwarfs the other variants.
    Lossless(Box<crate::lossless::IncrementalDecoder<WebpFrameDecoder>>),
    /// A bare lossy `VP8 ` still (no `VP8X`): true row streaming via [`crate::lossy`].
    Lossy(Box<crate::lossy::IncrementalDecoder>),
    /// An extended lossy still (`VP8X` + `VP8 `, possibly with `ALPH`): alpha /
    /// metadata compositing is not row-streamable byte-identically, so buffer the
    /// whole file and finish with a one-shot [`decode`].
    Deferred,
}

/// How [`IncrementalDecoder::push`] should proceed once enough bytes are buffered.
#[cfg(feature = "alloc")]
enum Decision {
    Lossless,
    Lossy,
    Deferred(ImageInfo),
    NeedMore,
}

/// A push-based decoder for **any** still WebP or animation.
///
/// Dispatches on the container kind: lossless stills and all animations stream
/// through the lossless decoder, a bare lossy `VP8 ` still row-streams through the
/// lossy decoder, and an extended lossy still (which may carry `ALPH` alpha) is buffered and
/// finished with a one-shot [`decode`]. The pixels and error semantics match
/// [`decode`] exactly; the only new streaming capability over the
/// lossless/animation paths is the bare lossy still. `Read`-free, so it works on
/// `no_std + alloc`.
#[cfg(feature = "alloc")]
pub struct IncrementalDecoder {
    buf: Vec<u8>,
    options: DecodeOptions,
    reported_header: bool,
    image: Option<Image>,
    backend: Backend,
}

#[cfg(feature = "alloc")]
impl IncrementalDecoder {
    /// A new decoder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(DecodeOptions::default())
    }

    /// A new decoder with the given options (output layout, per-image pixel limit).
    #[must_use]
    pub const fn with_options(options: DecodeOptions) -> Self {
        Self {
            buf: Vec::new(),
            options,
            reported_header: false,
            image: None,
            backend: Backend::Undecided,
        }
    }

    /// Feed the next slice of the file and report [`Progress`].
    ///
    /// # Errors
    ///
    /// The same errors as [`decode`], surfaced as soon as the buffered bytes make
    /// them detectable.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Progress> {
        // Once a back end is chosen, forward the raw chunk to it (its own buffer
        // already holds everything up to here, replayed on the deciding push).
        match &mut self.backend {
            Backend::Lossless(be) => return be.push(chunk),
            Backend::Lossy(be) => return be.push(chunk),
            Backend::Deferred => {
                self.buf.extend_from_slice(chunk);
                return self.drive_deferred();
            },
            Backend::Undecided => {},
        }
        if self.image.is_some() {
            return Ok(Progress::Finished);
        }
        self.buf.extend_from_slice(chunk);
        match classify(&self.buf)? {
            Decision::Lossless => {
                // Inject the both-codecs frame decoder so animated `VP8 ` frames
                // decode (a bare `lossless` decoder would reject them).
                let mut be = crate::lossless::IncrementalDecoder::with_options_and_decoder(
                    self.options.clone(),
                    WebpFrameDecoder,
                );
                let progress = be.push(&self.buf)?;
                self.backend = Backend::Lossless(Box::new(be));
                Ok(progress)
            },
            Decision::Lossy => {
                let mut be = crate::lossy::IncrementalDecoder::with_options(self.options.clone());
                let progress = be.push(&self.buf)?;
                self.backend = Backend::Lossy(Box::new(be));
                Ok(progress)
            },
            Decision::Deferred(info) => {
                self.backend = Backend::Deferred;
                if !self.reported_header {
                    self.reported_header = true;
                    return Ok(Progress::HeaderReady(info));
                }
                self.drive_deferred()
            },
            Decision::NeedMore => {
                // A complete-but-unclassifiable buffer (e.g. a malformed container)
                // takes the one-shot path, preserving its exact error / image.
                if is_complete(&self.buf) {
                    self.image = Some(decode(&self.buf)?);
                    Ok(Progress::Finished)
                } else {
                    Ok(Progress::NeedMoreInput)
                }
            },
        }
    }

    /// Finish the deferred (extended-lossy) path once the whole RIFF is buffered.
    fn drive_deferred(&mut self) -> Result<Progress> {
        if is_complete(&self.buf) {
            self.image = Some(decode(&self.buf)?);
            Ok(Progress::Finished)
        } else {
            Ok(Progress::NeedMoreInput)
        }
    }

    /// The most-recently composited animation frame, or `None` for a still image.
    /// Mirrors the underlying decoder's `frame_image`.
    #[must_use]
    pub fn frame_image(&self) -> Option<&Image> {
        match &self.backend {
            Backend::Lossless(be) => be.frame_image(),
            _ => None,
        }
    }

    /// Borrow the finalized-but-not-yet-viewed rows of a streamed still image (a
    /// non-consuming early view). `None` unless a row-streaming back end is active
    /// (a lossless or bare-lossy still); the deferred and animation paths yield no
    /// rows.
    pub fn drain_rows(&mut self) -> Option<RowDrain<'_>> {
        match &mut self.backend {
            Backend::Lossless(be) => be.drain_rows(),
            Backend::Lossy(be) => be.drain_rows(),
            Backend::Undecided | Backend::Deferred => None,
        }
    }

    /// Retrieve the complete decoded image (an animation's first composited frame)
    /// once [`Progress::Finished`] has been reported.
    ///
    /// # Errors
    ///
    /// The same errors as [`decode`] when the buffer is not a fully-decoded image.
    pub fn into_image(self) -> Result<Image> {
        if let Some(image) = self.image {
            return Ok(image);
        }
        match self.backend {
            Backend::Lossless(be) => be.into_image(),
            Backend::Lossy(be) => be.into_image(),
            Backend::Undecided | Backend::Deferred => decode(&self.buf),
        }
    }
}

#[cfg(feature = "alloc")]
impl Default for IncrementalDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Walk the buffered container to decide which streaming back end handles it.
/// `Ok(Decision::NeedMore)` means the first image chunk is not yet reachable.
#[cfg(feature = "alloc")]
fn classify(buf: &[u8]) -> Result<Decision> {
    if buf.len() < 12 {
        return Ok(Decision::NeedMore);
    }
    if buf[0..4] != FourCc::RIFF.0 || buf[8..12] != FourCc::WEBP.0 {
        return Err(Error::NotWebp);
    }
    // The header of a non-animated `VP8X`, if seen before the image chunk: its
    // presence means an extended (possibly alpha-bearing) lossy still is deferred.
    let mut vp8x_info: Option<ImageInfo> = None;
    for chunk in scan_chunks(buf) {
        match chunk.id {
            FourCc::VP8X => {
                let Some(data) =
                    buf.get(chunk.payload_start..chunk.payload_start + VP8X_PAYLOAD_LEN)
                else {
                    return Ok(Decision::NeedMore);
                };
                let info = Vp8xInfo::parse(data)?;
                if info.flags.is_animated() {
                    return Ok(Decision::Lossless);
                }
                vp8x_info = Some(ImageInfo::new(
                    info.canvas,
                    info.flags.has_alpha(),
                    info.flags.has_icc() || info.flags.has_exif() || info.flags.has_xmp(),
                    false,
                ));
            },
            // A lossless still (crate::lossless re-reads any VP8X sidecar) or an animation
            // chunk: crate::lossless streams it.
            FourCc::VP8L | FourCc::ANIM | FourCc::ANMF => return Ok(Decision::Lossless),
            FourCc::VP8 => {
                if let Some(info) = vp8x_info {
                    // Extended lossy (has VP8X); the chunk we just reached says VP8.
                    return Ok(Decision::Deferred(info.with_codec(Codec::Lossy)));
                }
                // Bare VP8: row-stream only if it spans the whole declared RIFF
                // body (`chunk.next` is the padded end), so no trailing
                // ALPH/metadata chunk can diverge the opaque stream from the
                // one-shot decode; otherwise defer.
                return match declared_len(buf) {
                    Some(riff_end) if chunk.next >= riff_end => Ok(Decision::Lossy),
                    Some(_) => Ok(bare_deferred(buf.get(chunk.payload_start..))),
                    None => Ok(Decision::NeedMore),
                };
            },
            _ => {},
        }
    }
    Ok(Decision::NeedMore)
}

/// A bare `VP8 ` followed by trailing chunks (e.g. a malformed `ALPH` with no
/// `VP8X`): defer to the one-shot decode, peeking the VP8 dimensions for the
/// header report. `NeedMore` until the 10-byte VP8 header is buffered.
#[cfg(feature = "alloc")]
fn bare_deferred(vp8_payload: Option<&[u8]>) -> Decision {
    vp8_payload
        .and_then(|p| crate::lossy::peek_dimensions(p).ok())
        .map_or(Decision::NeedMore, |dimensions| {
            Decision::Deferred(
                ImageInfo::new(dimensions, false, false, false).with_codec(Codec::Lossy),
            )
        })
}

/// Decode an animation's first composited frame as a still [`Image`], honoring
/// `options` (output layout, per-frame pixel limit).
#[cfg(feature = "alloc")]
fn first_composited_frame(input: &[u8], options: &DecodeOptions) -> Result<Image> {
    decode_frames_with(input, options)?
        .composited()
        .next()
        .ok_or(Error::MissingImage)?
        .map(CompositedFrame::into_image)
}

/// The umbrella's [`FrameDecoder`](crate::stream::FrameDecoder): the seam that
/// drives **both** codecs.
///
/// It lets the lossless codec's codec-agnostic animation walker decode frames of
/// either codec. A `VP8L` frame is decoded by the lossless codec (delegating to its
/// own VP8L frame decoder); a lossy `VP8 ` (+ optional sibling `ALPH`) frame is
/// decoded by the lossy codec, compositing the alpha plane into the pixels' top byte.
///
/// Not part of the public API: it is `#[doc(hidden)] pub` only because the fixed
/// [`Frames`]/[`CompositedFrames`] aliases name it as their type argument. Callers
/// never spell it — [`decode_frames`] wires it in.
#[cfg(feature = "alloc")]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WebpFrameDecoder;

#[cfg(feature = "alloc")]
impl crate::stream::FrameDecoder for WebpFrameDecoder {
    fn decode_frame(
        &self,
        frame: crate::stream::FramePayload<'_>,
        options: &DecodeOptions,
    ) -> Result<crate::stream::DecodedFrame> {
        if frame.vp8l.is_some() {
            // The VP8L path is codec-internal to `crate::lossless` — reuse it verbatim.
            return crate::lossless::Vp8lFrameDecoder.decode_frame(frame, options);
        }
        let Some(payload) = frame.vp8 else {
            return Err(Error::MissingImage);
        };
        // Guard the pixel budget against the frame's declared dimensions before the
        // lossy decoder allocates its planes.
        let pixels = frame.dims.pixel_count();
        if let Some(limit) = options.max_pixels.filter(|&l| pixels > l) {
            return Err(Error::LimitExceeded { pixels, limit });
        }
        let (dims, mut argb) = crate::lossy::decode_argb_with(payload, options)?;
        if dims != frame.dims {
            return Err(Error::InvalidContainer);
        }
        if let Some(alph) = frame.alph {
            let plane = alpha_plane(alph, dims.width(), dims.height())?;
            for (pixel, &a) in argb.iter_mut().zip(&plane) {
                *pixel = (*pixel & 0x00FF_FFFF) | (u32::from(a) << 24);
            }
        }
        // libwebp keys the compositor on whether an `ALPH` chunk is present.
        Ok(crate::stream::DecodedFrame {
            argb,
            alpha_used: frame.alph.is_some(),
        })
    }
}

/// Whether `input` is an animated WebP.
///
/// A cheap header probe (a `VP8X` animation flag, or an `ANIM`/`ANMF` chunk) that
/// decodes no pixels and is codec-agnostic (it works for a lossy file the same as a
/// lossless one).
///
/// # Errors
///
/// [`Error::NotWebp`]/[`Error::Truncated`] for a non-WebP or short input, or
/// [`Error::InvalidContainer`] for a malformed `VP8X`.
pub fn is_animated(input: &[u8]) -> Result<bool> {
    crate::container::reader::is_animated(input)
}

/// Read a WebP's header — dimensions, alpha, metadata, animation — without
/// decoding a single pixel.
///
/// Answers the "what is this file" question that [`decode`] answers expensively:
/// the cost is a chunk walk and one bitstream header, so it is the same on a
/// 40-byte image and a 40-megapixel one, and it succeeds even when the pixel data
/// is truncated or corrupt.
///
/// This is the one-shot counterpart to [`Progress::HeaderReady`]. The incremental
/// decoder reports a header only as a side effect of streaming — a file small
/// enough to decode within one `push` never emits the event at all — so it cannot
/// be used as a probe.
///
/// # Examples
///
/// ```
/// // Busy pixels, so the encoded body is comfortably longer than its header.
/// let rgba: Vec<u8> = (0..64u32 * 64 * 4).map(|i| (i * 7 % 251) as u8).collect();
/// let webp = webpkit::encode_lossless_rgba(64, 64, &rgba)?;
///
/// let info = webpkit::probe(&webp)?;
/// assert_eq!((info.dimensions.width(), info.dimensions.height()), (64, 64));
/// assert!(!info.is_animated);
///
/// // The header survives what the pixels do not.
/// let truncated = &webp[..webp.len() / 2];
/// assert!(webpkit::decode(truncated).is_err());
/// assert_eq!(webpkit::probe(truncated)?.dimensions, info.dimensions);
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::NotWebp`] if the container magic is wrong, [`Error::Truncated`] if the
/// header is not fully present, [`Error::MissingImage`] if no image chunk is
/// found, or the codec's error for a malformed bitstream header.
#[cfg(feature = "alloc")]
pub fn probe(input: &[u8]) -> Result<ImageInfo> {
    if input.len() < 12 {
        return Err(Error::Truncated);
    }
    if input[0..4] != FourCc::RIFF.0 || input[8..12] != FourCc::WEBP.0 {
        return Err(Error::NotWebp);
    }

    // The walk mirrors `read_container`, which is what `decode` uses: the first
    // image chunk wins, and the sidecars are collected wherever they sit. Anything
    // narrower would let `probe` and `decode` disagree about one file — an `ALPH`
    // needs no `VP8X` to announce it, and it may follow the image chunk.
    let mut image: Option<ImageInfo> = None;
    let mut sidecar_alpha = false;
    let mut sidecar_metadata = false;

    for chunk in scan_chunks(input) {
        match chunk.id {
            FourCc::VP8X => {
                let data = input
                    .get(chunk.payload_start..chunk.payload_start + VP8X_PAYLOAD_LEN)
                    .ok_or(Error::Truncated)?;
                let info = Vp8xInfo::parse(data)?;
                // An animation is fully described by its VP8X: probe returns here,
                // before any per-frame chunk, so its alpha/metadata must come from
                // the flags. Its frames each carry their own codec, so the file has
                // none. A still VP8X only wraps flags the image chunk and sidecars
                // below state authoritatively, so it is not kept.
                if info.flags.is_animated() {
                    return Ok(ImageInfo::new(
                        info.canvas,
                        info.flags.has_alpha(),
                        vp8x_has_metadata(info),
                        true,
                    ));
                }
            },
            FourCc::VP8L if image.is_none() => {
                image = crate::lossless::decoder::peek_vp8l_info(
                    input.get(chunk.payload_start..),
                    false,
                    false,
                )?;
            },
            FourCc::VP8 if image.is_none() => {
                let payload = input.get(chunk.payload_start..).ok_or(Error::Truncated)?;
                let dimensions = crate::lossy::peek_dimensions(payload)?;
                image =
                    Some(ImageInfo::new(dimensions, false, false, false).with_codec(Codec::Lossy));
            },
            FourCc::ALPH => sidecar_alpha = true,
            FourCc::ICCP | FourCc::EXIF | FourCc::XMP => sidecar_metadata = true,
            // Animation chunks with no preceding animated VP8X are malformed; a
            // well-formed animation returns above.
            FourCc::ANIM | FourCc::ANMF => return Err(Error::InvalidContainer),
            _ => {},
        }
    }

    let mut info = image.ok_or(Error::MissingImage)?;
    // A still's alpha and metadata are exactly what `decode` finds: the VP8L
    // header's alpha bit (peeked above) or a lossy sibling `ALPH`, and the
    // metadata sidecar chunks. `decode` reads chunk presence, not the VP8X flags,
    // so `probe` does too — otherwise a VP8X that advertises a chunk it does not
    // carry would make the two disagree.
    info.has_alpha |= sidecar_alpha;
    info.has_metadata |= sidecar_metadata;
    Ok(info)
}

/// Whether a `VP8X` header advertises any metadata chunk.
#[cfg(feature = "alloc")]
const fn vp8x_has_metadata(info: Vp8xInfo) -> bool {
    info.flags.has_icc() || info.flags.has_exif() || info.flags.has_xmp()
}

/// Read a WebP file's sidecar [`Metadata`] — ICC profile, Exif, XMP — without
/// decoding a single pixel.
///
/// A chunk-level walk that collects the `ICCP`/`EXIF`/`XMP ` chunks wherever they
/// sit (still or animation): the read half of [`write_metadata`], and the metadata
/// companion to [`probe`]. It interprets no bitstream, so the cost is independent
/// of the image size, and it answers for a file whose pixel data would not decode
/// as long as the container framing is intact.
///
/// # Examples
///
/// ```
/// use webpkit::{Dimensions, Encoder, ImageRef, Metadata, PixelLayout};
///
/// let rgba = vec![0u8; 4 * 4 * 4];
/// let image = ImageRef::new(Dimensions::new(4, 4)?, PixelLayout::Rgba8, &rgba)?;
/// let webp = Encoder::lossless()
///     .metadata(Metadata::none().with_exif(vec![1, 2, 3]))
///     .encode_ref(image)?;
///
/// let meta = webpkit::read_metadata(&webp)?;
/// assert_eq!(meta.exif.as_deref(), Some(&[1, 2, 3][..]));
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::NotWebp`] for a bad `RIFF`/`WEBP` magic, or [`Error::Truncated`] for a
/// short or malformed container.
#[cfg(feature = "alloc")]
pub fn read_metadata(input: &[u8]) -> Result<Metadata> {
    let mut metadata = Metadata::none();
    for chunk in crate::container::reader::chunks(input)? {
        let chunk = chunk?;
        // First-wins, mirroring the `image.is_none()` VP8L guard: a duplicate
        // `ICCP`/`EXIF`/`XMP ` must not silently override the earlier one.
        match chunk.id {
            FourCc::ICCP if metadata.icc_profile.is_none() => {
                metadata.icc_profile = Some(chunk.data.to_vec());
            },
            FourCc::EXIF if metadata.exif.is_none() => metadata.exif = Some(chunk.data.to_vec()),
            FourCc::XMP if metadata.xmp.is_none() => metadata.xmp = Some(chunk.data.to_vec()),
            _ => {},
        }
    }
    Ok(metadata)
}

/// Rewrite a WebP file's sidecar metadata — set, replace, or strip ICC/Exif/XMP —
/// without re-encoding or even decoding the image bitstream.
///
/// The webpmux-style companion to [`read_metadata`]: the `VP8 `/`VP8L`/`ALPH`/
/// `ANIM`/`ANMF` image chunks are copied through byte-for-byte, the old
/// `ICCP`/`EXIF`/`XMP `/`VP8X` are dropped, and a fresh `VP8X` plus the new
/// metadata chunks are emitted in the spec's order. Stills and animations alike are
/// handled. An empty `metadata` on a file that was already the simple form yields a
/// bare (`VP8X`-free) file again.
///
/// # Examples
///
/// ```
/// let rgba = vec![0u8; 4 * 4 * 4];
/// let webp = webpkit::encode_lossless_rgba(4, 4, &rgba)?;
///
/// let tagged = webpkit::write_metadata(
///     &webp,
///     &webpkit::Metadata::none().with_xmp(b"<x:xmpmeta/>".to_vec()),
/// )?;
/// assert_eq!(
///     webpkit::read_metadata(&tagged)?.xmp.as_deref(),
///     Some(&b"<x:xmpmeta/>"[..]),
/// );
/// // The pixels are untouched by the metadata rewrite.
/// assert_eq!(webpkit::decode_rgba(&tagged)?, webpkit::decode_rgba(&webp)?);
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::NotWebp`]/[`Error::Truncated`] for a non-WebP or short input, or the
/// errors of [`probe`] when the container header cannot be read.
#[cfg(feature = "alloc")]
pub fn write_metadata(input: &[u8], metadata: &Metadata) -> Result<Vec<u8>> {
    // `probe` supplies the canvas, alpha, and animation facts from the header
    // alone, so the container rewrite interprets no bitstream itself.
    let info = probe(input)?;
    crate::container::mux::rewrite_metadata(
        input,
        metadata,
        info.dimensions,
        info.has_alpha,
        info.is_animated,
    )
}

/// Read an animation's canvas, loop count, frame count and total duration —
/// without decoding a single frame.
///
/// The animation counterpart to [`probe`]. Every fact lives in the `VP8X`,
/// `ANIM` and `ANMF` headers, so the cost is a chunk walk however many frames
/// there are, and a file whose frame data is damaged still answers.
///
/// libwebp's `WebPAnimInfo` is the same idea; [`decode_frames`] is what to use
/// when you actually want the pixels.
///
/// ```
/// # fn main() -> Result<(), webpkit::Error> {
/// # let bytes = std::fs::read(concat!(
/// #     env!("CARGO_MANIFEST_DIR"),
/// #     "/../webpkit-lossless-conformance/fixtures/decode/animation_frames/input.webp",
/// # )).unwrap();
/// let anim = webpkit::probe_animation(&bytes)?;
/// println!("{} frames, {} ms", anim.frame_count, anim.total_duration_ms);
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] if the file is not an animation,
/// [`Error::NotWebp`]/[`Error::Truncated`] for a non-WebP or short input, or
/// [`Error::InvalidContainer`] for a malformed `ANIM`.
#[cfg(feature = "alloc")]
pub fn probe_animation(input: &[u8]) -> Result<AnimInfo> {
    // Lifted so a canvas too large to composite can still be described; nothing
    // here allocates per pixel.
    let options = DecodeOptions::new().unbounded();
    Ok(decode_frames_with(input, &options)?.anim_info())
}

/// The crate version, as reported by Cargo.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use crate::container::fourcc::FourCc;
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::container::writer::{push_chunk, riff_envelope};

    use super::{
        BlendMode, Codec, DecodeOptions, Dimensions, DisposalMode, Effort, Encoder, Error,
        FrameMeta, Image, ImageRef, IncrementalDecoder, Metadata, MetadataPolicy, PixelLayout,
        Progress, decode, decode_with, decode_yuv, probe, probe_animation,
    };

    /// A 32x32 image of busy pixels, encoded with `make`.
    ///
    /// Deliberately not a flat fill: a flat 64x64 encodes to 28 bytes, so half of
    /// it is header and a truncation test on it would be measuring the header
    /// rather than the body. Busy pixels give a body worth truncating.
    fn sample(make: fn(ImageRef<'_>) -> Vec<u8>) -> Vec<u8> {
        let rgba: Vec<u8> = (0..dims().width() * dims().height() * 4)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        make(ImageRef::new(dims(), PixelLayout::Rgba8, &rgba).unwrap())
    }

    /// The sample's dimensions, named once so every probe assertion agrees.
    fn dims() -> Dimensions {
        Dimensions::new(32, 32).unwrap()
    }

    fn lossless_bytes() -> Vec<u8> {
        sample(|img| Encoder::lossless().encode_ref(img).unwrap())
    }

    fn lossy_bytes() -> Vec<u8> {
        sample(|img| Encoder::lossy().quality(80).encode_ref(img).unwrap())
    }

    #[test]
    fn probe_reads_a_still_header_for_either_codec() {
        for bytes in [lossless_bytes(), lossy_bytes()] {
            let info = probe(&bytes).unwrap();
            assert_eq!(info.dimensions, dims());
            assert!(!info.is_animated);
            assert!(!info.has_metadata);
        }
    }

    /// The whole point of a probe: the header is readable when the pixels are not.
    /// `decode` must fail on the same bytes, or this test measures nothing.
    #[test]
    fn probe_reads_a_header_whose_pixel_data_is_truncated() {
        for full in [lossless_bytes(), lossy_bytes()] {
            // Half the body, so the cut is unambiguously in the pixels.
            let cut = &full[..full.len() / 2];
            assert!(decode(cut).is_err(), "the truncation must break decoding");
            let info = probe(cut).expect("probe survives a truncated body");
            assert_eq!(info.dimensions, dims());
        }
    }

    #[test]
    fn probe_reports_which_codec_coded_a_still() {
        assert_eq!(
            probe(&lossless_bytes()).unwrap().codec,
            Some(Codec::Lossless)
        );
        assert_eq!(probe(&lossy_bytes()).unwrap().codec, Some(Codec::Lossy));
    }

    /// An animation's frames each carry their own image chunk and need not agree,
    /// so the container header cannot answer for the file. `None` says that; it
    /// does not mean "unknown codec".
    #[test]
    fn probe_leaves_an_animations_codec_unanswered() {
        let canvas = Dimensions::new(16, 8).unwrap();
        let rgba = vec![0x40u8; 16 * 8 * 4];
        let frame = ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = crate::AnimationEncoder::new(canvas)
            .add_frame(
                frame,
                FrameMeta {
                    x: 0,
                    y: 0,
                    dimensions: canvas,
                    duration_ms: 100,
                    blend: BlendMode::Blend,
                    dispose: DisposalMode::Keep,
                },
            )
            .unwrap()
            .finish();
        assert_eq!(probe(&bytes).unwrap().codec, None);
    }

    /// One file, two ways of asking: a probe and a stream must not disagree about
    /// what coded it. Nothing else keeps these two paths in step.
    #[test]
    fn probe_and_streaming_agree_on_the_codec() {
        for bytes in [lossless_bytes(), lossy_bytes()] {
            let expected = probe(&bytes).unwrap().codec;
            assert!(expected.is_some(), "a still always has a codec");

            let mut decoder = IncrementalDecoder::new();
            let mut streamed = None;
            for slice in bytes.chunks(16) {
                if let Progress::HeaderReady(info) = decoder.push(slice).unwrap() {
                    streamed = Some(info.codec);
                    break;
                }
            }
            assert_eq!(
                streamed,
                Some(expected),
                "HeaderReady must report the codec probe reports"
            );
        }
    }

    /// A bare `VP8 ` + `ALPH` with no `VP8X`: alpha announced by a chunk that no
    /// header flag mentions, which is the shape
    /// `composites_a_raw_alpha_chunk_onto_a_lossy_image` pins for `decode`.
    fn lossy_with_bare_alpha() -> Vec<u8> {
        let vp8_key_frame = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        let mut alph = vec![0x00u8];
        alph.resize(1 + 16 * 16, 0x80);
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8_key_frame);
        push_chunk(&mut body, FourCc::ALPH, &alph);
        riff_envelope(&body)
    }

    /// Metadata written *after* the image chunk, so a walk that stops at the image
    /// never sees it.
    fn lossless_with_trailing_metadata() -> Vec<u8> {
        let rgba = vec![0x11u8; 4 * 4 * 4];
        let dims = Dimensions::new(4, 4).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        Encoder::lossless()
            .metadata(Metadata {
                exif: Some(vec![9, 9, 9, 9]),
                ..Metadata::none()
            })
            .encode_ref(img)
            .unwrap()
    }

    /// The invariant, not an example of it: two ways of asking one file must not
    /// disagree. `probe` is a promise about what `decode` will produce, so every
    /// field they share is checked over every container shape the crate builds —
    /// simple, extended, sidecar-alpha-without-a-flag, trailing metadata.
    ///
    /// This exists because `probe` shipped believing a `VP8X` flag was "the only
    /// place to look" for alpha. That is true of the streaming classifier it was
    /// copied from, which has not buffered the trailing chunks yet. It is false of
    /// a probe, which is handed the whole file.
    #[test]
    fn probe_never_contradicts_decode() {
        let cases: [(&str, Vec<u8>); 4] = [
            ("lossless", lossless_bytes()),
            ("lossy", lossy_bytes()),
            ("lossy + bare ALPH", lossy_with_bare_alpha()),
            (
                "lossless + trailing metadata",
                lossless_with_trailing_metadata(),
            ),
        ];
        for (name, bytes) in cases {
            let probed = probe(&bytes).unwrap_or_else(|e| panic!("{name}: probe: {e:?}"));
            let decoded = decode(&bytes).unwrap_or_else(|e| panic!("{name}: decode: {e:?}"));
            assert_eq!(
                probed.dimensions,
                decoded.dimensions(),
                "{name}: dimensions"
            );
            assert_eq!(probed.has_alpha, decoded.has_alpha(), "{name}: has_alpha");
            assert_eq!(
                probed.has_metadata,
                !decoded.metadata().is_empty(),
                "{name}: has_metadata"
            );
        }
    }

    /// The honest boundary: a probe needs the header. Cutting into it must fail
    /// rather than invent dimensions.
    #[test]
    fn probe_fails_when_the_truncation_reaches_the_header() {
        assert!(probe(&lossless_bytes()[..14]).is_err());
    }

    #[test]
    fn probe_reports_metadata_from_the_vp8x_header() {
        let rgba = vec![0x20u8; 4 * 3 * 4];
        let dims = Dimensions::new(4, 3).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = Encoder::lossless()
            .metadata(Metadata {
                icc_profile: Some(vec![1, 2, 3]),
                ..Metadata::none()
            })
            .encode_ref(img)
            .unwrap();
        assert!(probe(&bytes).unwrap().has_metadata);
    }

    #[test]
    fn probe_reads_an_animation_canvas_without_decoding_frames() {
        let canvas = Dimensions::new(16, 8).unwrap();
        let rgba = vec![0x40u8; 16 * 8 * 4];
        let frame = ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = crate::AnimationEncoder::new(canvas)
            .add_frame(
                frame,
                FrameMeta {
                    x: 0,
                    y: 0,
                    dimensions: canvas,
                    duration_ms: 100,
                    blend: BlendMode::Blend,
                    dispose: DisposalMode::Keep,
                },
            )
            .unwrap()
            .finish();
        let info = probe(&bytes).unwrap();
        assert!(info.is_animated);
        assert_eq!(info.dimensions, canvas);
    }

    /// A frame meta with `duration_ms`, everything else fixed to the canvas.
    fn frame_meta(canvas: Dimensions, duration_ms: u32) -> FrameMeta {
        FrameMeta {
            x: 0,
            y: 0,
            dimensions: canvas,
            duration_ms,
            blend: BlendMode::Blend,
            dispose: DisposalMode::Keep,
        }
    }

    /// `probe_animation` reports the exact frame count and the sum of the frame
    /// durations, read from the `ANMF` headers — no decode. Distinct durations so
    /// the total is not reproducible by dropping or multiplying frames: 3 frames of
    /// 100/150/200 ms are 3 and 450, not 1, not 100·150·200.
    #[test]
    fn probe_animation_counts_frames_and_sums_durations() {
        let canvas = Dimensions::new(8, 6).unwrap();
        let rgba = vec![0x30u8; 8 * 6 * 4];
        let frame = || ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = crate::AnimationEncoder::new(canvas)
            .add_frame(frame(), frame_meta(canvas, 100))
            .unwrap()
            .add_frame(frame(), frame_meta(canvas, 150))
            .unwrap()
            .add_frame(frame(), frame_meta(canvas, 200))
            .unwrap()
            .finish();
        let info = probe_animation(&bytes).unwrap();
        assert_eq!(info.frame_count, 3);
        assert_eq!(info.total_duration_ms, 450);
    }

    /// `decode_yuv` recovers a lossy still's native 4:2:0 planes; odd sides pin the
    /// ceil-halved chroma dimensions and the `y = w*h`, `u = v = cw*ch` plane sizes.
    #[test]
    fn decode_yuv_recovers_4_2_0_plane_shapes_for_a_lossy_still() {
        let (w, h) = (7u32, 5u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let webp = crate::encode_lossy_rgba(w, h, &rgba, 80).unwrap();
        let yuv = decode_yuv(&webp).unwrap();
        assert_eq!((yuv.width(), yuv.height()), (w, h));
        assert_eq!((yuv.chroma_width(), yuv.chroma_height()), (4, 3));
        assert_eq!(yuv.y().len(), (w * h) as usize);
        assert_eq!(yuv.u().len(), yuv.v().len());
        assert_eq!(
            yuv.u().len(),
            (yuv.chroma_width() * yuv.chroma_height()) as usize
        );
    }

    /// A lossless still has no YUV form; `decode_yuv` says so rather than guessing.
    #[test]
    fn decode_yuv_rejects_a_lossless_still() {
        assert_eq!(
            decode_yuv(&lossless_bytes()),
            Err(Error::UnsupportedFeature)
        );
    }

    /// An animation is not a single still, so `decode_yuv` rejects it too.
    #[test]
    fn decode_yuv_rejects_an_animation() {
        let canvas = Dimensions::new(8, 6).unwrap();
        let rgba = vec![0x30u8; 8 * 6 * 4];
        let frame = ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = crate::AnimationEncoder::new(canvas)
            .add_frame(frame, frame_meta(canvas, 100))
            .unwrap()
            .finish();
        assert_eq!(decode_yuv(&bytes), Err(Error::UnsupportedFeature));
    }

    /// Metadata announced only by a sidecar chunk, with no `VP8X` at all: `probe`
    /// must report it, matching `decode`, which reads the chunk. Pins the
    /// `ICCP|EXIF|XMP` collection arm and the `has_metadata` fold.
    #[test]
    fn probe_reads_metadata_from_a_sidecar_without_a_vp8x() {
        // A bare `VP8L` still (no `VP8X`), then a trailing `EXIF` chunk.
        let mut body = lossless_bytes()[12..].to_vec();
        push_chunk(&mut body, FourCc::EXIF, b"MM\x00*exif");
        let bytes = riff_envelope(&body);
        assert!(probe(&bytes).unwrap().has_metadata, "sidecar EXIF, no VP8X");
        assert!(
            !decode(&bytes).unwrap().metadata().is_empty(),
            "decode reads the same chunk"
        );
    }

    /// An animation's alpha and metadata come from its `VP8X` flags, since `probe`
    /// returns at the animated header before any per-frame chunk. Pins
    /// `vp8x_has_metadata` per flag — a single flag must still report metadata,
    /// which `&&` between the three would deny.
    #[test]
    fn probe_reads_animation_metadata_from_vp8x_flags() {
        let canvas = Dimensions::new(16, 16).unwrap();
        let animated_vp8x = |meta: &Metadata| {
            let flags = Vp8xFlags::for_output(meta, false).with_animation();
            let mut body = Vec::new();
            push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, canvas));
            riff_envelope(&body)
        };
        // Each metadata kind alone must report metadata: `&&` between the three
        // flags would deny a single one.
        let cases = [
            Metadata {
                icc_profile: Some(vec![1]),
                ..Metadata::none()
            },
            Metadata {
                exif: Some(vec![1]),
                ..Metadata::none()
            },
            Metadata {
                xmp: Some(vec![1]),
                ..Metadata::none()
            },
        ];
        for meta in &cases {
            let info = probe(&animated_vp8x(meta)).unwrap();
            assert!(info.is_animated && info.has_metadata, "{meta:?}");
        }
        // No metadata flag: an animation reports none — the other side of the OR.
        let info = probe(&animated_vp8x(&Metadata::none())).unwrap();
        assert!(info.is_animated && !info.has_metadata);
    }

    /// `ANIM`/`ANMF` chunks with no preceding animated `VP8X` are malformed and
    /// rejected, not read as a still.
    #[test]
    fn probe_rejects_animation_chunks_without_an_animated_vp8x() {
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::ANMF, b"not a real frame");
        assert!(matches!(
            probe(&riff_envelope(&body)),
            Err(Error::InvalidContainer)
        ));
    }

    /// The first image chunk wins: a second one is ignored, so `probe` reports the
    /// first's dimensions. Pins the `image.is_none()` guards.
    #[test]
    fn probe_reports_the_first_image_chunk_when_two_are_present() {
        let first = lossless_bytes(); // 32x32
        let second = {
            let rgba = vec![0x11u8; 4 * 4 * 4];
            let dims = Dimensions::new(4, 4).unwrap();
            let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
            Encoder::lossless().encode_ref(img).unwrap()
        };
        let mut body = first[12..].to_vec();
        body.extend_from_slice(&second[12..]);
        let info = probe(&riff_envelope(&body)).unwrap();
        assert_eq!(
            info.dimensions,
            dims(),
            "the first (32x32) chunk, not the 4x4"
        );
    }

    /// The same first-image-wins guard on the lossy `VP8 ` arm: two `VP8 ` chunks
    /// of different sizes, and `probe` must report the first's dimensions. The
    /// `VP8L`-only case above never exercises the `VP8 ` arm, so its `image.is_none()`
    /// guard needs its own two-chunk fixture.
    #[test]
    fn probe_reports_the_first_lossy_image_chunk_when_two_are_present() {
        let first = lossy_bytes(); // 32x32
        let second = {
            let rgba = vec![0x11u8; 16 * 16 * 4];
            let d = Dimensions::new(16, 16).unwrap();
            let img = ImageRef::new(d, PixelLayout::Rgba8, &rgba).unwrap();
            Encoder::lossy().quality(80).encode_ref(img).unwrap()
        };
        let mut body = first[12..].to_vec();
        body.extend_from_slice(&second[12..]);
        let info = probe(&riff_envelope(&body)).unwrap();
        assert_eq!(
            info.dimensions,
            dims(),
            "the first (32x32) VP8, not the 16x16"
        );
        assert_eq!(info.codec, Some(Codec::Lossy));
    }

    #[test]
    fn probe_rejects_what_is_not_a_webp() {
        assert!(matches!(
            probe(b"not a webp file here"),
            Err(Error::NotWebp)
        ));
        assert!(matches!(probe(b"tiny"), Err(Error::Truncated)));
    }

    /// The length guard is `< 12`, and 12 is the smallest input that carries the
    /// full `RIFF....WEBP` magic. 11 bytes is too short to even check the magic
    /// (`Truncated`); 12 bytes is long enough, so a wrong magic is `NotWebp`, not
    /// `Truncated`. Pins the boundary so `<` cannot become `<=`.
    #[test]
    fn probe_length_guard_is_exactly_twelve() {
        assert!(matches!(probe(&[0u8; 11]), Err(Error::Truncated)));
        assert!(matches!(probe(&[0u8; 12]), Err(Error::NotWebp)));
    }

    /// The magic check rejects when *either* `RIFF` or `WEBP` is wrong (`||`). A
    /// file with a correct `RIFF` but a wrong `WEBP` fourcc must still be `NotWebp`;
    /// with `&&` it would slip past the guard. The mirror case (wrong `RIFF`,
    /// right `WEBP`) covers the other operand.
    #[test]
    fn probe_rejects_a_half_correct_magic() {
        let mut riff_only = *b"RIFF\0\0\0\0XXXX";
        assert_eq!(&riff_only[0..4], b"RIFF");
        assert_ne!(&riff_only[8..12], b"WEBP");
        assert!(matches!(probe(&riff_only), Err(Error::NotWebp)));

        // The other operand: right WEBP, wrong RIFF.
        riff_only[0..4].copy_from_slice(b"XXXX");
        riff_only[8..12].copy_from_slice(b"WEBP");
        assert!(matches!(probe(&riff_only), Err(Error::NotWebp)));
    }

    #[test]
    fn probe_rejects_a_container_with_no_image_chunk() {
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::EXIF, b"just metadata");
        assert!(matches!(
            probe(&riff_envelope(&body)),
            Err(Error::MissingImage)
        ));
    }

    #[test]
    fn round_trips_a_lossless_image_through_the_umbrella() {
        // Encoder::lossless writes VP8L; decode() must route it back through crate::lossless.
        let rgba = [10u8, 20, 30, 255, 40, 50, 60, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = Encoder::lossless().encode_ref(img).unwrap();
        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.dimensions(), dims);
        assert_eq!(decoded.as_bytes(), &rgba[..]);
    }

    #[test]
    fn round_trips_a_lossy_image_through_the_umbrella() {
        // Encoder::lossy writes a VP8 key frame; decode() routes it back
        // through crate::lossy and returns an image of the right shape (lossy, so the
        // pixels are close but not identical — only dimensions/opacity are pinned).
        let mut rgba = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[x * 16, y * 16, 128, 255]);
            }
        }
        let dims = Dimensions::new(16, 16).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = Encoder::lossy().quality(90).encode_ref(img).unwrap();
        assert_eq!(&file[12..16], b"VP8 ", "lossy chunk fourcc");
        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.dimensions(), dims);
        assert!(decoded.as_bytes().chunks_exact(4).all(|p| p[3] == 0xff));
    }

    #[test]
    fn lossy_alpha_round_trips_byte_exact_through_the_umbrella() {
        // Encode a lossy image with a NON-TRIVIAL alpha channel (a radial-ish
        // gradient plus fully-transparent and fully-opaque regions) and decode it
        // back. Alpha is LOSSLESS, so the decoded alpha lane must equal the source
        // byte-for-byte; the container must upgrade to the extended VP8X + ALPH form.
        let (w, h) = (24u32, 20u32);
        let mut rgba = Vec::new();
        let mut source_alpha = Vec::new();
        for y in 0..h {
            for x in 0..w {
                // Fully transparent top-left block, fully opaque bottom-right block,
                // a smooth diagonal ramp elsewhere.
                let a = if x < 4 && y < 4 {
                    0
                } else if x >= w - 4 && y >= h - 4 {
                    255
                } else {
                    u8::try_from(((x + y) * 255) / (w + h - 2)).unwrap_or(255)
                };
                source_alpha.push(a);
                let px = [
                    u8::try_from((x * 9) & 0xff).unwrap_or(0),
                    u8::try_from((y * 11) & 0xff).unwrap_or(0),
                    100,
                    a,
                ];
                rgba.extend_from_slice(&px);
            }
        }
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = Encoder::lossy().quality(90).encode_ref(img).unwrap();
        assert_eq!(
            &file[12..16],
            b"VP8X",
            "alpha image must use the extended form"
        );
        assert!(
            file.windows(4).any(|c| c == b"ALPH"),
            "must carry an ALPH chunk"
        );

        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.dimensions(), dims);
        assert!(decoded.has_alpha());
        // The alpha lane (byte 3 of every Rgba8 pixel) is byte-exact vs the source.
        let decoded_alpha: Vec<u8> = decoded.as_bytes().chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(decoded_alpha, source_alpha, "alpha must be lossless");
    }

    #[test]
    fn round_trips_each_lossy_effort_through_the_umbrella() {
        // Every effort preset (the shared `Effort`, now common to both codecs)
        // rides on the shared `Encoder::lossy` builder, so each effort must produce
        // a decodable, correctly-sized, fully-opaque VP8 image.
        let mut rgba = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[x * 16, y * 16, 128, 255]);
            }
        }
        let dims = Dimensions::new(16, 16).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        for effort in [Effort::level(0), Effort::AUTO, Effort::level(9)] {
            let file = Encoder::lossy()
                .quality(90)
                .effort(effort)
                .encode_ref(img)
                .unwrap();
            assert_eq!(&file[12..16], b"VP8 ", "{effort:?}: lossy chunk fourcc");
            let decoded = decode(&file).unwrap();
            assert_eq!(decoded.dimensions(), dims, "{effort:?}: dims");
            assert!(
                decoded.as_bytes().chunks_exact(4).all(|p| p[3] == 0xff),
                "{effort:?}: not fully opaque"
            );
        }
    }

    #[test]
    fn encode_image_lossy_preserves_metadata_through_the_umbrella() {
        // `Encoder::lossy().encode(&Image)` routes an `Image` carrying metadata to
        // the lossy encoder, which upgrades to the extended VP8X form and emits the
        // ICCP/EXIF/XMP chunks. The lossless side is exercised by crate::lossless's own
        // tests; here we confirm the lossy encode preserves metadata by default.
        let mut rgba = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[x * 16, y * 16, 128, 255]);
            }
        }
        let dims = Dimensions::new(16, 16).unwrap();
        let metadata = Metadata {
            icc_profile: Some(b"icc".to_vec()),
            exif: Some(b"exif".to_vec()),
            xmp: Some(b"<x/>".to_vec()),
        };
        let img = Image::from_parts(dims, PixelLayout::Rgba8, rgba, false, metadata.clone());
        let file = Encoder::lossy().quality(90).encode(&img).unwrap();
        assert_eq!(
            &file[12..16],
            b"VP8X",
            "metadata must force the extended form"
        );
        let find = |id: &[u8; 4]| -> Option<Vec<u8>> {
            crate::container::reader::chunks(&file)
                .unwrap()
                .filter_map(Result::ok)
                .find(|c| &c.id.0 == id)
                .map(|c| c.data.to_vec())
        };
        assert_eq!(find(b"ICCP").as_deref(), metadata.icc_profile.as_deref());
        assert_eq!(find(b"EXIF").as_deref(), metadata.exif.as_deref());
        assert_eq!(find(b"XMP ").as_deref(), metadata.xmp.as_deref());
        // The image still decodes to the right shape through the umbrella.
        assert_eq!(decode(&file).unwrap().dimensions(), dims);
    }

    #[test]
    fn lossy_decode_surfaces_vp8x_metadata_and_round_trips() {
        // Decoding a lossy VP8X file must surface its ICCP/EXIF/XMP into the
        // returned `Image`, so a decode → `encode_image` round trip preserves the
        // color profile and sidecar metadata (matching the lossless path).
        let mut rgba = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[x * 16, y * 16, 200, 255]);
            }
        }
        let dims = Dimensions::new(16, 16).unwrap();
        let metadata = Metadata {
            icc_profile: Some(b"icc-profile-bytes".to_vec()),
            exif: Some(b"exif-bytes".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
        };
        let img = Image::from_parts(dims, PixelLayout::Rgba8, rgba, false, metadata.clone());
        let file = Encoder::lossy().quality(90).encode(&img).unwrap();

        // A decoded lossy file carries its ICC/Exif/XMP metadata.
        let decoded = decode(&file).unwrap();
        assert_eq!(
            decoded.metadata().icc_profile.as_deref(),
            metadata.icc_profile.as_deref()
        );
        assert_eq!(decoded.metadata().exif.as_deref(), metadata.exif.as_deref());
        assert_eq!(decoded.metadata().xmp.as_deref(), metadata.xmp.as_deref());

        // Full round trip: re-encoding the decoded image preserves the metadata.
        let refile = Encoder::lossy().quality(90).encode(&decoded).unwrap();
        let round = decode(&refile).unwrap();
        assert_eq!(
            round.metadata(),
            &metadata,
            "decode → Encoder::lossy().encode must preserve metadata"
        );

        // A bare (metadata-free) lossy file still decodes to empty metadata.
        let bare = Encoder::lossy()
            .quality(90)
            .encode_ref(ImageRef::new(dims, PixelLayout::Rgba8, img.as_bytes()).unwrap())
            .unwrap();
        assert_eq!(decode(&bare).unwrap().metadata(), &Metadata::none());
    }

    #[test]
    fn dispatches_a_lossy_file_to_the_vp8_decoder() {
        // A RIFF/`VP8 ` container with a valid key-frame header routes to
        // crate::lossy, which reconstructs it to an image of the declared size.
        let vp8_key_frame = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8_key_frame);
        let file = riff_envelope(&body);
        let image = decode(&file).unwrap();
        assert_eq!(image.dimensions(), Dimensions::new(16, 16).unwrap());
    }

    #[test]
    fn composites_a_raw_alpha_chunk_onto_a_lossy_image() {
        // A `VP8 ` 16x16 key-frame header plus a raw (method=0, filter=NONE) `ALPH`
        // plane must decode to an image whose alpha channel is the plane's bytes.
        let vp8_key_frame = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        let mut alph = Vec::new();
        alph.push(0x00u8); // method=0 (none), filter=0 (NONE), pre_processing=0
        alph.resize(1 + 16 * 16, 0x80); // 256 alpha bytes, all 0x80
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8_key_frame);
        push_chunk(&mut body, FourCc::ALPH, &alph);
        let file = riff_envelope(&body);
        let image = decode(&file).unwrap();
        assert_eq!(image.dimensions(), Dimensions::new(16, 16).unwrap());
        assert!(image.has_alpha());
        // Default Rgba8 layout: the alpha lane is byte 3 of every pixel.
        assert!(image.as_bytes().chunks_exact(4).all(|px| px[3] == 0x80));
    }

    #[test]
    fn rejects_a_non_webp_input() {
        // At least 12 bytes so the RIFF/WEBP magic check runs (a shorter input is
        // reported as `Truncated` before the magic is even examined).
        assert_eq!(
            decode(b"definitely not a webp file").unwrap_err(),
            Error::NotWebp
        );
    }

    #[test]
    fn unified_streams_a_bare_lossy_still_and_drains_rows() {
        // A bare RIFF/`VP8 ` still routes to crate::lossy and row-streams: one-byte
        // pushes reproduce the one-shot decode and expose rows via drain_rows. A
        // real 32x24 stream (unlike a tiny header-only frame, whose payload only
        // completes at EOF) sets up the still stream well before completion, so
        // rows are genuinely drained incrementally.
        let vp8 = include_bytes!("../tests/fixtures/noise_32x24_q30.vp8");
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, vp8);
        let file = riff_envelope(&body);
        let expected = decode(&file).unwrap();

        let mut dec = IncrementalDecoder::new();
        let mut drained = 0u32;
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
            if let Some(rows) = dec.drain_rows() {
                drained += rows.rows;
            }
        }
        assert!(drained > 0, "a lossy still must stream rows");
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected.as_bytes());
    }

    #[test]
    fn unified_streams_a_lossless_still() {
        // A VP8L still routes to crate::lossless and streams; the assembled image matches
        // the one-shot decode.
        let rgba: Vec<u8> = (0..16u8).flat_map(|i| [i * 3, i * 5, i * 7, 255]).collect();
        let dims = Dimensions::new(4, 4).unwrap();
        let file = Encoder::lossless()
            .encode_ref(ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap())
            .unwrap();
        let expected = decode(&file).unwrap();

        let mut dec = IncrementalDecoder::new();
        for chunk in file.chunks(3) {
            dec.push(chunk).unwrap();
        }
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected.as_bytes());
    }

    #[test]
    fn unified_defers_a_lossy_still_with_alpha() {
        // `VP8 ` + `ALPH` (alpha present) is deferred to the one-shot decode: no
        // rows stream, but into_image composites the alpha exactly like decode().
        let vp8 = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        let mut alph = vec![0x00u8]; // method=0, filter=NONE
        alph.resize(1 + 16 * 16, 0x80);
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8);
        push_chunk(&mut body, FourCc::ALPH, &alph);
        let file = riff_envelope(&body);
        let expected = decode(&file).unwrap();

        let mut dec = IncrementalDecoder::new();
        let mut any_drain = false;
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
            any_drain |= dec.drain_rows().is_some();
        }
        assert!(!any_drain, "the deferred alpha path streams no rows");
        let image = dec.into_image().unwrap();
        assert!(image.has_alpha());
        assert_eq!(image.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn decode_with_enforces_max_pixels_on_a_lossless_still() {
        // The umbrella decode_with propagates the pixel limit to the lossless
        // decoder, which rejects an 8x8 (64px) image *before* allocating pixels.
        let rgba: Vec<u8> = (0u8..64).flat_map(|i| [i, 0, 0, 255]).collect();
        let dims = Dimensions::new(8, 8).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = Encoder::lossless().encode_ref(img).unwrap();
        assert_eq!(
            decode_with(&file, &DecodeOptions::default().max_pixels(10)).unwrap_err(),
            Error::LimitExceeded {
                pixels: 64,
                limit: 10,
            }
        );
        // A 64px image is far under the default `DEFAULT_MAX_PIXELS` cap, so the
        // default options still decode it.
        assert_eq!(
            decode_with(&file, &DecodeOptions::default())
                .unwrap()
                .dimensions(),
            dims
        );
    }

    #[test]
    fn plain_decode_is_bounded_by_default_on_a_hostile_header() {
        // Safe by default: a bare `decode` (no options) must reject a header that
        // claims a huge canvas — here a 16383x16383 (≈268 Mpx) lossy `VP8 ` key
        // frame, well past `DEFAULT_MAX_PIXELS` (100 Mpx) — *before* any plane is
        // allocated, so only the 10-byte header is needed. A regression that let
        // `decode` bypass the default cap would allocate ≈1 GiB here instead.
        let vp8 = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0xFF, 0x3F, 0xFF, 0x3F];
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8);
        let file = riff_envelope(&body);
        assert!(
            matches!(decode(&file), Err(Error::LimitExceeded { limit, .. })
                if limit == crate::DEFAULT_MAX_PIXELS),
            "plain decode must enforce DEFAULT_MAX_PIXELS on an oversized header"
        );
        // `.unbounded()` is the explicit escape hatch: the same header now passes the
        // pixel guard (and fails later on the truncated body, not on the cap).
        assert!(
            !matches!(
                decode_with(&file, &DecodeOptions::default().unbounded()),
                Err(Error::LimitExceeded { .. })
            ),
            "unbounded() must lift the pixel cap"
        );
    }

    #[test]
    fn decode_with_enforces_max_pixels_on_lossy_still_and_frames() {
        // A bare 16x16 lossy `VP8 ` still: decode_with rejects it before planes.
        let vp8 = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, &vp8);
        let file = riff_envelope(&body);
        assert_eq!(
            decode_with(&file, &DecodeOptions::default().max_pixels(4)).unwrap_err(),
            Error::LimitExceeded {
                pixels: 256,
                limit: 4,
            }
        );

        // An animation: decode_with routes to the first composited frame through
        // decode_frames_with, which rejects a 2x2 (4px) frame under a 1-pixel cap.
        let canvas = Dimensions::new(2, 2).unwrap();
        let red = [255u8, 0, 0, 255].repeat(4);
        let meta = FrameMeta {
            x: 0,
            y: 0,
            dimensions: canvas,
            duration_ms: 100,
            blend: BlendMode::Blend,
            dispose: DisposalMode::Keep,
        };
        let anim = crate::AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &red).unwrap(),
                meta,
            )
            .unwrap()
            .finish();
        assert!(matches!(
            decode_with(&anim, &DecodeOptions::default().max_pixels(1)),
            Err(Error::LimitExceeded { .. })
        ));
    }

    #[test]
    fn metadata_policy_is_reexported_from_the_umbrella() {
        // The umbrella re-exports the shared `MetadataPolicy` so a caller can name
        // the `Encoder::metadata_policy` argument type through `webpkit`.
        let dims = Dimensions::new(1, 1).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![1]),
            exif: Some(vec![2]),
            xmp: Some(vec![3]),
        };
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            vec![0, 0, 0, 255],
            false,
            metadata,
        );
        let file = Encoder::lossless()
            .metadata_policy(MetadataPolicy::StripPrivate)
            .encode(&img)
            .unwrap();
        let decoded = decode(&file).unwrap();
        // StripPrivate keeps ICC, drops the privacy-bearing Exif/XMP sidecars.
        assert_eq!(decoded.metadata().icc_profile.as_deref(), Some(&[1][..]));
        assert_eq!(decoded.metadata().exif, None);
        assert_eq!(decoded.metadata().xmp, None);
    }

    #[test]
    fn unified_streams_animation_frames() {
        // An animation routes to crate::lossless's frame walker: each frame composites and
        // exposes its canvas; into_image returns the first composited frame.
        let canvas = Dimensions::new(2, 2).unwrap();
        let red = [255u8, 0, 0, 255].repeat(4);
        let blue = [0u8, 0, 255, 255].repeat(4);
        let meta = |ms| FrameMeta {
            x: 0,
            y: 0,
            dimensions: canvas,
            duration_ms: ms,
            blend: BlendMode::Blend,
            dispose: DisposalMode::Keep,
        };
        let file = crate::AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &red).unwrap(),
                meta(100),
            )
            .unwrap()
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &blue).unwrap(),
                meta(100),
            )
            .unwrap()
            .finish();
        let expected = decode(&file).unwrap();

        let mut dec = IncrementalDecoder::new();
        let mut chunks = file.chunks(5);
        let mut frames = 0;
        loop {
            let progress = match chunks.next() {
                Some(chunk) => dec.push(chunk),
                None => dec.push(&[]),
            }
            .unwrap();
            match progress {
                Progress::FrameComplete(_) => {
                    frames += 1;
                    assert!(dec.frame_image().is_some());
                },
                Progress::Finished => break,
                _ => {},
            }
        }
        assert_eq!(frames, 2, "both frames composited");
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected.as_bytes());
    }

    /// `write_metadata` then `read_metadata` round-trips an arbitrary metadata set
    /// on a still of either codec, and the decoded pixels are unchanged — the
    /// facade's promise that the image bitstream is never touched.
    #[test]
    fn write_then_read_metadata_round_trips_on_a_still() {
        for bytes in [lossless_bytes(), lossy_bytes()] {
            let meta = Metadata {
                icc_profile: Some(vec![1, 2, 3]),
                exif: Some(vec![4, 5]),
                xmp: Some(vec![6]),
            };
            let out = super::write_metadata(&bytes, &meta).unwrap();
            assert_eq!(super::read_metadata(&out).unwrap(), meta);
            assert_eq!(
                decode(&out).unwrap().into_pixels(),
                decode(&bytes).unwrap().into_pixels(),
                "the metadata rewrite must not alter the pixels"
            );
        }
    }

    /// Stripping metadata (an empty `Metadata`) leaves the pixels intact and the
    /// output carrying nothing — even when the source was the extended form.
    #[test]
    fn write_metadata_strips_and_preserves_pixels() {
        let tagged = lossless_with_trailing_metadata();
        assert!(!super::read_metadata(&tagged).unwrap().is_empty());
        let stripped = super::write_metadata(&tagged, &Metadata::none()).unwrap();
        assert!(super::read_metadata(&stripped).unwrap().is_empty());
        assert_eq!(
            decode(&stripped).unwrap().into_pixels(),
            decode(&tagged).unwrap().into_pixels()
        );
    }

    /// An animation survives a metadata rewrite: it stays animated, its metadata
    /// round-trips, and its first composited frame is unchanged.
    #[test]
    fn write_metadata_preserves_an_animation() {
        let canvas = Dimensions::new(16, 8).unwrap();
        let rgba = vec![0x40u8; 16 * 8 * 4];
        let frame = ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        let bytes = crate::AnimationEncoder::new(canvas)
            .add_frame(
                frame,
                FrameMeta {
                    x: 0,
                    y: 0,
                    dimensions: canvas,
                    duration_ms: 100,
                    blend: BlendMode::Blend,
                    dispose: DisposalMode::Keep,
                },
            )
            .unwrap()
            .finish();
        let meta = Metadata {
            xmp: Some(vec![7, 7]),
            ..Metadata::none()
        };
        let out = super::write_metadata(&bytes, &meta).unwrap();
        assert_eq!(super::read_metadata(&out).unwrap(), meta);
        assert!(super::is_animated(&out).unwrap());
        assert_eq!(
            decode(&out).unwrap().into_pixels(),
            decode(&bytes).unwrap().into_pixels()
        );
    }

    /// `read_metadata` reports a non-WebP input rather than inventing empty
    /// metadata, and reads a sidecar that sits after the image chunk.
    #[test]
    fn read_metadata_validates_and_finds_trailing_sidecars() {
        assert!(matches!(
            super::read_metadata(b"not a webp file"),
            Err(Error::NotWebp)
        ));
        let tagged = lossless_with_trailing_metadata();
        assert_eq!(
            super::read_metadata(&tagged).unwrap().exif.as_deref(),
            Some(&[9, 9, 9, 9][..])
        );
    }
}
