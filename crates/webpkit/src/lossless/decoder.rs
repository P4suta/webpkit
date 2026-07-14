//! Public decoding: the one-shot [`decode_image`], the `std` [`Decoder<R>`], and
//! the byte-buffering [`IncrementalDecoder`].
//!
//! For a **still image** the incremental decoder streams: as pushed bytes arrive
//! it drives a suspend/resume [`Vp8lStream`], reporting [`Progress::RowsDecoded`]
//! as output rows finalize and exposing them zero-copy through
//! [`IncrementalDecoder::drain_rows`] (a non-consuming early view).
//! [`IncrementalDecoder::into_image`] always returns the **complete** image —
//! assembled from the retained rows with no second decode when nothing was
//! drained-and-freed, else re-decoded from the buffer — byte-identical to a single
//! [`decode_image`] call regardless of how the input was split.
//!
//! For an **animation** it drives an [`AnimState`] frame walker: as bytes arrive
//! it decodes and composites each `ANMF` chunk (whole-frame granularity — frames
//! are small, so there is no intra-frame VP8L suspension), reporting
//! [`Progress::FrameComplete`] per frame through the *same* [`Compositor`] the
//! one-shot [`crate::lossless::decode_frames`] path uses. [`IncrementalDecoder::into_image`]
//! still returns the first composited frame (matching [`crate::lossless::decode`]).

use crate::anim::{Compositor, decode_anmf, frame_meta};
use crate::container::anim::FrameMeta;
use crate::container::fourcc::FourCc;
use crate::container::reader::{body_range, parse_container, read_chunk_at};
use crate::container::scan::{is_complete, scan_chunks};
use crate::container::vp8x::{VP8X_PAYLOAD_LEN, Vp8xInfo};
use crate::error::{Codec, Error, Result};
use crate::image::{self, Dimensions, Image, Metadata, PixelLayout};
use crate::lossless::animation::{CompositedFrame, Frames, Vp8lFrameDecoder};
use crate::lossless::prelude::*;
use crate::lossless::vp8l;
use crate::lossless::vp8l::decode_incr::{Step, Vp8lStream};

pub use crate::stream::{
    DecodeOptions, DecodedFrame, FrameDecoder, FramePayload, ImageInfo, Progress, RowDrain,
};

#[cfg(feature = "std")]
use std::io::Read;

/// One-shot decode of a complete WebP file into an [`Image`].
///
/// For an animation, this returns the **first composited frame** (matching
/// libwebp's `WebPDecode`); use [`crate::lossless::decode_frames`] for all frames.
///
/// # Errors
///
/// Container/bitstream errors, [`Error::UnsupportedFeature`] for lossy input, or
/// [`Error::LimitExceeded`] if `options.max_pixels` is exceeded.
pub(crate) fn decode_image(bytes: &[u8], options: &DecodeOptions) -> Result<Image> {
    // Route a clearly-animated file to its first composited frame, leaving the
    // still-image path (and its exact error semantics) untouched otherwise.
    if is_animated_file(bytes) {
        return decode_first_frame(bytes, options);
    }
    let parsed = parse_container(bytes, options.read_metadata)?;
    let image = decode_vp8l(parsed.vp8l, options)?;
    // A VP8X canvas, if present, must agree with the image dimensions.
    if parsed
        .vp8x
        .is_some_and(|vp8x| vp8x.canvas != image.dimensions())
    {
        return Err(Error::InvalidContainer);
    }
    Ok(image.with_metadata(parsed.metadata))
}

/// Decode a raw `VP8L` bitstream `payload` (the contents of a WebP `VP8L` chunk,
/// starting at the `0x2f` signature) into an [`Image`] — no container framing.
///
/// The payload-in counterpart to [`crate::lossless::decode`] (which parses the RIFF
/// container first): the umbrella `webpkit` crate, having already located the chunk
/// in a single container walk, calls this so the file is not re-parsed. The
/// returned image carries **no** metadata (the caller attaches any container
/// sidecars); `options.max_pixels` is enforced against the header dimensions,
/// before the pixel buffer is allocated, exactly as the full-file path does.
///
/// # Errors
///
/// [`Error::InvalidBitstream`] for a malformed VP8L stream, or
/// [`Error::LimitExceeded`] when `options.max_pixels` is exceeded.
pub fn decode_vp8l(payload: &[u8], options: &DecodeOptions) -> Result<Image> {
    // Size-check against the limit from the cheap header peek, before the full
    // decode allocates the pixel buffer.
    let (width, height, _header_alpha) = vp8l::decode::peek_header(payload)?;
    let pixels = u64::from(width) * u64::from(height);
    if let Some(limit) = options.max_pixels.filter(|&limit| pixels > limit) {
        return Err(Error::LimitExceeded { pixels, limit });
    }
    let decoded = vp8l::decode::decode(payload)?;
    let dims =
        Dimensions::new(decoded.width, decoded.height).map_err(|_| Error::InvalidBitstream {
            codec: Codec::Lossless,
        })?;
    let has_alpha = image::argb_has_alpha(&decoded.argb);
    let pixels = image::pack_pixels(options.layout, &decoded.argb);
    Ok(Image::from_parts(
        dims,
        options.layout,
        pixels,
        has_alpha,
        Metadata::none(),
    ))
}

/// Whether `bytes` is a well-formed animated WebP. Used to route [`decode_image`]
/// to the animation path; it swallows errors (returning `false`) so the
/// still-image path keeps its exact error behavior for anything not clearly
/// animated.
fn is_animated_file(bytes: &[u8]) -> bool {
    matches!(peek_info(bytes), Ok(Some(info)) if info.is_animated)
}

/// Reject a canvas/image that exceeds `options.max_pixels` *before* any buffer is
/// sized to it. Shared by every header-peek path (still stream, one-shot
/// animation, and the incremental animation walker) so the limit is enforced
/// identically ahead of the pixel/canvas allocation.
pub(crate) fn check_pixel_limit(dims: Dimensions, options: &DecodeOptions) -> Result<()> {
    let pixels = dims.pixel_count();
    if let Some(limit) = options.max_pixels.filter(|&limit| pixels > limit) {
        return Err(Error::LimitExceeded { pixels, limit });
    }
    Ok(())
}

/// Decode an animation's first composited frame as a still [`Image`].
fn decode_first_frame(bytes: &[u8], options: &DecodeOptions) -> Result<Image> {
    Frames::new(Cow::Borrowed(bytes), options.clone(), Vp8lFrameDecoder)?
        .composited()
        .next()
        .ok_or(Error::MissingImage)?
        .map(CompositedFrame::into_image)
}

/// The still-image streaming state: the suspend/resume [`Vp8lStream`], the VP8L
/// chunk's payload range within the buffer, and the accumulated packed rows.
struct StillState {
    /// The suspend/resume VP8L decoder, fed the VP8L payload prefix each push.
    stream: Vp8lStream,
    /// The header peek that drove [`Progress::HeaderReady`] (dims/alpha/metadata).
    info: ImageInfo,
    /// Byte offset of the VP8L chunk payload within the decoder's buffer.
    payload_start: usize,
    /// Declared end of the VP8L payload (`payload_start + chunk size`); may exceed
    /// the buffer until the chunk is fully received.
    payload_end: usize,
    /// The caller's requested output layout.
    layout: PixelLayout,
    /// Image (and output-row) width in pixels.
    width: u32,
    /// Image height in pixels.
    height: u32,
    /// Whether any decoded pixel is non-opaque (accumulated over every row).
    has_alpha: bool,
    /// Whether the stream has decoded every pixel.
    done: bool,
    /// Packed bytes of the currently-retained rows `[packed_base, next_row)`.
    packed: Vec<u8>,
    /// Output-row index of the first row still held in `packed`.
    packed_base: u32,
    /// Total output rows finalized so far (one past the last row in `packed`).
    next_row: u32,
    /// Count of front rows in `packed` handed out by `drain_rows`, freed next push.
    drained: u32,
}

impl core::fmt::Debug for StillState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StillState")
            .field("info", &self.info)
            .field("payload_start", &self.payload_start)
            .field("payload_end", &self.payload_end)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("has_alpha", &self.has_alpha)
            .field("done", &self.done)
            .field("packed_len", &self.packed.len())
            .field("packed_base", &self.packed_base)
            .field("next_row", &self.next_row)
            .field("drained", &self.drained)
            .finish_non_exhaustive()
    }
}

