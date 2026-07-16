//! The public push-based streaming decoder for still lossy (`VP8 `) images.
//!
//! [`IncrementalDecoder`] mirrors the `lossless` codec's still-image path: it buffers pushed
//! bytes, locates the top-level `VP8 ` chunk, and drives the suspend/resume
//! [`Vp8Stream`] row by row, reporting [`Progress`] and exposing finalized rows
//! through [`IncrementalDecoder::drain_rows`]. A bare `VP8 ` still is always
//! opaque (alpha lives only in the extended `VP8X` form), so the streamed rows
//! and [`IncrementalDecoder::into_image`] are byte-identical to a one-shot
//! [`crate::lossy::decode`] of the same chunk. `Read`-free, so it works on
//! `no_std + alloc`. Animation and alpha compositing belong to the umbrella
//! `webp` crate, which routes only bare-lossy stills here.

use crate::container::fourcc::FourCc;
use crate::container::reader::{ImageChunk, locate_image_with_alpha};
use crate::container::scan::{is_complete, scan_chunks};
use crate::container::vp8x::{VP8X_PAYLOAD_LEN, Vp8xInfo};
use crate::image;
use crate::stream::{DecodeOptions, ImageInfo, Progress, RowDrain};
use crate::{Codec, Dimensions, Error, Image, Metadata, PixelLayout, Result};

use crate::lossy::decode;
use crate::lossy::decode_incr::{Step, Vp8Stream};
use crate::lossy::frame_header::FrameHeader;
use crate::lossy::prelude::*;

/// The still-image streaming state: the suspend/resume [`Vp8Stream`], the `VP8 `
/// chunk's payload range within the buffer, and the accumulated packed rows.
struct StillState {
    /// The suspend/resume VP8 decoder, fed the chunk payload prefix each push.
    stream: Vp8Stream,
    /// The header peek that drove [`Progress::HeaderReady`] (dims/metadata).
    info: ImageInfo,
    /// Byte offset of the `VP8 ` chunk payload within the decoder's buffer.
    payload_start: usize,
    /// Declared end of the payload (`payload_start + chunk size`); may exceed the
    /// buffer until the chunk is fully received.
    payload_end: usize,
    /// The caller's requested output layout.
    layout: PixelLayout,
    /// Image (and output-row) width in pixels.
    width: u32,
    /// Image height in pixels.
    height: u32,
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
            .field("done", &self.done)
            .field("packed_len", &self.packed.len())
            .field("packed_base", &self.packed_base)
            .field("next_row", &self.next_row)
            .field("drained", &self.drained)
            .finish_non_exhaustive()
    }
}

impl StillState {
    /// A fresh stream for the `VP8 ` chunk at `[payload_start, payload_end)`.
    const fn new(
        info: ImageInfo,
        payload_start: usize,
        payload_end: usize,
        layout: PixelLayout,
    ) -> Self {
        Self {
            stream: Vp8Stream::new(),
            width: info.dimensions.width(),
            height: info.dimensions.height(),
            info,
            payload_start,
            payload_end,
            layout,
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

    /// Assemble the streamed image from the retained rows with no second decode.
    /// Only called on a fully-buffered RIFF (see [`IncrementalDecoder::into_image`]),
    /// so the container re-parse is exhaustive: a malformed-but-complete container
    /// (a lossless `VP8L`, an `ANMF`, or a `VP8X` canvas mismatch) is rejected here
    /// exactly as a one-shot [`decode_image`] would, rather than silently accepted.
    fn assemble(self, buf: &[u8]) -> Result<Image> {
        let dims =
            Dimensions::new(self.width, self.height).map_err(|_| Error::InvalidBitstream {
                codec: Codec::Lossy,
            })?;
        validate_lossy_container(buf, dims)?;
        // A bare VP8 still is opaque; the umbrella composites any ALPH itself.
        Ok(Image::from_parts(
            dims,
            self.layout,
            self.packed,
            false,
            Metadata::none(),
        ))
    }
}

/// A push-based decoder for a still lossy (`VP8 `) WebP image.
///
/// The image is streamed row-by-row through a `Vp8Stream`; [`Self::drain_rows`]
/// yields finalized rows early and [`Self::into_image`] returns the complete
/// picture. `Read`-free, so it works on `no_std + alloc`.
///
/// The pixel limit is **opt-in**: [`DecodeOptions::max_pixels`] defaults to `None`
/// (no cap), so a hostile header is not rejected until you set it (via
/// [`Self::with_options`]). Set it when streaming untrusted input — the limit is
/// then enforced against the peeked dimensions before the planes are allocated.
#[derive(Debug)]
pub struct IncrementalDecoder {
    buf: Vec<u8>,
    options: DecodeOptions,
    reported_header: bool,
    image: Option<Image>,
    still: Option<StillState>,
}

impl IncrementalDecoder {
    /// A new decoder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(DecodeOptions::default())
    }

    /// A new decoder with the given options.
    #[must_use]
    pub const fn with_options(options: DecodeOptions) -> Self {
        Self {
            buf: Vec::new(),
            options,
            reported_header: false,
            image: None,
            still: None,
        }
    }

    /// Feed the next slice of the file and report [`Progress`].
    ///
    /// # Errors
    ///
    /// The same errors as [`crate::lossy::decode`], surfaced as soon as the buffered
    /// bytes make them detectable.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Progress> {
        // Rows handed out by the previous `drain_rows` are freed now (the borrow
        // they returned has ended).
        if let Some(st) = self.still.as_mut() {
            st.free_drained();
        }
        if self.image.is_some() {
            return Ok(Progress::Finished);
        }
        self.buf.extend_from_slice(chunk);

        // Before a stream is set up, gate on completeness / determine the kind.
        if self.still.is_none()
            && let Some(progress) = self.begin_or_buffer()?
        {
            return Ok(progress);
        }

        // Report the header once (peek-driven, so its timing matches the one-shot
        // path) before streaming rows.
        if !self.reported_header {
            self.reported_header = true;
            if let Some(st) = self.still.as_ref() {
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
        // Enforce the pixel limit against the peeked dimensions *before* the stream
        // allocates its planes.
        let pixels = info.dimensions.pixel_count();
        if let Some(limit) = self.options.max_pixels.filter(|&limit| pixels > limit) {
            return Err(Error::LimitExceeded { pixels, limit });
        }
        let Some((start, end)) = locate_vp8(&self.buf) else {
            return Ok(Some(Progress::NeedMoreInput));
        };
        self.still = Some(StillState::new(info, start, end, self.options.layout));
        Ok(None)
    }

    /// Drive the still stream over the buffered `VP8 ` payload, packing every
    /// newly-finalized row and returning the resulting [`Progress`].
    fn drive_still(&mut self) -> Result<Progress> {
        let complete = is_complete(&self.buf);
        let Some(st) = self.still.as_mut() else {
            return Ok(Progress::NeedMoreInput);
        };
        // The chunk is `final` once fully buffered — or once the whole RIFF is, so
        // a chunk truncated within a complete file zero-pads exactly as one-shot.
        let final_input = self.buf.len() >= st.payload_end || complete;
        let end = self.buf.len().min(st.payload_end);
        let payload = &self.buf[st.payload_start..end];
        let row_bytes = st.width as usize * 4;

        let mut rows_first: Option<u32> = None;
        let mut rows_count = 0u32;
        loop {
            match st.stream.advance(payload, final_input)? {
                Step::Header { width, height } => {
                    st.width = width;
                    st.height = height;
                    // The peek already drove `HeaderReady`; this only transitions
                    // the stream into its streaming phase. Reporting here is a
                    // belt-and-suspenders fallback should the peek path be skipped.
                    if !self.reported_header {
                        self.reported_header = true;
                        let info = ImageInfo::new(
                            Dimensions::new(width, height).map_err(|_| {
                                Error::InvalidBitstream {
                                    codec: Codec::Lossy,
                                }
                            })?,
                            st.info.has_alpha,
                            st.info.has_metadata,
                            false,
                        )
                        .with_codec(Codec::Lossy);
                        st.info = info;
                        return Ok(Progress::HeaderReady(info));
                    }
                },
                Step::Rows { first_row, count } => {
                    let start = first_row as usize * row_bytes;
                    let stop = (first_row + count) as usize * row_bytes;
                    let rows_rgba = &st.stream.ready()[start..stop];
                    if st.layout == PixelLayout::Rgba8 {
                        st.packed.extend_from_slice(rows_rgba);
                    } else {
                        let argb = image::unpack_pixels(PixelLayout::Rgba8, rows_rgba);
                        st.packed
                            .extend_from_slice(&image::pack_pixels(st.layout, &argb));
                    }
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

    /// Always `None` for a still image (the animation-only frame view). Kept for
    /// API parity with the umbrella and lossless decoders.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "API parity: a still decoder has no per-frame canvas, but the \
                  umbrella and lossless decoders expose one through this method"
    )]
    pub const fn frame_image(&self) -> Option<&Image> {
        None
    }

    /// Borrow the finalized-but-not-yet-viewed rows in the requested
    /// [`PixelLayout`], as a non-consuming early view. `None` if none are pending.
    /// The borrowed bytes are freed on the next [`Self::push`], yet
    /// [`Self::into_image`] still returns the complete image (see [`RowDrain`]).
    pub fn drain_rows(&mut self) -> Option<RowDrain<'_>> {
        self.still.as_mut()?.take_drain()
    }

    /// Retrieve the **complete** decoded image once [`Progress::Finished`] has been
    /// reported.
    ///
    /// Draining rows via [`Self::drain_rows`] is a non-consuming early view. When
    /// the whole RIFF is buffered and no rows were drained-and-freed, the image is
    /// assembled from the retained rows with no second decode; otherwise it falls
    /// back to a one-shot decode of the buffered bytes — which reconstructs the
    /// full image, or errors exactly as [`crate::lossy::decode`] would.
    ///
    /// # Errors
    ///
    /// The same errors as [`crate::lossy::decode`] when the buffer is not a fully-decoded
    /// still image.
    pub fn into_image(self) -> Result<Image> {
        let Self {
            buf,
            options,
            image,
            still,
            ..
        } = self;
        if let Some(image) = image {
            return Ok(image);
        }
        match still {
            // Fast-path only when the whole RIFF is buffered AND no drained rows
            // were freed (so `packed` still holds every row). Otherwise defer to
            // the one-shot decode, which reconstructs any freed rows from `buf` or
            // surfaces the same error.
            Some(st) if st.done && st.packed_base == 0 && is_complete(&buf) => st.assemble(&buf),
            _ => decode_image(&buf, &options),
        }
    }
}

impl Default for IncrementalDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot decode of a complete (or erroring) buffered WebP into an [`Image`] in
/// the requested layout — the fallback [`IncrementalDecoder::into_image`] uses.
fn decode_image(buf: &[u8], options: &DecodeOptions) -> Result<Image> {
    let located = locate_image_with_alpha(buf)?;
    let payload = match located.image {
        ImageChunk::Lossy(payload) => payload,
        ImageChunk::Lossless(_) => return Err(Error::UnsupportedFeature),
    };
    // Enforce the pixel limit from the cheap header peek *before* `decode_frame`
    // sizes the reconstruction planes, so a whole-file push is guarded exactly as
    // the streaming path's pre-stream check is (and as the lossless one-shot is).
    let header = FrameHeader::parse_key_frame(payload)?;
    let pixels = u64::from(header.width) * u64::from(header.height);
    if let Some(limit) = options.max_pixels.filter(|&limit| pixels > limit) {
        return Err(Error::LimitExceeded { pixels, limit });
    }
    let image = decode::decode_frame(payload)?;
    if let Some(vp8x) = located.vp8x
        && vp8x.canvas != image.dimensions()
    {
        return Err(Error::InvalidContainer);
    }
    Ok(to_layout(image, options.layout))
}

/// Re-validate a fully-buffered container as a lossy still whose `VP8X` canvas (if
/// any) matches `dims`, for the no-re-decode assembly fast-path.
fn validate_lossy_container(buf: &[u8], dims: Dimensions) -> Result<()> {
    let located = locate_image_with_alpha(buf)?;
    match located.image {
        ImageChunk::Lossy(_) => {},
        ImageChunk::Lossless(_) => return Err(Error::UnsupportedFeature),
    }
    if located.vp8x.is_some_and(|vp8x| vp8x.canvas != dims) {
        return Err(Error::InvalidContainer);
    }
    Ok(())
}

/// Repack an RGBA8 [`Image`] into `layout` (identity for `Rgba8`); stays opaque.
fn to_layout(image: Image, layout: PixelLayout) -> Image {
    if layout == PixelLayout::Rgba8 {
        return image;
    }
    let dims = image.dimensions();
    let argb = image::unpack_pixels(PixelLayout::Rgba8, image.as_bytes());
    let bytes = image::pack_pixels(layout, &argb);
    Image::from_parts(dims, layout, bytes, false, Metadata::none())
}

/// Locate the top-level `VP8 ` chunk's payload range `[start, end)` once its
/// 8-byte chunk header is buffered. `end` is the declared payload end
/// (`start + size`), which may exceed the buffer until the chunk is fully received.
fn locate_vp8(bytes: &[u8]) -> Option<(usize, usize)> {
    scan_chunks(bytes)
        .find(|chunk| chunk.id == FourCc::VP8)
        .map(|chunk| (chunk.payload_start, chunk.payload_end))
}

/// Peek the image header from a (possibly partial) buffer, walking chunks until
/// the `VP8 ` header is reachable. `Ok(None)` means more bytes are needed.
/// A lossless (`VP8L`) or animated file is rejected — the umbrella routes those
/// elsewhere.
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
                // the lossy codec does not decode animations; the umbrella routes them.
                if info.flags.is_animated() {
                    return Err(Error::UnsupportedFeature);
                }
            },
            FourCc::VP8 => {
                return peek_vp8_info(bytes.get(chunk.payload_start..), has_metadata, vp8x_alpha);
            },
            FourCc::VP8L => return Err(Error::UnsupportedFeature),
            FourCc::ANIM | FourCc::ANMF => return Err(Error::InvalidContainer),
            _ => {},
        }
    }
    Ok(None)
}