impl StillState {
    /// A fresh stream for the VP8L chunk at `[payload_start, payload_end)`.
    const fn new(
        info: ImageInfo,
        payload_start: usize,
        payload_end: usize,
        layout: PixelLayout,
    ) -> Self {
        Self {
            stream: Vp8lStream::new(),
            width: info.dimensions.width(),
            height: info.dimensions.height(),
            info,
            payload_start,
            payload_end,
            layout,
            has_alpha: false,
            done: false,
            packed: Vec::new(),
            packed_base: 0,
            next_row: 0,
            drained: 0,
        }
    }

    /// Bytes per packed output row.
    const fn row_bytes(&self) -> usize {
        self.width as usize * 4
    }

    /// Drop the rows a previous [`IncrementalDecoder::drain_rows`] handed out,
    /// advancing the retained window. Called at the start of each push.
    fn free_drained(&mut self) {
        if self.drained > 0 {
            let cut = self.drained as usize * self.row_bytes();
            self.packed.drain(..cut);
            self.packed_base += self.drained;
            self.drained = 0;
        }
    }

    /// Borrow the finalized-but-undrained packed rows, marking them drained (they
    /// are freed on the next push). `None` when nothing is pending.
    fn take_drain(&mut self) -> Option<RowDrain<'_>> {
        let pending = self.next_row - self.packed_base - self.drained;
        if pending == 0 {
            return None;
        }
        let offset = self.drained as usize * self.row_bytes();
        let drain = RowDrain::new(
            self.packed_base + self.drained,
            pending,
            self.width,
            self.layout,
            &self.packed[offset..],
        );
        self.drained += pending;
        Some(drain)
    }

    /// Assemble the streamed image with no second pixel decode, folding in the
    /// container's metadata and validating any `VP8X` canvas against the decoded
    /// dimensions (as the one-shot path does).
    ///
    /// Only called on a fully-buffered RIFF (see [`IncrementalDecoder::into_image`]),
    /// so the container re-parse is exhaustive: a malformed-but-complete container
    /// (e.g. a trailing lossy `VP8 ` or `ANMF` chunk) must be rejected here exactly
    /// as a one-shot [`crate::lossless::decode`] would, rather than silently accepted.
    fn assemble(self, buf: &[u8], options: &DecodeOptions) -> Result<Image> {
        let dims =
            Dimensions::new(self.width, self.height).map_err(|_| Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?;
        // A cheap container re-parse (no pixel decode) recovers metadata and the
        // VP8X canvas so we mirror the one-shot canvas check. Errors propagate: the
        // buffer is complete, so a container error is a real one, not a "trailing
        // chunks not yet buffered" artifact (that case never reaches assemble).
        let parsed = parse_container(buf, options.read_metadata)?;
        let vp8x_canvas = parsed.vp8x.map(|vp8x| vp8x.canvas);
        if vp8x_canvas.is_some_and(|canvas| canvas != dims) {
            return Err(Error::InvalidContainer);
        }
        Ok(Image::from_parts(
            dims,
            self.layout,
            self.packed,
            self.has_alpha,
            parsed.metadata,
        ))
    }
}

/// The animation streaming state: the persistent [`Compositor`] plus a byte
/// cursor that walks `ANMF` chunks as they arrive. One whole `ANMF` chunk is
/// decoded and composited per step (frames are small — there is no intra-frame
/// VP8L suspension), emitting [`Progress::FrameComplete`] for each.
#[derive(Debug)]
struct AnimState<D> {
    /// The persistent canvas + blend/dispose bookkeeping, shared with the one-shot
    /// [`crate::lossless::animation::CompositedFrames`] compositor.
    compositor: Compositor,
    /// Decode options (output layout, per-frame pixel limit) for each frame.
    options: DecodeOptions,
    /// Absolute byte offset of the next chunk to examine within the RIFF body.
    cursor: usize,
    /// The most recently composited canvas — the pre-disposal snapshot, identical
    /// to the corresponding one-shot `CompositedFrame`. Retained so the push API
    /// can hand it out per frame through [`IncrementalDecoder::frame_image`].
    latest: Option<Image>,
    /// The per-frame decoder (both-codecs when supplied by the umbrella crate).
    decoder: D,
}

impl<D: FrameDecoder> AnimState<D> {
    /// Set up the walker for a detected animation: an empty canvas sized from the
    /// VP8X header and a cursor at the start of the RIFF body.
    fn new(info: ImageInfo, options: DecodeOptions, decoder: D) -> Self {
        Self {
            compositor: Compositor::new(info.dimensions, options.layout),
            // The RIFF `RIFF....WEBP` header is 12 bytes; the body (VP8X, ANIM,
            // ANMF...) begins right after it, exactly where `body_range` starts.
            cursor: 12,
            options,
            latest: None,
            decoder,
        }
    }

    /// Decode and composite the next fully-buffered `ANMF` frame, advancing the
    /// cursor past it and returning its [`FrameMeta`]. Interleaved metadata /
    /// unknown chunks are skipped. `Ok(None)` means no whole `ANMF` chunk is
    /// available at the cursor yet — the caller waits for more input unless
    /// `complete`, in which case the animation has no more frames.
    fn next_frame(&mut self, buf: &[u8], complete: bool) -> Result<Option<FrameMeta>> {
        // Walk within the declared RIFF body only (clamped to what is buffered),
        // exactly as `animation::Frames` does, so a chunk crossing the declared
        // size is rejected identically once the file is complete.
        let (_start, body_end) = body_range(buf)?;
        let body = &buf[..body_end];
        loop {
            if self.cursor >= body_end {
                return Ok(None);
            }
            match read_chunk_at(body, self.cursor) {
                Ok(None) => return Ok(None),
                Ok(Some((chunk, next))) => {
                    if chunk.id == FourCc::ANMF {
                        let (header, alpha_used, argb) =
                            decode_anmf(chunk.data, &self.decoder, &self.options)?;
                        self.cursor = next;
                        let meta = frame_meta(header);
                        let image = self.compositor.paint(header, alpha_used, &argb)?;
                        self.latest = Some(image);
                        return Ok(Some(meta));
                    }
                    // Skip interleaved metadata / unknown chunks between frames.
                    self.cursor = next;
                },
                Err(err) => {
                    // The next chunk is not wholly within the buffered body. Once
                    // the whole RIFF is buffered this is a genuine truncation;
                    // otherwise simply wait for more bytes.
                    return if complete { Err(err) } else { Ok(None) };
                },
            }
        }
    }
}

/// The streaming path, chosen once the container kind is known. Making `Still` and
/// `Anim` alternatives of one enum (rather than two `Option` fields) means the
/// "a decoder is a still xor an animation" invariant holds by construction.
#[derive(Debug)]
enum Mode<D> {
    /// Not yet classified — buffering until the first image chunk is reachable.
    Undecided,
    /// A still image streamed row-by-row through a `Vp8lStream`.
    Still(StillState),
    /// An animation walked frame-by-frame through an `AnimState`.
    Anim(AnimState<D>),
}

/// A push-based decoder.
///
/// A still image is streamed row-by-row through a `Vp8lStream`; an animation is
/// streamed frame-by-frame through an `AnimState` walker, reporting
/// [`Progress::FrameComplete`] per composited frame; [`Self::into_image`] returns
/// its first composited frame. `Read`-free, so it works on `no_std + alloc`.
#[derive(Debug)]
pub struct IncrementalDecoder<D = Vp8lFrameDecoder> {
    buf: Vec<u8>,
    options: DecodeOptions,
    reported_header: bool,
    image: Option<Image>,
    /// The streaming path once the container kind is known (still xor animation).
    mode: Mode<D>,
    /// The per-frame decoder handed to an [`AnimState`] once an animation is
    /// detected. Defaults to [`Vp8lFrameDecoder`]; the umbrella crate uses a
    /// both-codecs decoder via [`IncrementalDecoder::with_options_and_decoder`].
    /// Chosen at compile time — no dynamic dispatch.
    decoder: D,
}

impl IncrementalDecoder<Vp8lFrameDecoder> {
    /// A new decoder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(DecodeOptions::default())
    }

    /// A new decoder with the given options, decoding animated frames as `VP8L`
    /// (a lossy frame is rejected — inject a decoder with
    /// [`with_options_and_decoder`](Self::with_options_and_decoder) to handle both).
    #[must_use]
    pub const fn with_options(options: DecodeOptions) -> Self {
        Self::with_options_and_decoder(options, Vp8lFrameDecoder)
    }
}