/// Build [`ImageInfo`] from a `VP8 ` payload once at least its 10-byte header is
/// present. A bare VP8 still is opaque; `has_alpha` reflects only a `VP8X` flag.
fn peek_vp8_info(
    payload: Option<&[u8]>,
    has_metadata: bool,
    vp8x_alpha: bool,
) -> Result<Option<ImageInfo>> {
    let Some(payload) = payload else {
        return Ok(None);
    };
    if payload.len() < 10 {
        return Ok(None); // the uncompressed key-frame header is not fully buffered
    }
    let header = FrameHeader::parse_key_frame(payload)?;
    let dimensions =
        Dimensions::new(u32::from(header.width), u32::from(header.height)).map_err(|_| {
            Error::InvalidBitstream {
                codec: Codec::Lossy,
            }
        })?;
    Ok(Some(
        ImageInfo::new(dimensions, vp8x_alpha, has_metadata, false).with_codec(Codec::Lossy),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        reason = "test fixtures build container byte lengths with casts that fit their targets \
                  by construction"
    )]

    use super::{IncrementalDecoder, locate_vp8, peek_info, peek_vp8_info};
    use crate::Error;
    use crate::container::fourcc::FourCc;
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::container::writer::{push_chunk, riff_envelope};
    use crate::lossy::prelude::*;
    use crate::lossy::{
        Dimensions, Effort, ImageRef, LossyConfig, PixelLayout, decode, decode_with, encode,
        encode_vp8,
    };
    use crate::stream::{DecodeOptions, Progress};

    // A real 32x24 filtered VP8 stream (3 macroblock rows — several streamed row
    // bursts) used to drive the public decoder end to end.
    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/noise_32x24_q30.vp8");

    /// Wrap a raw `VP8 ` payload in a minimal RIFF/WEBP container.
    fn riff_vp8(payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, payload);
        riff_envelope(&body)
    }

    /// One-shot RGBA bytes for the fixture, the streaming target of truth.
    fn one_shot_rgba() -> Vec<u8> {
        decode::decode_frame(FIXTURE).unwrap().as_bytes().to_vec()
    }

    #[test]
    fn incremental_equals_one_shot_over_one_byte_pushes() {
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        assert_eq!(dec.into_image().unwrap().as_bytes(), one_shot_rgba());
    }

    #[test]
    fn drained_rows_reassemble_to_one_shot() {
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::new();
        let mut drained = Vec::new();
        let mut next_row = 0u32;
        for chunk in file.chunks(7) {
            dec.push(chunk).unwrap();
            if let Some(rows) = dec.drain_rows() {
                assert_eq!(rows.first_row, next_row, "drained rows not contiguous");
                next_row += rows.rows;
                drained.extend_from_slice(rows.as_bytes());
            }
        }
        let expected = one_shot_rgba();
        assert_eq!(drained, expected, "drained rows differ from one-shot");
        // into_image reconstructs the whole image even after rows were freed.
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected);
    }

    #[test]
    fn progress_sequence_is_monotonic() {
        let file = riff_vp8(FIXTURE);
        let height = decode::decode_frame(FIXTURE).unwrap().height();
        let mut dec = IncrementalDecoder::new();
        let mut header_seen = false;
        let mut rows_seen = 0u32;
        let mut finished = false;
        for byte in &file {
            assert!(!finished, "no push after Finished");
            match dec.push(core::slice::from_ref(byte)).unwrap() {
                Progress::HeaderReady(info) => {
                    assert!(!header_seen, "HeaderReady reported twice");
                    assert_eq!(rows_seen, 0, "rows before header");
                    assert_eq!(info.dimensions.height(), height);
                    header_seen = true;
                },
                Progress::RowsDecoded { first_row, count } => {
                    assert!(header_seen, "rows before HeaderReady");
                    assert_eq!(first_row, rows_seen, "row bursts not contiguous");
                    rows_seen += count;
                },
                Progress::Finished => finished = true,
                Progress::FrameComplete(_) => panic!("a still image emits no frames"),
                _ => {},
            }
        }
        assert!(finished, "stream never Finished");
        assert_eq!(rows_seen, height, "streamed rows did not sum to height");
    }

    #[test]
    fn whole_file_single_push_finishes_and_matches() {
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&file).unwrap(), Progress::Finished);
        assert_eq!(dec.into_image().unwrap().as_bytes(), one_shot_rgba());
    }

    #[test]
    fn max_pixels_rejects_before_plane_alloc() {
        // A complete RIFF/`VP8 ` file declaring a 16383x16383 frame: the whole-file
        // push routes to the one-shot decode, which must reject via the pixel limit
        // *before* reconstruction allocates ~1 GiB of planes (0x3fff = 16383).
        let header = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0xff, 0x3f, 0xff, 0x3f];
        let file = riff_vp8(&header);
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(16));
        assert_eq!(
            dec.push(&file).unwrap_err(),
            Error::LimitExceeded {
                pixels: 16383 * 16383,
                limit: 16,
            }
        );
    }

    /// A 32×64 checkerboard encoded with Balanced (loop filter on) plus its
    /// one-shot RGBA decode. The tall, blocky frame finalizes rows in several
    /// bursts across pushes, so the row-window bookkeeping (free / drain / offset)
    /// is exercised over multiple frees.
    fn tall_webp_and_one_shot() -> (Vec<u8>, Vec<u8>, u32) {
        let (w, h) = (32u32, 64u32);
        let dims = Dimensions::new(w, h).unwrap();
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 4 + y / 4) % 2 == 0 { 30u8 } else { 210 };
                rgba.extend_from_slice(&[v, v, v, 0xff]);
            }
        }
        let cfg = LossyConfig::new()
            .with_quality(50)
            .with_effort(Effort::Balanced);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let webp = encode(img, &cfg).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let payload = encode_vp8(img, &cfg).unwrap().1;
        let one_shot = decode(&payload).unwrap().as_bytes().to_vec();
        (webp, one_shot, w)
    }

    #[test]
    fn multi_push_drains_free_and_reassemble_exactly() {
        // Drain after every push so finalized rows are handed out and then freed on
        // the next push (`packed_base` advances past 0). Each batch must be the exact
        // one-shot rows at its output offset, and the reassembly must equal one-shot.
        // Because rows were drained-and-freed, `into_image` cannot take the assemble
        // fast-path and must re-decode the buffer — still yielding the whole image.
        // This pins the row-window arithmetic (`row_bytes`, `free_drained`,
        // `take_drain`) and the `into_image` match-guard to exact bytes.
        let (webp, one_shot, w) = tall_webp_and_one_shot();
        let rb = w as usize * 4;
        let mut dec = IncrementalDecoder::new();
        let mut collected = Vec::new();
        let mut next_row = 0u32;
        let mut drains = 0u32;
        for chunk in webp.chunks(16) {
            dec.push(chunk).unwrap();
            if let Some(rows) = dec.drain_rows() {
                assert_eq!(rows.first_row, next_row, "drained rows are not contiguous");
                let start = next_row as usize * rb;
                let end = start + rows.rows as usize * rb;
                assert_eq!(
                    rows.as_bytes(),
                    &one_shot[start..end],
                    "drained bytes differ from the one-shot rows at this offset"
                );
                next_row += rows.rows;
                collected.extend_from_slice(rows.as_bytes());
                drains += 1;
            }
        }
        assert!(
            drains >= 2,
            "expected multiple drains to exercise the free path, saw {drains}"
        );
        assert_eq!(collected, one_shot, "reassembled drained rows differ");
        // packed_base > 0 here (front rows were freed), so into_image re-decodes.
        assert_eq!(dec.into_image().unwrap().as_bytes(), one_shot);
    }

    #[test]
    fn second_drain_within_a_push_yields_nothing() {
        // A drain marks its rows viewed; a second drain in the SAME push (no
        // intervening push/free) has nothing newly finalized and must be `None`.
        let (webp, _one_shot, _w) = tall_webp_and_one_shot();
        let mut dec = IncrementalDecoder::new();
        let mut exercised = false;
        for chunk in webp.chunks(16) {
            dec.push(chunk).unwrap();
            if dec.drain_rows().is_some() {
                assert!(
                    dec.drain_rows().is_none(),
                    "a second drain in the same push must be None"
                );
                exercised = true;
            }
        }
        assert!(exercised, "never observed a drain to re-drain");
    }

    #[test]
    fn debug_of_active_still_names_the_state_and_fields() {
        // A partial push installs the still state; its Debug (a field of the derived
        // IncrementalDecoder Debug) must render the struct name and fields, not an
        // empty body.
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::new();
        dec.push(&file[..30]).unwrap();
        let rendered = format!("{dec:?}");
        assert!(
            rendered.contains("StillState"),
            "still Debug body missing: {rendered}"
        );
        assert!(
            rendered.contains("payload_start"),
            "still Debug fields missing: {rendered}"
        );
    }

    #[test]
    fn streaming_pixel_limit_is_exact_before_the_stream_starts() {
        // A partial push (30 bytes: RIFF + `VP8 ` header + the 10-byte frame header)
        // reaches the pre-stream pixel gate. The 32×24 frame is 768 pixels.
        let file = riff_vp8(FIXTURE);
        let prefix = &file[..30];
        // limit strictly below the pixel count -> rejected before the stream installs.
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(16));
        assert_eq!(
            dec.push(prefix).unwrap_err(),
            Error::LimitExceeded {
                pixels: 768,
                limit: 16,
            }
        );
        // limit exactly equal to the pixel count -> NOT rejected (the bound is `>`).
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(768));
        assert!(
            matches!(dec.push(prefix).unwrap(), Progress::HeaderReady(_)),
            "a frame at exactly the pixel limit must be accepted"
        );
    }

    #[test]
    fn one_shot_pixel_limit_accepts_exactly_the_limit() {
        // The whole-file one-shot path uses the same `>` bound: a 768-pixel frame at
        // a limit of 768 decodes; only strictly more would be rejected.
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::with_options(DecodeOptions::default().max_pixels(768));
        assert_eq!(dec.push(&file).unwrap(), Progress::Finished);
        assert_eq!(dec.into_image().unwrap().as_bytes(), one_shot_rgba());
    }

    #[test]
    fn final_input_flushes_the_last_row_before_the_riff_completes() {
        // A trailing EXIF chunk makes `payload_end` (end of the VP8 chunk) fall before
        // the RIFF's declared end. Pushing exactly up to `payload_end` gives the stream
        // its whole payload while the file is still incomplete: the decoder must treat
        // the input as final and flush EVERY row (the deferred last row included), so
        // the reported rows sum to the full height even before the EXIF arrives.
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8, FIXTURE);
        push_chunk(&mut body, FourCc::EXIF, b"metadata");
        let file = riff_envelope(&body);
        let payload_end = 20 + FIXTURE.len(); // 12 RIFF + 8 chunk header + payload
        let height = decode::decode_frame(FIXTURE).unwrap().height();

        let mut dec = IncrementalDecoder::new();
        let mut rows = 0u32;
        for byte in &file[..payload_end] {
            if let Progress::RowsDecoded { count, .. } =
                dec.push(core::slice::from_ref(byte)).unwrap()
            {
                rows += count;
            }
        }
        assert_eq!(
            rows, height,
            "the last row was not flushed when the payload (not the RIFF) completed"
        );
    }

    #[test]
    fn truncated_chunk_in_a_complete_riff_is_driven_as_final() {
        // `drive_still`'s `final_input = buf.len() >= payload_end || complete`. The
        // second operand (`complete`) is what makes a `VP8 ` token stream that is
        // TRUNCATED *within an otherwise complete RIFF* zero-pad exactly like the
        // one-shot decoder — committing the padded final rows and reporting
        // `Finished` — rather than suspending forever waiting for bytes the file will
        // never contain.
        //
        // We give the `VP8 ` chunk a truncated payload (the last 40 token bytes cut,
        // so the boolean decoder runs past the buffer on the final macroblock row)
        // and a chunk-size header that LIES about its length (claiming the untruncated
        // size), so `payload_end` runs past the buffer and `buf.len() >= payload_end`
        // is FALSE. The RIFF length, by contrast, is set to exactly the bytes present,
        // so the file is `complete`. Only the `complete` operand can make the input
        // final here.
        //
        // Correct (`||`): final_input = true -> exhausted token reads commit as
        // padding, the stream reaches every row and, with the whole RIFF buffered,
        // reports `Finished`, matching the one-shot decode of the truncated payload.
        // Mutant (`&&`): final_input = `false && true = false` -> the truncated final
        // row suspends on exhaustion, the stream never finishes, and `Finished` is
        // never reported.
        let cut = FIXTURE.len() - 40;
        let payload = &FIXTURE[..cut];
        let mut file = Vec::new();
        file.extend_from_slice(&FourCc::RIFF.0);
        // Declared RIFF length = actual buffered length (20-byte prefix + payload).
        let riff_size = (12 + payload.len()) as u32;
        file.extend_from_slice(&riff_size.to_le_bytes());
        file.extend_from_slice(&FourCc::WEBP.0);
        file.extend_from_slice(&FourCc::VP8.0);
        // Lie: claim the untruncated size, so payload_end runs past the buffered bytes.
        let vp8_size = FIXTURE.len() as u32;
        file.extend_from_slice(&vp8_size.to_le_bytes());
        file.extend_from_slice(payload);

        // The streamed rows must equal the one-shot decode of the truncated payload.
        let expected = decode::decode_frame(payload).unwrap().as_bytes().to_vec();

        let mut dec = IncrementalDecoder::new();
        // A partial push installs the still stream (peek + locate) and reports the
        // header; the RIFF is not yet complete here.
        assert!(matches!(
            dec.push(&file[..30]).unwrap(),
            Progress::HeaderReady(_)
        ));
        // The rest completes the RIFF. The VP8 chunk is still "short" of its lying
        // declared end, so ONLY the `complete` operand can make the input final.
        assert_eq!(
            dec.push(&file[30..]).unwrap(),
            Progress::Finished,
            "a truncated chunk inside a complete RIFF must be driven as final input"
        );
        // Drain the finalized rows (the lying chunk size makes the `assemble`/one-shot
        // `into_image` path reject the container, so read the committed pixels the
        // stream produced directly): they are the zero-padded one-shot decode of the
        // truncation, every row present.
        let rows = dec.drain_rows().expect("finalized rows to drain");
        assert_eq!(rows.first_row, 0, "rows drain contiguously from row 0");
        assert_eq!(
            rows.as_bytes(),
            expected.as_slice(),
            "streamed rows differ from the one-shot decode of the truncated payload"
        );
    }

    #[test]
    fn streamed_non_rgba_layout_repacks_like_one_shot() {
        // Streaming into a Bgra8 layout must repack each finalized row (R/B swapped),
        // matching a one-shot Bgra8 decode — pinning the layout branch in `drive_still`.
        let opts = DecodeOptions::default().layout(PixelLayout::Bgra8);
        let expected = decode_with(FIXTURE, &opts).unwrap().as_bytes().to_vec();
        assert_ne!(expected, one_shot_rgba(), "Bgra8 must differ from Rgba8");
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::with_options(opts);
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected);
    }

    #[test]
    fn one_shot_non_rgba_layout_repacks() {
        // The whole-file one-shot fallback must apply the requested layout too.
        let opts = DecodeOptions::default().layout(PixelLayout::Bgra8);
        let expected = decode_with(FIXTURE, &opts).unwrap().as_bytes().to_vec();
        let file = riff_vp8(FIXTURE);
        let mut dec = IncrementalDecoder::with_options(opts);
        assert_eq!(dec.push(&file).unwrap(), Progress::Finished);
        assert_eq!(dec.into_image().unwrap().as_bytes(), expected);
    }

    /// Wrap a `VP8X` (flags + canvas) followed by a `VP8 ` payload in a RIFF file.
    fn riff_vp8x(flags: u8, canvas: Dimensions, vp8: &[u8]) -> Vec<u8> {
        let vp8x = Vp8xInfo::build(Vp8xFlags(flags), canvas);
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8X, &vp8x);
        push_chunk(&mut body, FourCc::VP8, vp8);
        riff_envelope(&body)
    }

    #[test]
    fn one_shot_accepts_matching_vp8x_canvas() {
        // A VP8X whose canvas equals the coded 32×24 frame is valid: decode succeeds.
        // (A `!= -> ==` flip would reject this exact-match file.)
        let canvas = Dimensions::new(32, 24).unwrap();
        let file = riff_vp8x(0, canvas, FIXTURE);
        let mut dec = IncrementalDecoder::new();
        assert_eq!(dec.push(&file).unwrap(), Progress::Finished);
        assert_eq!(dec.into_image().unwrap().as_bytes(), one_shot_rgba());
    }

    #[test]
    fn streamed_assemble_rejects_vp8x_canvas_mismatch() {
        // Streaming a fully-buffered VP8X whose canvas (48×48) disagrees with the coded
        // frame (32×24) takes the no-re-decode assemble fast-path, whose container
        // re-validation must reject the mismatch exactly as a one-shot decode would.
        let canvas = Dimensions::new(48, 48).unwrap();
        let file = riff_vp8x(0, canvas, FIXTURE);
        let mut dec = IncrementalDecoder::new();
        for byte in &file {
            dec.push(core::slice::from_ref(byte)).unwrap();
        }
        assert_eq!(dec.into_image().unwrap_err(), Error::InvalidContainer);
    }

    /// A RIFF with a leading (odd-length) `ICCP` chunk before the `VP8 ` chunk, so the
    /// chunk walkers must skip a padded chunk to reach `VP8 ` at file offset 32.
    ///
    /// The 4-byte RIFF *size* field is set to deliberately-nonzero garbage. The chunk
    /// walkers start at offset 12 and never read that field, so it does not affect the
    /// real walk — but any cursor arithmetic that reads a byte at `cursor - k` (a
    /// `+ -> -` mutation of an id/size index) lands in that garbage and diverges,
    /// which a correct-size envelope (whose high bytes are zero) would hide.
    fn leading_chunk_file() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&FourCc::RIFF.0);
        f.extend_from_slice(&[0x11, 0xAA, 0xBB, 0xCC]); // size field: unused, all-nonzero
        f.extend_from_slice(&FourCc::WEBP.0);
        // Leading ICCP chunk, odd 3-byte payload -> one pad byte (exercises `size & 1`).
        f.extend_from_slice(&FourCc::ICCP.0);
        f.extend_from_slice(&3u32.to_le_bytes());
        f.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        f.push(0x00); // pad
        // VP8 chunk with the real fixture payload.
        f.extend_from_slice(&FourCc::VP8.0);
        f.extend_from_slice(&(FIXTURE.len() as u32).to_le_bytes());
        f.extend_from_slice(FIXTURE);
        f
    }

    #[test]
    fn locate_vp8_walks_past_a_padded_leading_chunk() {
        // ICCP occupies 12 bytes (8 header + 3 payload + 1 pad); the VP8 payload then
        // begins at file offset 12 + 12 + 8 = 32. An off-by-one in the cursor math, a
        // wrong id/size byte, or a broken pad step would miss it.
        let file = leading_chunk_file();
        assert_eq!(locate_vp8(&file), Some((32, 32 + FIXTURE.len())));
    }

    #[test]
    fn peek_info_walks_past_a_padded_leading_chunk() {
        // The same walk in `peek_info` must reach `VP8 ` and report the coded 32×24
        // dimensions rather than running off the end.
        let file = leading_chunk_file();
        let info = peek_info(&file)
            .unwrap()
            .expect("peek should reach the VP8 header past the leading chunk");
        assert_eq!(info.dimensions.width(), 32);
        assert_eq!(info.dimensions.height(), 24);
    }

    #[test]
    fn peek_info_reports_vp8x_metadata_and_alpha_flags() {
        // A VP8X carrying only the EXIF flag must set `has_metadata` (kills dropping the
        // VP8X arm, both `|| -> &&` in the metadata OR-chain, and the payload-slice
        // arithmetic that would otherwise yield None). The alpha flag must propagate too.
        let canvas = Dimensions::new(32, 24).unwrap();
        let exif = riff_vp8x(Vp8xFlags(0).0 | 0x08, canvas, FIXTURE);
        let info = peek_info(&exif)
            .unwrap()
            .expect("VP8X + VP8 peek yields info");
        assert!(info.has_metadata, "EXIF flag must set has_metadata");
        assert!(!info.has_alpha);
        let alpha = riff_vp8x(0x10, canvas, FIXTURE);
        let info = peek_info(&alpha)
            .unwrap()
            .expect("VP8X + VP8 peek yields info");
        assert!(info.has_alpha, "alpha flag must set has_alpha");
    }

    #[test]
    fn peek_info_rejects_lossless_and_animation() {
        // A bare VP8L is an unsupported still here; an ANIM chunk is a malformed still.
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8L, &[0x2f, 0, 0, 0, 0]);
        let vp8l = riff_envelope(&body);
        assert_eq!(peek_info(&vp8l).unwrap_err(), Error::UnsupportedFeature);

        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::ANIM, &[0u8; 6]);
        let anim = riff_envelope(&body);
        assert_eq!(peek_info(&anim).unwrap_err(), Error::InvalidContainer);
    }

    #[test]
    fn peek_info_requires_the_webp_magic() {
        // Valid RIFF tag but a wrong `WEBP` tag is not a WebP file (kills the
        // `|| -> &&` on the magic check and the redundant-length-guard flips).
        let mut bad = Vec::new();
        bad.extend_from_slice(b"RIFF");
        bad.extend_from_slice(&0u32.to_le_bytes());
        bad.extend_from_slice(b"XXXX"); // should be WEBP
        assert_eq!(peek_info(&bad).unwrap_err(), Error::NotWebp);
        // And an all-zero 12-byte buffer fails the magic rather than returning None.
        assert_eq!(peek_info(&[0u8; 12]).unwrap_err(), Error::NotWebp);
    }

    #[test]
    fn peek_vp8_info_parses_a_ten_byte_header() {
        // Exactly the 10-byte uncompressed key-frame header is enough to peek the
        // dimensions (kills the `< -> <=` off-by-one that would demand an 11th byte).
        let header = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0xff, 0x3f, 0xff, 0x3f];
        let info = peek_vp8_info(Some(&header), false, false)
            .unwrap()
            .expect("a full 10-byte header must peek");
        assert_eq!(info.dimensions.width(), 16383);
        assert_eq!(info.dimensions.height(), 16383);
    }
}