impl<D: FrameDecoder + Clone> IncrementalDecoder<D> {
    /// A new decoder with the given options and a caller-supplied per-frame
    /// [`FrameDecoder`] — the seam the umbrella `webpkit` crate uses so animated
    /// `VP8 ` frames decode without the `lossless` codec depending on the lossy codec.
    #[must_use]
    pub const fn with_options_and_decoder(options: DecodeOptions, decoder: D) -> Self {
        Self {
            buf: Vec::new(),
            options,
            reported_header: false,
            image: None,
            mode: Mode::Undecided,
            decoder,
        }
    }

    /// Feed the next slice of the file and report [`Progress`].
    ///
    /// # Errors
    ///
    /// The same errors as [`crate::lossless::decode`], surfaced as soon as the buffered
    /// bytes make them detectable.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Progress> {
        // Rows handed out by the previous `drain_rows` are freed now (the borrow
        // they returned has ended).
        if let Mode::Still(st) = &mut self.mode {
            st.free_drained();
        }
        if self.image.is_some() {
            return Ok(Progress::Finished);
        }
        self.buf.extend_from_slice(chunk);

        // An animation drives its own ANMF frame walker once set up (on the push
        // that detected it, which reported `HeaderReady`).
        if matches!(self.mode, Mode::Anim(_)) {
            return self.drive_anim();
        }

        // Before a stream is set up, gate on completeness / determine the kind.
        if matches!(self.mode, Mode::Undecided) {
            let outcome = self.begin_or_buffer()?;
            if let Some(progress) = outcome {
                return Ok(progress);
            }
        }

        // Still-image streaming path. Report the header once (peek-driven, so its
        // timing matches the pre-streaming decoder) before streaming rows.
        if !self.reported_header {
            self.reported_header = true;
            if let Mode::Still(st) = &self.mode {
                return Ok(Progress::HeaderReady(st.info));
            }
        }
        self.drive_still()
    }

    /// Handle the state before a still stream is set up: finish immediately once
    /// the whole RIFF is buffered, otherwise determine the kind. Returns
    /// `Some(progress)` to return at once, or `None` once a still stream has been
    /// installed (the caller then reports the header and streams).
    fn begin_or_buffer(&mut self) -> Result<Option<Progress>> {
        // Detect a well-formed animation as soon as its VP8X header is buffered
        // and route it to the incremental ANMF frame walker. Checked *before* the
        // `is_complete` one-shot short-circuit below so a whole-file push still
        // emits a `FrameComplete` per frame. A peek *error* is deliberately not
        // propagated here — it falls through to the one-shot path, preserving the
        // exact error semantics of a malformed complete file.
        if let Ok(Some(info)) = peek_info(&self.buf)
            && info.is_animated
        {
            // Enforce the pixel limit against the peeked canvas *before*
            // `AnimState::new` sizes (and allocates) the compositor canvas.
            check_pixel_limit(info.dimensions, &self.options)?;
            self.reported_header = true;
            self.mode = Mode::Anim(AnimState::new(
                info,
                self.options.clone(),
                self.decoder.clone(),
            ));
            return Ok(Some(Progress::HeaderReady(info)));
        }
        // A complete RIFF that arrived before streaming was set up (a whole-file
        // single push or a malformed container) takes the one-shot path,
        // preserving its exact `Finished`/error behavior.
        if is_complete(&self.buf) {
            self.image = Some(decode_image(&self.buf, &self.options)?);
            return Ok(Some(Progress::Finished));
        }
        let Some(info) = peek_info(&self.buf)? else {
            return Ok(Some(Progress::NeedMoreInput));
        };
        // `info` is a still image here — animations returned above.
        // Enforce the pixel limit against the peeked dimensions *before* the
        // stream allocates its coded buffer.
        check_pixel_limit(info.dimensions, &self.options)?;
        let Some((start, end)) = locate_vp8l(&self.buf) else {
            return Ok(Some(Progress::NeedMoreInput));
        };
        self.mode = Mode::Still(StillState::new(info, start, end, self.options.layout));
        Ok(None)
    }

    /// Drive the still stream over the buffered VP8L payload, packing every
    /// newly-finalized row and returning the resulting [`Progress`].
    fn drive_still(&mut self) -> Result<Progress> {
        let complete = is_complete(&self.buf);
        let Mode::Still(st) = &mut self.mode else {
            return Ok(Progress::NeedMoreInput);
        };
        // The VP8L chunk is `final` once fully buffered — or once the whole RIFF
        // is, so a chunk truncated within a complete file surfaces as `Truncated`.
        let final_input = self.buf.len() >= st.payload_end || complete;
        let end = self.buf.len().min(st.payload_end);
        let payload = &self.buf[st.payload_start..end];
        let width = st.width as usize;

        let mut rows_first: Option<u32> = None;
        let mut rows_count = 0u32;
        loop {
            match st.stream.advance(payload, final_input)? {
                Step::Header(w, h, alpha) => {
                    st.width = w;
                    st.height = h;
                    // The peek already drove `HeaderReady`; this only transitions
                    // the stream into its streaming phase. Reporting here is a
                    // belt-and-suspenders fallback should the peek path be skipped.
                    if !self.reported_header {
                        self.reported_header = true;
                        let info = ImageInfo::new(
                            Dimensions::new(w, h).map_err(|_| Error::InvalidBitstream {
                                codec: Codec::Lossless,
                            })?,
                            alpha || st.info.has_alpha,
                            st.info.has_metadata,
                            false,
                        );
                        st.info = info;
                        return Ok(Progress::HeaderReady(info));
                    }
                },
                Step::Rows { first_row, count } => {
                    let start = first_row as usize * width;
                    let stop = (first_row + count) as usize * width;
                    let rows_argb = &st.stream.ready()[start..stop];
                    st.has_alpha = st.has_alpha || image::argb_has_alpha(rows_argb);
                    let bytes = image::pack_pixels(st.layout, rows_argb);
                    st.packed.extend_from_slice(&bytes);
                    st.next_row = first_row + count;
                    if rows_first.is_none() {
                        rows_first = Some(first_row);
                    }
                    rows_count += count;
                },
                Step::NeedMore => break,
                Step::Done => {
                    st.done = true;
                    break;
                },
            }
        }

        // `Finished` only once every pixel is in *and* the whole RIFF is buffered,
        // so trailing metadata and the VP8X-canvas check are preserved. Otherwise
        // report the rows finalized on this push, then plain progress.
        if st.done && complete {
            return Ok(Progress::Finished);
        }
        if let Some(first_row) = rows_first {
            return Ok(Progress::RowsDecoded {
                first_row,
                count: rows_count,
            });
        }
        Ok(Progress::NeedMoreInput)
    }

    /// Drive the animation walker over the buffered `ANMF` chunks: composite the
    /// next whole frame if one is available and report [`Progress::FrameComplete`],
    /// otherwise wait for more input or — once the whole RIFF is buffered and every
    /// frame is composited — report the single terminal [`Progress::Finished`].
    fn drive_anim(&mut self) -> Result<Progress> {
        let complete = is_complete(&self.buf);
        // Disjoint field borrows: `buf` read-only, `anim` mutable. `drive_anim` is
        // only reached with the walker installed, but handle its absence gracefully
        // rather than panic.
        let next = {
            let buf = &self.buf;
            let Mode::Anim(anim) = &mut self.mode else {
                return Ok(Progress::NeedMoreInput);
            };
            anim.next_frame(buf, complete)?
        };
        if let Some(meta) = next {
            return Ok(Progress::FrameComplete(meta));
        }
        // No more whole `ANMF` chunk at the cursor. If the file is complete every
        // frame has been composited: report `Finished` (`into_image` then returns
        // the first composited frame via the one-shot path). Otherwise wait.
        if complete {
            Ok(Progress::Finished)
        } else {
            Ok(Progress::NeedMoreInput)
        }
    }

    /// The most-recently composited animation frame (the pre-disposal canvas
    /// snapshot), available right after a [`Progress::FrameComplete`] push and
    /// replaced by the next frame. `None` before the first frame or for a still
    /// image. Mirrors [`Self::drain_rows`] for the animation path: a non-consuming
    /// borrow of the current canvas; use [`Self::into_image`] for the final image.
    ///
    /// ```
    /// use webpkit::{
    ///     AnimationEncoder, BlendMode, Dimensions, DisposalMode, FrameMeta, ImageRef,
    ///     IncrementalDecoder, PixelLayout, Progress,
    /// };
    ///
    /// // A tiny 2-frame 2x2 animation (red, then blue).
    /// let canvas = Dimensions::new(2, 2).unwrap();
    /// let red = [255u8, 0, 0, 255].repeat(4);
    /// let blue = [0u8, 0, 255, 255].repeat(4);
    /// let meta = |ms| FrameMeta {
    ///     x: 0,
    ///     y: 0,
    ///     dimensions: canvas,
    ///     duration_ms: ms,
    ///     blend: BlendMode::Blend,
    ///     dispose: DisposalMode::Keep,
    /// };
    /// let bytes = AnimationEncoder::new(canvas)
    ///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &red).unwrap(), meta(100))
    ///     .unwrap()
    ///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &blue).unwrap(), meta(100))
    ///     .unwrap()
    ///     .finish();
    ///
    /// // Feed the file, then drive one frame per push: each `FrameComplete`
    /// // exposes the freshly composited canvas.
    /// let mut dec = IncrementalDecoder::new();
    /// dec.push(&bytes).unwrap(); // buffers the whole file, reports HeaderReady
    /// let mut frames = 0;
    /// loop {
    ///     match dec.push(&[]).unwrap() {
    ///         Progress::FrameComplete(_) => {
    ///             assert!(dec.frame_image().is_some());
    ///             frames += 1;
    ///         }
    ///         Progress::Finished => break,
    ///         _ => {}
    ///     }
    /// }
    /// assert_eq!(frames, 2);
    /// ```
    #[must_use]
    pub const fn frame_image(&self) -> Option<&Image> {
        match &self.mode {
            Mode::Anim(anim) => anim.latest.as_ref(),
            _ => None,
        }
    }

    /// Borrow the finalized-but-not-yet-viewed rows of a streamed still image, in
    /// the requested [`PixelLayout`], as a non-consuming early view. `None` if none
    /// are pending or the input is not a still image. The borrowed bytes are freed
    /// on the next [`Self::push`], yet [`Self::into_image`] still returns the
    /// complete image (see [`RowDrain`]).
    pub fn drain_rows(&mut self) -> Option<RowDrain<'_>> {
        match &mut self.mode {
            Mode::Still(st) => st.take_drain(),
            _ => None,
        }
    }

    /// Retrieve the **complete** decoded image once [`Progress::Finished`] has been
    /// reported.
    ///
    /// Draining rows via [`Self::drain_rows`] is a non-consuming early view: it does
    /// not remove rows from the image this returns. When the whole RIFF is buffered
    /// and no rows were drained-and-freed, the image is assembled from the retained
    /// rows with no second decode; otherwise (incomplete input, or rows already
    /// freed after a drain) it falls back to a one-shot decode of the buffered bytes
    /// — which reconstructs the full image, or errors (e.g. [`Error::Truncated`], or
    /// a malformed-but-complete container) exactly as [`crate::lossless::decode`] would.
    ///
    /// # Errors
    ///
    /// The same errors as [`crate::lossless::decode`] when the buffer is not a fully-decoded
    /// still image.
    pub fn into_image(self) -> Result<Image> {
        let Self {
            buf,
            options,
            image,
            mode,
            ..
        } = self;
        if let Some(image) = image {
            return Ok(image);
        }
        match mode {
            // Fast-path only when the whole RIFF is buffered AND no drained rows
            // were freed (so `packed` still holds every row). The result is then
            // byte-identical to a one-shot decode, including rejecting a
            // complete-but-invalid trailing container. Otherwise defer to the
            // one-shot decode, which reconstructs any freed rows from `buf` or
            // surfaces the same error.
            Mode::Still(st) if st.done && st.packed_base == 0 && is_complete(&buf) => {
                st.assemble(&buf, &options)
            },
            _ => decode_image(&buf, &options),
        }
    }
}

impl Default for IncrementalDecoder<Vp8lFrameDecoder> {
    fn default() -> Self {
        Self::new()
    }
}

/// Locate the top-level `VP8L` chunk's payload range `[start, end)` once its
/// 8-byte chunk header is buffered. `end` is the declared payload end
/// (`start + size`), which may exceed the buffer until the chunk is fully
/// received. `None` until the `VP8L` header is reachable.
///
/// The container magic and shape are already validated by [`peek_info`] before a
/// still stream is set up, so this only needs to find the offset.
fn locate_vp8l(bytes: &[u8]) -> Option<(usize, usize)> {
    scan_chunks(bytes)
        .find(|chunk| chunk.id == FourCc::VP8L)
        .map(|chunk| (chunk.payload_start, chunk.payload_end))
}

/// Peek the image header from a (possibly partial) buffer, walking chunks until
/// the `VP8L` header is reachable. `Ok(None)` means more bytes are needed.
fn peek_info(bytes: &[u8]) -> Result<Option<ImageInfo>> {
    if bytes.len() < 12 {
        return Ok(None);
    }
    if bytes[0..4] != FourCc::RIFF.0 || bytes[8..12] != FourCc::WEBP.0 {
        return Err(Error::NotWebp);
    }
    let mut has_metadata = false;
    let mut vp8x_alpha = false;
    for chunk in scan_chunks(bytes) {
        match chunk.id {
            FourCc::VP8X => {
                let Some(data) =
                    bytes.get(chunk.payload_start..chunk.payload_start + VP8X_PAYLOAD_LEN)
                else {
                    return Ok(None);
                };
                let info = Vp8xInfo::parse(data)?;
                has_metadata =
                    info.flags.has_icc() || info.flags.has_exif() || info.flags.has_xmp();
                vp8x_alpha = info.flags.has_alpha();
                // An animation's header is fully described by the VP8X canvas;
                // report it now (there is no top-level VP8L chunk to find).
                if info.flags.is_animated() {
                    return Ok(Some(ImageInfo::new(
                        info.canvas,
                        vp8x_alpha,
                        has_metadata,
                        true,
                    )));
                }
            },
            FourCc::VP8L => {
                return peek_vp8l_info(bytes.get(chunk.payload_start..), has_metadata, vp8x_alpha);
            },
            FourCc::VP8 => return Err(Error::UnsupportedFeature),
            // Animation chunks without a preceding animated VP8X are malformed
            // (a well-formed animation returns above).
            FourCc::ANIM | FourCc::ANMF => return Err(Error::InvalidContainer),
            _ => {},
        }
    }
    Ok(None)
}

/// Build [`ImageInfo`] from a VP8L payload once at least its header is present.
/// `has_alpha` combines the VP8L header advisory with any VP8X alpha flag.
fn peek_vp8l_info(
    payload: Option<&[u8]>,
    has_metadata: bool,
    vp8x_alpha: bool,
) -> Result<Option<ImageInfo>> {
    let Some(payload) = payload else {
        return Ok(None);
    };
    if payload.len() < 5 {
        return Ok(None); // VP8L header is not fully buffered yet
    }
    let (width, height, header_alpha) = vp8l::decode::peek_header(payload)?;
    let dimensions = Dimensions::new(width, height).map_err(|_| Error::InvalidBitstream {
        codec: Codec::Lossless,
    })?;
    Ok(Some(ImageInfo::new(
        dimensions,
        header_alpha || vp8x_alpha,
        has_metadata,
        false,
    )))
}

/// A `std` decoder that reads a whole WebP from an [`std::io::Read`] source.
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct Decoder<R> {
    reader: R,
    options: DecodeOptions,
    buf: Option<Vec<u8>>,
}

#[cfg(feature = "std")]
impl<R: Read> Decoder<R> {
    /// A decoder reading from `reader` with default options.
    pub fn new(reader: R) -> Self {
        Self::with_options(reader, DecodeOptions::default())
    }

    /// A decoder reading from `reader` with the given options.
    pub const fn with_options(reader: R, options: DecodeOptions) -> Self {
        Self {
            reader,
            options,
            buf: None,
        }
    }

    /// Read the whole source and report the image header.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a read failure, or a container error for a malformed file.
    pub fn read_info(&mut self) -> Result<ImageInfo> {
        self.fill()?;
        let bytes = self.buf.as_deref().unwrap_or(&[]);
        peek_info(bytes)?.ok_or(Error::Truncated)
    }

    /// Read the whole source and decode it into an [`Image`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a read failure, or any [`crate::lossless::decode`] error.
    pub fn decode(mut self) -> Result<Image> {
        let bytes = if let Some(bytes) = self.buf.take() {
            bytes
        } else {
            let mut bytes = Vec::new();
            self.reader.read_to_end(&mut bytes)?;
            bytes
        };
        decode_image(&bytes, &self.options)
    }

    /// Read the entire source and return a lazy [`Frames`] iterator over an
    /// animation's frames.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a read failure, [`Error::UnsupportedFeature`] if the
    /// file is not an animation, or a container error for a malformed file.
    pub fn into_frames(mut self) -> Result<Frames<'static>> {
        let bytes = if let Some(bytes) = self.buf.take() {
            bytes
        } else {
            let mut bytes = Vec::new();
            self.reader.read_to_end(&mut bytes)?;
            bytes
        };
        Frames::new(Cow::Owned(bytes), self.options, Vp8lFrameDecoder)
    }

    /// Read the entire source into `self.buf` if not already done.
    fn fill(&mut self) -> Result<()> {
        if self.buf.is_none() {
            let mut bytes = Vec::new();
            self.reader.read_to_end(&mut bytes)?;
            self.buf = Some(bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        reason = "test fixtures build container byte lengths with casts that fit their targets \
                  by construction"
    )]

    use super::{DecodeOptions, IncrementalDecoder, Progress, decode_image};
    use crate::container::fourcc::FourCc;
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::container::writer::{push_chunk, riff_envelope, wrap, wrap_vp8l};
    use crate::error::Error;
    use crate::image::{Dimensions, Metadata, PixelLayout};
    use crate::lossless::vp8l;

    /// Encode a small solid image to a full WebP file for decoder tests.
    fn sample_webp(width: u32, height: u32) -> Vec<u8> {
        let argb: Vec<u32> = (0..width * height).map(|i| 0xff00_0000 | i).collect();
        wrap_vp8l(&vp8l::encode::encode(width, height, &argb))
    }

    #[test]
    fn decode_image_matches_layout() {
        let file = sample_webp(4, 4);
        let rgba = decode_image(&file, &DecodeOptions::default()).unwrap();
        let bgra =
            decode_image(&file, &DecodeOptions::default().layout(PixelLayout::Bgra8)).unwrap();
        assert_eq!(rgba.dimensions(), bgra.dimensions());
        // The same pixel, re-ordered: RGBA[0..4] vs BGRA[0..4].
        let r = rgba.as_bytes();
        let b = bgra.as_bytes();
        assert_eq!([r[0], r[1], r[2], r[3]], [b[2], b[1], b[0], b[3]]);
    }

    #[test]
    fn max_pixels_rejects_before_decode() {
        let file = sample_webp(8, 8);
        let opts = DecodeOptions::default().max_pixels(10);
        assert_eq!(
            decode_image(&file, &opts).unwrap_err(),
            Error::LimitExceeded {
                pixels: 64,
                limit: 10
            }
        );
    }

    #[test]
    fn max_pixels_rejects_still_incremental_before_stream() {
        // An 8x8 (64px) VP8L still fed incrementally: the still streaming path must
        // reject it via `check_pixel_limit` — before the stream is set up — as soon
        // as the header is peekable, matching the one-shot and animation guards.
        let file = sample_webp(8, 8);
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(10));
        let mut err = None;
        for byte in &file {
            if let Err(e) = dec.push(core::slice::from_ref(byte)) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(
            err,
            Some(Error::LimitExceeded {
                pixels: 64,
                limit: 10,
            })
        );
    }

    #[test]
    fn max_pixels_rejects_animated_canvas_before_alloc() {
        // A ~30-byte animated VP8X header declaring a 16384x16384 canvas: the
        // incremental animation path must enforce `max_pixels` *before* it sizes
        // the compositor canvas (which would otherwise allocate ~1 GiB), matching
        // the still and one-shot paths.
        let canvas = Dimensions::new(16384, 16384).unwrap();
        let flags = Vp8xFlags::for_output(&Metadata::none(), false).with_animation();
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, canvas));
        let file = riff_envelope(&body);

        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(4));
        assert_eq!(
            dec.push(&file).unwrap_err(),
            Error::LimitExceeded {
                pixels: 16384 * 16384,
                limit: 4,
            }
        );
    }

    #[test]
    fn incremental_equals_one_shot_over_splits() {
        let file = sample_webp(7, 5);
        let one_shot = decode_image(&file, &DecodeOptions::default()).unwrap();
        // Feed the file one byte at a time — the hardest split.
        let mut dec = IncrementalDecoder::new();
        let mut finished = false;
        for byte in &file {
            if matches!(dec.push(&[*byte]).unwrap(), Progress::Finished) {
                finished = true;
            }
        }
        assert!(finished, "must finish once all bytes are pushed");
        assert_eq!(dec.into_image().unwrap(), one_shot);
    }

    #[test]
    fn incremental_reports_header_before_finish() {
        let file = sample_webp(6, 3);
        let mut dec = IncrementalDecoder::new();
        // Push everything but the last byte: header should be known, not finished.
        let split = file.len() - 1;
        let mut saw_header = false;
        for byte in &file[..split] {
            if let Progress::HeaderReady(info) = dec.push(&[*byte]).unwrap() {
                saw_header = true;
                assert_eq!((info.dimensions.width(), info.dimensions.height()), (6, 3));
            }
        }
        assert!(
            saw_header,
            "a bare VP8L file exposes its header before completion"
        );
    }

    #[test]
    fn incremental_into_image_errors_when_incomplete() {
        let file = sample_webp(2, 2);
        let mut dec = IncrementalDecoder::new();
        let _ = dec.push(&file[..8]); // only the RIFF header
        assert!(dec.into_image().is_err());
    }

    #[test]
    fn tiny_riff_size_does_not_finish_early() {
        // RIFF size 2 -> declared length 10 (< 12). The incremental decoder must
        // not report Finished on a sub-header prefix; its result must match
        // one-shot rather than diverge by chunking.
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&2u32.to_le_bytes());
        file.extend_from_slice(b"WE");
        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            assert!(!matches!(dec.push(&[*byte]), Ok(Progress::Finished)));
        }
        assert_eq!(
            dec.into_image().is_err(),
            decode_image(&file, &DecodeOptions::default()).is_err()
        );
    }

    #[test]
    fn drained_rows_reassemble_to_one_shot() {
        // Push a still image in uneven chunks, draining rows at (most) push
        // boundaries; the concatenation of every drained batch — in the requested
        // layout — must equal the one-shot decoded pixels.
        let file = sample_webp(9, 7);
        let one_shot = decode_image(&file, &DecodeOptions::default()).unwrap();

        let mut dec = IncrementalDecoder::new();
        let mut collected: Vec<u8> = Vec::new();
        let steps = [3usize, 1, 5, 2, 11, 4, 7];
        let mut cursor = 0usize;
        let mut i = 0usize;
        while cursor < file.len() {
            let end = (cursor + steps[i % steps.len()]).min(file.len());
            dec.push(&file[cursor..end]).unwrap();
            cursor = end;
            // Skip the drain every third push so several row batches accumulate
            // before a single drain hands them all out at once.
            let batch = if i.is_multiple_of(3) {
                None
            } else {
                dec.drain_rows()
            };
            if let Some(drain) = batch {
                collected.extend_from_slice(drain.as_bytes());
            }
            i += 1;
        }
        // Flush whatever rows are still retained after completion.
        while let Some(drain) = dec.drain_rows() {
            collected.extend_from_slice(drain.as_bytes());
        }
        assert_eq!(collected, one_shot.as_bytes());
    }

    #[test]
    fn progress_sequence_is_monotonic() {
        // A VP8X file with trailing EXIF: the VP8L payload completes strictly
        // before the RIFF does, so the push finalizing the last rows reports
        // `RowsDecoded` and a later push (trailing metadata) reports the single
        // `Finished` — the row payout is never swallowed and sums to the height.
        let (w, h) = (5u32, 8u32);
        let argb: Vec<u32> = (0..w * h)
            .map(|i| 0xff00_0000 | (i.wrapping_mul(7)))
            .collect();
        let payload = vp8l::encode::encode(w, h, &argb);
        let dims = Dimensions::new(w, h).unwrap();
        let metadata = Metadata {
            exif: Some(vec![1, 2, 3, 4, 5, 6]),
            ..Metadata::none()
        };
        let file = wrap(&payload, dims, &metadata, false);
        let one_shot = decode_image(&file, &DecodeOptions::default()).unwrap();

        let mut dec = IncrementalDecoder::new();
        let mut header_at: Option<usize> = None;
        let mut first_rows_at: Option<usize> = None;
        let mut rows: Vec<(u32, u32)> = Vec::new();
        let mut finished = 0usize;
        for (event, byte) in file.iter().enumerate() {
            match dec.push(&[*byte]).unwrap() {
                Progress::HeaderReady(info) => {
                    assert!(header_at.is_none(), "header reported more than once");
                    assert_eq!((info.dimensions.width(), info.dimensions.height()), (w, h));
                    header_at = Some(event);
                },
                Progress::RowsDecoded { first_row, count } => {
                    first_rows_at.get_or_insert(event);
                    rows.push((first_row, count));
                },
                Progress::FrameComplete(_) => panic!("a still image must not report FrameComplete"),
                Progress::Finished => finished += 1,
                _ => {},
            }
        }

        let header_event = header_at.expect("the header must be reported");
        if let Some(rows_event) = first_rows_at {
            assert!(
                header_event < rows_event,
                "HeaderReady must precede every RowsDecoded"
            );
        }
        // Rows are contiguous from 0 and sum to the image height.
        let mut expected = 0u32;
        for (first_row, count) in &rows {
            assert_eq!(*first_row, expected, "row payout is not contiguous");
            expected += count;
        }
        assert_eq!(expected, h, "reported rows must sum to the height");
        assert_eq!(finished, 1, "exactly one Finished");
        // Never drained here, so the assembled image equals one-shot.
        assert_eq!(dec.into_image().unwrap(), one_shot);
    }

    #[test]
    fn frame_complete_fires_per_frame_over_splits() {
        use super::{FrameMeta, Image};

        // The committed 16x16 x3-frame lossless animation (img2webp 1.6.0).
        let file: &[u8] = include_bytes!(
            "../../../webpkit-lossless-conformance/fixtures/decode/animation_frames/input.webp"
        );

        // Batch reference: the one-shot composited canvases (pre-disposal
        // snapshots) and, independently, the per-frame metas.
        let batch: Vec<_> = crate::lossless::decode_frames(file)
            .unwrap()
            .composited()
            .map(Result::unwrap)
            .collect();
        let metas: Vec<FrameMeta> = crate::lossless::decode_frames(file)
            .unwrap()
            .map(|frame| frame.unwrap().meta())
            .collect();
        assert!(batch.len() >= 2, "fixture should have several frames");
        assert_eq!(batch.len(), metas.len());

        // Drive the incremental decoder over an uneven split, feeding empty slices
        // once the file is exhausted to drain frames that are fully buffered but
        // not yet handed out (one frame is composited per push).
        let steps = [7usize, 3, 13, 1, 5, 29, 2];
        let mut dec = IncrementalDecoder::new();
        let mut header_at: Option<usize> = None;
        let mut first_frame_at: Option<usize> = None;
        let mut got_metas: Vec<FrameMeta> = Vec::new();
        let mut got_frames: Vec<Image> = Vec::new();
        let (mut cursor, mut si, mut step) = (0usize, 0usize, 0usize);
        loop {
            step += 1;
            assert!(step < file.len() + 1000, "driver failed to terminate");
            let feed: &[u8] = if cursor < file.len() {
                let end = (cursor + steps[si % steps.len()]).min(file.len());
                si += 1;
                let slice = &file[cursor..end];
                cursor = end;
                slice
            } else {
                &[]
            };
            match dec.push(feed).unwrap() {
                Progress::RowsDecoded { .. } => panic!("an animation must not report RowsDecoded"),
                Progress::HeaderReady(info) => {
                    assert!(header_at.is_none(), "header reported more than once");
                    assert!(info.is_animated, "the fixture is an animation");
                    assert_eq!(info.dimensions, batch[0].image().dimensions());
                    header_at = Some(step);
                },
                Progress::FrameComplete(meta) => {
                    first_frame_at.get_or_insert(step);
                    got_metas.push(meta);
                    // Snapshot the incremental canvas the instant the frame
                    // completes (public per-frame borrow-getter).
                    got_frames.push(dec.frame_image().expect("canvas after a frame").clone());
                },
                // Stop at the first `Finished`: it is the single terminal event
                // (no earlier arm reports it), so reaching past the loop proves
                // exactly one `Finished` was reported, after every frame.
                Progress::Finished => break,
                _ => {},
            }
        }

        // HeaderReady precedes every frame; the loop's only non-panic exit above
        // is the terminal Finished.
        let header = header_at.expect("HeaderReady must be reported");
        let first_frame = first_frame_at.expect("at least one FrameComplete");
        assert!(header < first_frame, "HeaderReady must precede the frames");

        // Exactly one FrameComplete per frame, each carrying the expected meta and
        // a composited canvas byte-identical to the batch compositor.
        assert_eq!(
            got_metas.len(),
            batch.len(),
            "exactly one FrameComplete per frame"
        );
        for (i, (meta, frame)) in got_metas.iter().zip(&got_frames).enumerate() {
            assert_eq!(*meta, metas[i], "frame {i} meta mismatch");
            assert_eq!(
                frame,
                batch[i].image(),
                "frame {i} incremental composited canvas must match the batch compositor"
            );
        }

        // `into_image` still returns the first composited frame (unchanged).
        assert_eq!(dec.into_image().unwrap(), *batch[0].image());
    }

    #[test]
    fn into_image_returns_complete_image_after_draining() {
        // drain_rows is a non-consuming early view: into_image still returns the
        // COMPLETE image, whether or not rows were drained and whether or not a
        // later push freed them. Byte-identical to one-shot in both cases.
        let file = sample_webp(6, 9);
        let one_shot = decode_image(&file, &DecodeOptions::default()).unwrap();

        // (a) Terminal drain: stream to completion, drain every row, then call
        // into_image with NO intervening push — the drained rows must be neither
        // dropped nor double-counted (the regression the fix targets).
        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            dec.push(&[*byte]).unwrap();
        }
        let mut drained: Vec<u8> = Vec::new();
        while let Some(d) = dec.drain_rows() {
            drained.extend_from_slice(d.as_bytes());
        }
        assert_eq!(
            drained,
            one_shot.as_bytes(),
            "the drain view covers every row once"
        );
        assert_eq!(
            dec.into_image().unwrap(),
            one_shot,
            "into_image is still complete after draining every row"
        );

        // (b) Drain mid-stream then keep pushing, so free_drained runs and advances
        // packed_base: into_image re-decodes the freed rows from the buffer.
        let mut dec = IncrementalDecoder::new();
        let half = file.len() / 2;
        dec.push(&file[..half]).unwrap();
        let _ = dec.drain_rows();
        dec.push(&file[half..]).unwrap();
        assert_eq!(dec.into_image().unwrap(), one_shot);
    }

    #[test]
    fn into_image_rejects_complete_but_invalid_container_when_streamed() {
        // A valid VP8L followed by an unsupported trailing chunk (lossy `VP8 `),
        // all within riff_size. One-shot decode rejects it; the streamed path —
        // which decodes the VP8L fine and only meets the bad chunk at into_image —
        // must reject it identically, not silently accept a complete-but-invalid
        // file (the streaming-vs-one-shot divergence the fix targets).
        let mut file = sample_webp(4, 4);
        let extra: &[u8] = &[b'V', b'P', b'8', b' ', 4, 0, 0, 0, 0, 0, 0, 0];
        let riff = u32::from_le_bytes(file[4..8].try_into().unwrap());
        file[4..8].copy_from_slice(&(riff + u32::try_from(extra.len()).unwrap()).to_le_bytes());
        file.extend_from_slice(extra);

        let one_shot = decode_image(&file, &DecodeOptions::default());
        assert!(
            one_shot.is_err(),
            "one-shot must reject the trailing lossy chunk"
        );

        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            let _ = dec.push(&[*byte]);
        }
        assert_eq!(
            dec.into_image().unwrap_err(),
            one_shot.unwrap_err(),
            "the streamed path must reject the same complete-but-invalid file"
        );
    }

    #[test]
    fn max_pixels_allows_exactly_the_limit() {
        // Boundary: pixels == limit is accepted (`pixels > limit` is false). The
        // `> -> >=` mutant in `decode_image` would reject it.
        let file = sample_webp(8, 8); // 64 pixels
        let opts = DecodeOptions::default().max_pixels(64);
        let img = decode_image(&file, &opts).unwrap();
        assert_eq!(
            (img.dimensions().width(), img.dimensions().height()),
            (8, 8)
        );
    }

    #[test]
    fn check_pixel_limit_allows_exactly_the_limit_incremental() {
        // The still incremental path routes through `check_pixel_limit`; at the
        // boundary pixels == limit it must accept (`pixels > limit` false). The
        // `> -> >=` mutant there would reject it before the stream is set up.
        let file = sample_webp(8, 8); // 64 pixels
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(64));
        let mut err = None;
        for byte in &file {
            if let Err(e) = dec.push(core::slice::from_ref(byte)) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(err, None, "pixels == limit must be accepted, not rejected");
        let img = dec.into_image().unwrap();
        assert_eq!(
            (img.dimensions().width(), img.dimensions().height()),
            (8, 8)
        );
    }

    #[test]
    fn still_state_debug_reports_its_fields() {
        // The `StillState` `Debug` impl must render its fields (the decoder derives
        // `Debug` and holds a `Some(StillState { .. })`). Replacing `fmt` with
        // `Ok(Default::default())` writes nothing, so the name/fields vanish.
        let file = sample_webp(5, 5); // bare VP8L: payload starts at offset 20
        let mut dec = IncrementalDecoder::new();
        for byte in &file[..file.len() - 1] {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        let debug = format!("{dec:?}");
        assert!(
            debug.contains("StillState"),
            "still-state name is rendered: {debug}"
        );
        assert!(
            debug.contains("payload_start: 20"),
            "payload_start field: {debug}"
        );
        assert!(debug.contains("width: 5"), "width field: {debug}");
    }

    #[test]
    fn free_drained_resets_the_drained_counter_on_push() {
        // free_drained (run at the start of each push) frees the rows a prior
        // drain handed out: it advances packed_base and resets `drained` to 0. A
        // no-op body (delete, `drained < 0`, or `drained == 0`) leaves `drained`
        // non-zero and packed_base at 0 — visible in the Debug snapshot.
        let file = sample_webp(4, 16);
        let mut dec = IncrementalDecoder::new();
        let mut i = 0usize;
        let mut drained_rows = false;
        while i < file.len() {
            dec.push(core::slice::from_ref(&file[i])).unwrap();
            i += 1;
            if dec.drain_rows().is_some() {
                drained_rows = true;
                break;
            }
        }
        assert!(
            drained_rows,
            "a row must drain mid-stream before completion"
        );
        assert!(
            i < file.len(),
            "bytes must remain to push after the first drain"
        );
        // The next push runs free_drained over the outstanding drained rows.
        dec.push(core::slice::from_ref(&file[i])).unwrap();
        let debug = format!("{dec:?}");
        assert!(
            debug.contains("drained: 0"),
            "free_drained resets the drained counter: {debug}"
        );
        assert!(
            !debug.contains("packed_base: 0,"),
            "free_drained advances packed_base off zero: {debug}"
        );
    }

    #[test]
    fn drain_reports_absolute_first_row_after_freeing() {
        // Each drain batch's `first_row` is its absolute output-row index =
        // packed_base + drained (drained is 0 at every drain). Once free_drained
        // advances packed_base past 0, `first_row` is packed_base; the
        // `packed_base + drained -> packed_base * drained` mutant collapses it to 0.
        let file = sample_webp(4, 16);
        let mut dec = IncrementalDecoder::new();
        let mut expected = 0u32;
        let mut saw_nonzero_first = false;
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
            if let Some(drain) = dec.drain_rows() {
                assert_eq!(
                    drain.first_row, expected,
                    "drain batches must be contiguous from 0"
                );
                saw_nonzero_first |= drain.first_row > 0;
                expected += drain.rows;
            }
        }
        while let Some(drain) = dec.drain_rows() {
            assert_eq!(
                drain.first_row, expected,
                "trailing drain batches stay contiguous"
            );
            saw_nonzero_first |= drain.first_row > 0;
            expected += drain.rows;
        }
        assert_eq!(expected, 16, "every row drained exactly once");
        assert!(
            saw_nonzero_first,
            "a drain after freeing must report a non-zero absolute first_row"
        );
    }

    #[test]
    fn drive_still_treats_a_complete_riff_as_final_input() {
        // A VP8L chunk declaring more bytes than are present, inside a RIFF whose
        // declared length exactly covers the (short) file. Streaming reaches the
        // completing push with buf.len() < payload_end yet the file complete, so
        // final_input must be true (`>= payload_end` OR `complete`), surfacing the
        // truncated bitstream as an error. The `|| -> &&` mutant would keep
        // final_input false and never error.
        let (w, h) = (12u32, 12u32);
        let argb: Vec<u32> = (0..w * h)
            .map(|i| 0xff00_0000 | i.wrapping_mul(2_654_435_761))
            .collect();
        let full = vp8l::encode::encode(w, h, &argb);
        assert!(
            full.len() > 12,
            "payload long enough to truncate meaningfully"
        );
        let chopped = &full[..full.len() - 8];

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        let riff_size = (4 + 8 + chopped.len()) as u32; // "WEBP" + VP8L header(8) + chopped
        file.extend_from_slice(&riff_size.to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(b"VP8L");
        file.extend_from_slice(&(full.len() as u32).to_le_bytes()); // declares the FULL length
        file.extend_from_slice(chopped);

        let mut dec = IncrementalDecoder::new();
        let mut err = None;
        for byte in &file {
            if let Err(e) = dec.push(core::slice::from_ref(byte)) {
                err = Some(e);
                break;
            }
        }
        assert!(
            err.is_some(),
            "a VP8L bitstream truncated within a complete RIFF must error"
        );
    }

    #[test]
    fn streamed_image_accumulates_alpha_across_rows() {
        // A non-opaque still, streamed to completion without draining (packed_base
        // stays 0, so into_image assembles from retained rows using the accumulated
        // has_alpha). `has_alpha || argb_has_alpha(row)` must latch true; the
        // `|| -> &&` mutant keeps it false.
        let (w, h) = (4u32, 6u32);
        let argb: Vec<u32> = (0..w * h).map(|i| 0x8000_0000 | i).collect();
        let file = wrap_vp8l(&vp8l::encode::encode(w, h, &argb));
        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        let img = dec.into_image().unwrap();
        assert!(
            img.has_alpha(),
            "a streamed non-opaque image reports has_alpha"
        );
    }

    #[test]
    fn into_image_redecodes_after_all_rows_freed() {
        // Trailing EXIF makes the VP8L payload complete strictly before the RIFF,
        // so draining every push and then pushing the metadata frees ALL rows
        // (packed_base advances to the height, `packed` emptied). into_image must
        // then take the re-decode path — never assemble over the empty retained
        // rows — and still return the one-shot image. This exercises the match
        // guard `st.done && st.packed_base == 0 && is_complete(&buf)` with
        // packed_base != 0: forcing the guard true, or flipping either `&&` to
        // `||`, or `== 0` to `!= 0`, wrongly assembles the empty rows.
        let (w, h) = (5u32, 8u32);
        let argb: Vec<u32> = (0..w * h)
            .map(|i| 0xff00_0000 | i.wrapping_mul(7))
            .collect();
        let payload = vp8l::encode::encode(w, h, &argb);
        let dims = Dimensions::new(w, h).unwrap();
        let metadata = Metadata {
            exif: Some(vec![1, 2, 3, 4, 5, 6]),
            ..Metadata::none()
        };
        let file = wrap(&payload, dims, &metadata, false);
        let one_shot = decode_image(&file, &DecodeOptions::default()).unwrap();

        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
            let _ = dec.drain_rows();
        }
        let debug = format!("{dec:?}");
        assert!(
            debug.contains(&format!("packed_base: {h}")),
            "all rows freed: {debug}"
        );
        assert!(
            debug.contains("packed_len: 0"),
            "retained rows emptied: {debug}"
        );
        assert_eq!(dec.into_image().unwrap(), one_shot);
    }

    #[test]
    fn locate_vp8l_reads_the_correct_payload_end() {
        // The VP8L chunk header sits at offset 12; its payload starts at 20 and its
        // declared size is the LE u32 at [16..20]. The still stream's payload_end
        // must be payload_start + that exact size — mutating a size-byte index
        // (`cursor + n -> cursor - n`) shifts it, visible in the Debug snapshot.
        let file = sample_webp(6, 4);
        let size = u32::from_le_bytes(file[16..20].try_into().unwrap()) as usize;
        let expected_end = 20 + size;
        let mut dec = IncrementalDecoder::new();
        for byte in &file[..file.len() - 1] {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        let debug = format!("{dec:?}");
        assert!(
            debug.contains("payload_start: 20"),
            "payload starts after the header: {debug}"
        );
        assert!(
            debug.contains(&format!("payload_end: {expected_end}")),
            "payload_end must be payload_start + declared size: {debug}"
        );
    }

    #[test]
    fn peek_info_validates_magic_at_exactly_twelve_bytes() {
        // Exactly 12 bytes with a huge declared size (never "complete") and a bad
        // magic: peek_info runs the magic check at len == 12 (the `< 12` guard must
        // not fire), so push surfaces NotWebp rather than asking for more input.
        // The `< -> <=` mutant returns Ok(None) at 12, skipping the magic check.
        let mut buf = vec![0u8; 12];
        buf[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&buf).unwrap_err(), Error::NotWebp);
    }

    #[test]
    fn peek_info_rejects_wrong_webp_fourcc() {
        // "RIFF" correct but the "WEBP" form wrong: either magic mismatch triggers
        // NotWebp (`riff_bad || webp_bad`). The `|| -> &&` mutant would accept when
        // only one side is wrong.
        let mut buf = b"RIFF".to_vec();
        buf.extend_from_slice(&u32::MAX.to_le_bytes()); // huge -> not complete
        buf.extend_from_slice(b"XXXX"); // wrong WEBP fourcc
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&buf).unwrap_err(), Error::NotWebp);
    }

    #[test]
    fn peek_info_skips_unknown_chunk_by_its_true_size() {
        // An unknown "TEST" chunk precedes the VP8L chunk; peek_info advances the
        // cursor by the chunk's declared size to reach VP8L. Mutating a size-byte
        // index (`cursor + n -> cursor - n`) reads the RIFF-size bytes (all 0x01
        // here) instead, mislocating the cursor so VP8L is never found and no
        // header is reported.
        let (w, h) = (6u32, 4u32);
        let argb: Vec<u32> = (0..w * h).map(|i| 0xff00_0000 | i).collect();
        let payload = vp8l::encode::encode(w, h, &argb);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0x0101_0101u32.to_le_bytes()); // huge; size bytes 5/6/7 all nonzero
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(b"TEST"); // unknown chunk, skipped via `_`
        file.extend_from_slice(&4u32.to_le_bytes());
        file.extend_from_slice(&[0u8, 0, 0, 0]);
        file.extend_from_slice(b"VP8L");
        file.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        file.extend_from_slice(&payload);

        let mut dec = IncrementalDecoder::new();
        let mut header_dims = None;
        for byte in &file {
            if let Progress::HeaderReady(info) = dec.push(core::slice::from_ref(byte)).unwrap() {
                header_dims = Some((info.dimensions.width(), info.dimensions.height()));
            }
        }
        assert_eq!(
            header_dims,
            Some((w, h)),
            "peek_info must skip the unknown chunk by its true size and reach VP8L"
        );
    }

    #[test]
    fn peek_info_reports_metadata_from_any_single_vp8x_flag() {
        // ICC only (no Exif, no XMP): `has_icc || has_exif || has_xmp` must still be
        // true. Either `|| -> &&` mutant collapses it to false.
        let (w, h) = (4u32, 4u32);
        let argb: Vec<u32> = (0..w * h).map(|i| 0xff00_0000 | i).collect();
        let payload = vp8l::encode::encode(w, h, &argb);
        let dims = Dimensions::new(w, h).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![1, 2, 3, 4]),
            ..Metadata::none()
        };
        let file = wrap(&payload, dims, &metadata, false);
        let mut dec = IncrementalDecoder::new();
        let mut meta_flag = None;
        for byte in &file[..file.len() - 1] {
            if let Progress::HeaderReady(info) = dec.push(core::slice::from_ref(byte)).unwrap() {
                meta_flag = Some(info.has_metadata);
            }
        }
        assert_eq!(
            meta_flag,
            Some(true),
            "an ICC-only VP8X still reports has_metadata"
        );
    }

    #[test]
    fn peek_info_reports_metadata_from_exif_only_vp8x_flag() {
        // Exif only (no ICC, no XMP): `has_icc || has_exif || has_xmp` is true. The
        // ICC-only case above does NOT pin the *second* `||`, because `&&` binds
        // tighter — mutating it gives `has_icc() || (has_exif() && has_xmp())`,
        // still true when only ICC is set. With Exif alone it becomes
        // `false || (true && false)` = false, so this fixture is what kills it.
        let (w, h) = (4u32, 4u32);
        let argb: Vec<u32> = (0..w * h).map(|i| 0xff00_0000 | i).collect();
        let payload = vp8l::encode::encode(w, h, &argb);
        let dims = Dimensions::new(w, h).unwrap();
        let metadata = Metadata {
            exif: Some(vec![9, 8, 7, 6]),
            ..Metadata::none()
        };
        let file = wrap(&payload, dims, &metadata, false);
        let mut dec = IncrementalDecoder::new();
        let mut meta_flag = None;
        for byte in &file[..file.len() - 1] {
            if let Progress::HeaderReady(info) = dec.push(core::slice::from_ref(byte)).unwrap() {
                meta_flag = Some(info.has_metadata);
            }
        }
        assert_eq!(
            meta_flag,
            Some(true),
            "an Exif-only VP8X still reports has_metadata"
        );
    }

    #[test]
    fn peek_info_rejects_lossy_vp8_chunk() {
        // A top-level lossy `VP8 ` chunk must be rejected by peek_info with
        // UnsupportedFeature (deleting the match arm would skip it silently). Huge
        // RIFF size keeps the file "incomplete" so peek_info is the sole arbiter.
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0x0101_0101u32.to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(b"VP8 ");
        file.extend_from_slice(&8u32.to_le_bytes());
        file.extend_from_slice(&[0u8; 8]);
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&file).unwrap_err(), Error::UnsupportedFeature);
    }

    #[test]
    fn peek_info_rejects_anim_chunk_without_vp8x() {
        // An `ANMF` chunk with no preceding animated `VP8X` is malformed;
        // peek_info must reject it with InvalidContainer (deleting the match arm
        // would skip it silently). Huge RIFF size keeps the file "incomplete".
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0x0101_0101u32.to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(b"ANMF");
        file.extend_from_slice(&8u32.to_le_bytes());
        file.extend_from_slice(&[0u8; 8]);
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&file).unwrap_err(), Error::InvalidContainer);
    }

    #[test]
    fn peek_vp8l_info_reports_header_at_exactly_five_payload_bytes() {
        // The VP8L header is complete at exactly 5 payload bytes; peek_vp8l_info
        // must report it (the `< 5` guard must not fire at len == 5). The `< -> <=`
        // mutant would return Ok(None) and withhold the header until 6 bytes.
        let file = sample_webp(6, 4); // bare VP8L: payload starts at offset 20
        assert!(
            file.len() >= 25,
            "sample must have at least 5 payload bytes"
        );
        let mut dec = IncrementalDecoder::new();
        let mut header = None;
        for byte in &file[..25] {
            if let Progress::HeaderReady(info) = dec.push(core::slice::from_ref(byte)).unwrap() {
                header = Some((info.dimensions.width(), info.dimensions.height()));
            }
        }
        assert_eq!(
            header,
            Some((6, 4)),
            "the VP8L header is complete at 5 payload bytes"
        );
    }

    #[test]
    fn peek_vp8l_info_reports_header_alpha_advisory() {
        // Non-opaque pixels set the VP8L header alpha-used advisory; there is no
        // VP8X, so vp8x_alpha is false. `header_alpha || vp8x_alpha` must be true;
        // the `|| -> &&` mutant yields true && false == false.
        let (w, h) = (4u32, 4u32);
        let argb: Vec<u32> = (0..w * h).map(|i| 0x8000_0000 | i).collect();
        let file = wrap_vp8l(&vp8l::encode::encode(w, h, &argb));
        let mut dec = IncrementalDecoder::new();
        let mut alpha = None;
        for byte in &file[..file.len() - 1] {
            if let Progress::HeaderReady(info) = dec.push(core::slice::from_ref(byte)).unwrap() {
                alpha = Some(info.has_alpha);
            }
        }
        assert_eq!(
            alpha,
            Some(true),
            "a bare VP8L header alpha advisory reports has_alpha"
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn decoder_fill_reads_the_source_for_read_info() {
        // `Decoder::read_info` calls `fill` to read the whole source into `buf`
        // before peeking. Replacing `fill` with `Ok(())` leaves `buf` empty, so
        // read_info peeks an empty slice and fails with Truncated.
        use super::Decoder;
        let file = sample_webp(4, 4);
        let mut dec = Decoder::new(&file[..]); // &[u8] implements std::io::Read
        let info = dec.read_info().unwrap();
        assert_eq!((info.dimensions.width(), info.dimensions.height()), (4, 4));
    }
}
