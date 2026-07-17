//! Animation: lazy per-frame decoding and canvas compositing (`ANIM`/`ANMF`).
//!
//! The **codec-agnostic** animation layer: it locates each `ANMF` frame's
//! sub-chunks and composites the decoded pixels onto a persistent canvas, but
//! delegates the per-frame pixel decode to a [`FrameDecoder`] — so it lives in
//! the crate-root shell, above no particular codec. [`Frames`] is a lazy iterator that
//! decodes one frame per `next()`; [`CompositedFrames`] wraps it and paints each
//! frame onto the canvas honoring the blend and dispose methods.
//!
//! ## Canvas semantics (matching libwebp)
//!
//! The container spec says the canvas is cleared to the `ANIM` background color
//! and that "dispose to background" fills with it — but it also says viewers MAY
//! treat the background as a hint. libwebp's `WebPAnimDecoder` (which
//! `anim_dump` and browsers use) **ignores the background color**: it clears the
//! canvas and disposes rectangles to *transparent* (`0x0000_0000`). We match
//! libwebp so our composited output is byte-identical to the reference. The
//! background is still parsed and exposed on [`AnimInfo`] for completeness.

use alloc::borrow::Cow;

use crate::container::anim::{ANMF_HEADER_LEN, AnimChunk, AnmfHeader};
use crate::container::fourcc::FourCc;
use crate::container::reader::{Chunks, body_range, read_chunk_at};
use crate::container::vp8x::Vp8xInfo;
use crate::error::{Error, Result};
use crate::image::{self, Dimensions, Image, Metadata, PixelLayout};
use crate::prelude::*;
use crate::stream::{DecodeOptions, FrameDecoder, FramePayload};

pub use crate::container::anim::{BlendMode, DisposalMode, FrameMeta};

/// Canvas-wide animation parameters, from the `VP8X`, `ANIM`, and `ANMF` headers.
///
/// Every field is readable without decoding a frame — `frame_count` and
/// `total_duration_ms` included, since both live in the `ANMF` headers. See
/// [`crate::probe_animation`].
///
/// `#[non_exhaustive]`: further header facts can be added without a breaking
/// change (the fields stay `pub` to read).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub struct AnimInfo {
    /// The canvas size that every frame composites onto.
    pub canvas: Dimensions,
    /// The advisory background color, as RGBA bytes. Not used when compositing
    /// (see the module docs); exposed for completeness.
    pub background_rgba: [u8; 4],
    /// Loop count; `0` means loop forever.
    pub loop_count: u16,
    /// How many `ANMF` frames the file carries.
    pub frame_count: usize,
    /// The sum of every frame's display duration.
    pub total_duration_ms: u32,
}

/// A decoded animation frame: its metadata plus its own (frame-sized) pixels.
///
/// The image is the frame's raw rectangle, *not* the composited canvas — use
/// the frame iterator's `composited()` view for canvas-sized output.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    meta: FrameMeta,
    image: Image,
}

impl Frame {
    /// The frame's placement and timing.
    #[must_use]
    pub const fn meta(&self) -> FrameMeta {
        self.meta
    }
    /// The frame's own pixels (frame-sized).
    #[must_use]
    pub const fn image(&self) -> &Image {
        &self.image
    }
    /// Consume the frame, returning its image.
    #[must_use]
    pub fn into_image(self) -> Image {
        self.image
    }
}

/// A composited animation frame: the whole canvas at one point in time, plus how
/// long it is shown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompositedFrame {
    image: Image,
    duration_ms: u32,
}

impl CompositedFrame {
    /// How long this canvas is displayed, in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u32 {
        self.duration_ms
    }
    /// The composited canvas image (canvas-sized).
    #[must_use]
    pub const fn image(&self) -> &Image {
        &self.image
    }
    /// Consume the composited frame, returning its canvas image.
    #[must_use]
    pub fn into_image(self) -> Image {
        self.image
    }
}

/// A lazy iterator over an animation's frames.
///
/// Each [`Iterator::next`] locates the next `ANMF` chunk and decodes only that
/// frame's payload (`VP8L`, or lossy `VP8` + `ALPH`, via the injected
/// [`FrameDecoder`]), so frames are decoded on demand. The whole file is
/// held (borrowed for `decode_frames`, owned for `Decoder::into_frames`)
/// via [`Cow`], but no frame's pixels are decoded until requested.
#[derive(Clone, Debug)]
pub struct Frames<'a, D> {
    data: Cow<'a, [u8]>,
    /// Absolute byte offset of the next chunk to examine.
    cursor: usize,
    /// Absolute end of the RIFF body.
    body_end: usize,
    anim: AnimInfo,
    options: DecodeOptions,
    /// The per-frame decoder — `Vp8lFrameDecoder` for a bare lossless walk, or a
    /// both-codecs decoder injected by the umbrella crate. Chosen at compile time.
    decoder: D,
}

/// Count the `ANMF` frames from `start` and sum their durations, reading only
/// the frame headers.
///
/// Every fact here is in the headers, so counting must not cost a decode. The
/// walk stops at the first unreadable chunk rather than failing: a truncated
/// animation still has frames worth reporting, and the alternative is refusing
/// to describe exactly the file whose damage prompted the question.
fn count_frames(body: &[u8], start: usize) -> (usize, u32) {
    let (mut offset, mut count, mut duration) = (start, 0_usize, 0_u32);
    while offset < body.len() {
        let Ok(Some((chunk, next))) = read_chunk_at(body, offset) else {
            break;
        };
        if chunk.id == FourCc::ANMF {
            count += 1;
            if let Ok(header) = AnmfHeader::parse(chunk.data) {
                duration = duration.saturating_add(header.duration_ms);
            }
        }
        offset = next;
    }
    (count, duration)
}

impl<'a, D: FrameDecoder + Clone> Frames<'a, D> {
    /// Parse the animation header (`VP8X` canvas + `ANIM`) and position the
    /// cursor at the first `ANMF` frame.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedFeature`] if the file is not an animated `VP8X` (or a
    /// frame is lossy `VP8`), [`Error::InvalidContainer`] if the `ANIM` chunk is
    /// missing/malformed, [`Error::MissingImage`] if there are no frames, or
    /// [`Error::LimitExceeded`] if the canvas exceeds `options.max_pixels`.
    pub fn new(data: Cow<'a, [u8]>, options: DecodeOptions, decoder: D) -> Result<Self> {
        let (mut offset, body_end) = body_range(&data)?;
        // Walk within the declared RIFF body only, so a chunk whose size crosses
        // `body_end` into trailing bytes is rejected exactly like the still path
        // (`reader::chunks` slices to the same bound).
        let body = &data[..body_end];
        let mut canvas = None;
        let mut anim = None;
        let mut first_frame = None;
        while offset < body_end {
            let Some((chunk, next)) = read_chunk_at(body, offset)? else {
                break;
            };
            match chunk.id {
                FourCc::VP8X => {
                    let info = Vp8xInfo::parse(chunk.data)?;
                    if !info.flags.is_animated() {
                        return Err(Error::UnsupportedFeature);
                    }
                    canvas = Some(info.canvas);
                },
                FourCc::ANIM => anim = Some(AnimChunk::parse(chunk.data)?),
                FourCc::ANMF => {
                    first_frame = Some(offset);
                    break;
                },
                FourCc::VP8 => return Err(Error::UnsupportedFeature),
                _ => {},
            }
            offset = next;
        }
        // No animated VP8X means this is not an animation at all.
        let canvas = canvas.ok_or(Error::UnsupportedFeature)?;
        let anim = anim.ok_or(Error::InvalidContainer)?;
        let cursor = first_frame.ok_or(Error::MissingImage)?;
        let body = &data[..body_end];
        // Reject a canvas that exceeds the pixel limit *before* the compositor
        // allocates it (the same guard the still paths apply to their dimensions).
        let canvas_pixels = canvas.pixel_count();
        if let Some(limit) = options.max_pixels.filter(|&l| canvas_pixels > l) {
            return Err(Error::LimitExceeded {
                pixels: canvas_pixels,
                limit,
            });
        }
        let (frame_count, total_duration_ms) = count_frames(body, cursor);
        let anim = AnimInfo {
            canvas,
            background_rgba: PixelLayout::Rgba8.pack(anim.background),
            loop_count: anim.loop_count,
            frame_count,
            total_duration_ms,
        };
        Ok(Self {
            data,
            cursor,
            body_end,
            anim,
            options,
            decoder,
        })
    }

    /// The animation's canvas-wide parameters.
    #[must_use]
    pub const fn anim_info(&self) -> AnimInfo {
        self.anim
    }

    /// Turn this into a compositing iterator that paints each frame onto a
    /// persistent canvas (honoring blend and dispose), yielding full canvases.
    #[must_use]
    pub fn composited(self) -> CompositedFrames<'a, D> {
        let compositor = Compositor::new(self.anim.canvas, self.options.layout);
        CompositedFrames {
            frames: self,
            compositor,
        }
    }

    /// Decode the next `ANMF` frame into its header, the frame's declared-alpha bit,
    /// and native-ARGB pixels, advancing the cursor past it. Skips any interleaved
    /// metadata/unknown chunks.
    fn next_raw(&mut self) -> Option<Result<(AnmfHeader, bool, Vec<u32>)>> {
        while self.cursor < self.body_end {
            // Walk within the declared RIFF body only (see `new`), borrowing the
            // `data` field directly (not via a `&self` method) so the returned
            // slice and the `self.cursor` assignment are disjoint borrows.
            let body = &self.data[..self.body_end];
            let (chunk_id, chunk_data, next) = match read_chunk_at(body, self.cursor) {
                Ok(Some((chunk, next))) => (chunk.id, chunk.data, next),
                Ok(None) => return None,
                Err(err) => {
                    self.cursor = self.body_end;
                    return Some(Err(err));
                },
            };
            self.cursor = next;
            if chunk_id == FourCc::ANMF {
                return Some(decode_anmf(chunk_data, &self.decoder, &self.options));
            }
            // Trailing metadata (EXIF/XMP) or unknown chunks between/after
            // frames are skipped.
        }
        None
    }

    /// Pack a decoded frame into a [`Frame`] in the requested output layout.
    fn make_frame(&self, header: AnmfHeader, argb: &[u32]) -> Frame {
        let pixels = image::pack_pixels(self.options.layout, argb);
        let has_alpha = image::argb_has_alpha(argb);
        let image = Image::from_parts(
            header.dims,
            self.options.layout,
            pixels,
            has_alpha,
            Metadata::none(),
        );
        Frame {
            meta: frame_meta(header),
            image,
        }
    }
}

/// Parse one `ANMF` chunk into its header, then decode its frame through `decoder`
/// (the codec-agnostic seam), returning the header, the declared-alpha bit, and
/// native-ARGB pixels.
///
/// Shared by the lazy [`Frames`] walk and the byte-streaming animation path in
/// the byte-streaming decoder, so both decode a frame identically. Locating the frame's
/// `VP8L`/`VP8 `/`ALPH` sub-chunks is codec-agnostic and done here; the actual
/// pixel decode is delegated to the [`FrameDecoder`] (a bare `lossless` codec
/// passes a `Vp8lFrameDecoder`, which rejects a lossy frame; the umbrella crate
/// passes one that handles both).
///
/// # Errors
///
/// A container error for a malformed `ANMF` frame, or any error the `decoder`
/// reports for the located frame payload.
pub fn decode_anmf<D: FrameDecoder>(
    data: &[u8],
    decoder: &D,
    options: &DecodeOptions,
) -> Result<(AnmfHeader, bool, Vec<u32>)> {
    let header = AnmfHeader::parse(data)?;
    // The frame data (a bare chunk sequence) follows the 16-byte header.
    let frame_data = &data[ANMF_HEADER_LEN..];
    let mut vp8l = None;
    let mut vp8 = None;
    let mut alph = None;
    for chunk in Chunks::walk(frame_data) {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8L if vp8l.is_none() => vp8l = Some(chunk.data),
            FourCc::VP8 if vp8.is_none() => vp8 = Some(chunk.data),
            FourCc::ALPH if alph.is_none() => alph = Some(chunk.data),
            _ => {}, // skip a duplicate / unknown chunk
        }
    }
    let payload = FramePayload {
        vp8l,
        vp8,
        alph,
        dims: header.dims,
    };
    let decoded_frame = decoder.decode_frame(payload, options)?;
    Ok((header, decoded_frame.alpha_used, decoded_frame.argb))
}

impl<D: FrameDecoder + Clone> Iterator for Frames<'_, D> {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_raw()? {
            // The declared-alpha bit only matters for compositing, so the
            // per-frame view ignores it (the image's `has_alpha` is a pixel scan,
            // matching the still decoder).
            Ok((header, _alpha_used, argb)) => Some(Ok(self.make_frame(header, &argb))),
            Err(err) => Some(Err(err)),
        }
    }
}

/// Bookkeeping about the previously composited frame, needed for key-frame
/// detection and for reproducing libwebp's blend-range behavior: when the previous
/// frame disposed to background, the current blend frame keeps its *raw* pixels
/// wherever it overlaps that disposed rectangle (libwebp `FindBlendRangeAtRow`
/// skips the overlap), so the rectangle must be remembered.
#[derive(Clone, Copy, Debug)]
struct PrevFrame {
    /// The previous frame's rectangle `(x, y, w, h)` in canvas coordinates.
    rect: (u32, u32, u32, u32),
    dispose_background: bool,
    full_canvas: bool,
    was_key: bool,
}

/// A compositing iterator: paints each frame onto a persistent canvas and yields
/// the full canvas after every frame, replicating libwebp's `WebPAnimDecoder`.
#[derive(Clone, Debug)]
pub struct CompositedFrames<'a, D> {
    frames: Frames<'a, D>,
    compositor: Compositor,
}

impl<D: FrameDecoder + Clone> CompositedFrames<'_, D> {
    /// The animation's canvas-wide parameters.
    #[must_use]
    pub const fn anim_info(&self) -> AnimInfo {
        self.frames.anim
    }
}

impl<D: FrameDecoder + Clone> Iterator for CompositedFrames<'_, D> {
    type Item = Result<CompositedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        let (header, alpha_used, argb) = match self.frames.next_raw()? {
            Ok(frame) => frame,
            Err(err) => return Some(Err(err)),
        };
        let duration_ms = header.duration_ms;
        match self.compositor.paint(header, alpha_used, &argb) {
            Ok(image) => Some(Ok(CompositedFrame { image, duration_ms })),
            Err(err) => Some(Err(err)),
        }
    }
}

/// The persistent compositing state: canvas, previous-frame bookkeeping, layout.
///
/// Factored out so both the lazy [`CompositedFrames`] iterator and a codec's
/// byte-streaming animation walker paint frames through the *same*
/// [`Compositor::paint`] (identical blend / dispose / key-frame logic).
#[derive(Clone, Debug)]
pub struct Compositor {
    /// The canvas size every frame composites onto.
    canvas_dims: Dimensions,
    /// The requested output pixel layout for the snapshot images.
    layout: PixelLayout,
    /// Persistent native-ARGB canvas (row-major, `canvas.w * canvas.h`).
    canvas: Vec<u32>,
    prev: Option<PrevFrame>,
}

impl Compositor {
    /// A fresh compositor for a `canvas_dims`-sized canvas, cleared to transparent.
    #[must_use]
    pub fn new(canvas_dims: Dimensions, layout: PixelLayout) -> Self {
        let count = usize::try_from(canvas_dims.pixel_count()).unwrap_or(usize::MAX);
        Self {
            canvas_dims,
            layout,
            canvas: vec![0u32; count],
            prev: None,
        }
    }

    /// The persistent canvas as native ARGB (`0xAARRGGBB`), reflecting every frame
    /// painted so far *including* the last one's deferred disposal — i.e. the exact
    /// state the next frame composites onto. The animation optimizer reads this to
    /// diff each source frame against the true canvas the decoder would see.
    #[must_use]
    pub(crate) fn canvas_argb(&self) -> &[u32] {
        &self.canvas
    }

    /// Composite one frame onto the canvas and snapshot it, then defer this
    /// frame's disposal. Mirrors `WebPAnimDecoderGetNext`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidContainer`] if the frame rectangle does not fit the canvas.
    pub fn paint(&mut self, header: AnmfHeader, alpha_used: bool, argb: &[u32]) -> Result<Image> {
        let canvas_dims = self.canvas_dims;
        let (cw, ch) = (canvas_dims.width(), canvas_dims.height());
        let (x, y, w, h) = (
            header.x,
            header.y,
            header.dims.width(),
            header.dims.height(),
        );
        // The frame rectangle must fit inside the canvas.
        if x + w > cw || y + h > ch {
            return Err(Error::InvalidContainer);
        }
        // The decoded frame buffer must be exactly `w * h` long — every per-row
        // slice below indexes `argb` on that assumption. Enforce it here so a short
        // (or oversized) buffer from any of the crate's own `FrameDecoder`s
        // (`WebpFrameDecoder`/`Vp8lFrameDecoder`, plus the in-crate test decoders)
        // is a clean error rather than an out-of-bounds panic in the row loop. The
        // built-in decoders already reject a dimension mismatch upstream; this makes
        // the compositor safe on its own.
        if argb.len() as u64 != header.dims.pixel_count() {
            return Err(Error::InvalidContainer);
        }

        // libwebp keys on the VP8L declared-alpha bit, not on the actual pixels.
        let frame_has_alpha = alpha_used;
        let full_canvas = x == 0 && y == 0 && w == cw && h == ch;
        let overwrite = header.flags.do_not_blend();
        let dispose_bg = header.flags.dispose_background();

        // Key-frame detection, a byte-exact port of libwebp `IsKeyFrame`
        // (demux/anim_decode.c): the first frame is always a key frame; otherwise a
        // full-canvas no-alpha/no-blend frame is, and — critically — so is any
        // frame whose predecessor disposed a full-canvas (or key) frame to
        // background. That last clause is why a blend frame following a full-canvas
        // background-dispose is promoted to overwrite, matching the reference.
        let is_key = self.prev.is_none_or(|prev| {
            ((!frame_has_alpha || overwrite) && full_canvas)
                || (prev.dispose_background && (prev.full_canvas || prev.was_key))
        });
        if is_key {
            self.canvas.fill(0);
        }

        // When the previous frame disposed to background, a blend frame keeps its
        // *raw* pixels wherever it overlaps that disposed rectangle rather than
        // blending them against the now-transparent canvas — libwebp
        // `FindBlendRangeAtRow` excludes the overlap from the blend. Without this,
        // partial/transparent pixels in the overlap round differently (our canvas
        // already matches libwebp's `prev_frame_disposed` everywhere else).
        let raw_overlap = self
            .prev
            .filter(|_| !is_key && !overwrite)
            .and_then(|p| p.dispose_background.then_some(p.rect));

        // Paint the frame into its rectangle, row by row.
        let (cw_us, w_us) = (cw as usize, w as usize);
        for row in 0..h as usize {
            let cy = y as usize + row;
            let src = &argb[row * w_us..row * w_us + w_us];
            let start = cy * cw_us + x as usize;
            let dst = &mut self.canvas[start..start + w_us];
            for (col, (out, &pixel)) in dst.iter_mut().zip(src).enumerate() {
                let cx = x as usize + col;
                let in_prev_rect = raw_overlap.is_some_and(|(px, py, pw, ph)| {
                    (px as usize..(px + pw) as usize).contains(&cx)
                        && (py as usize..(py + ph) as usize).contains(&cy)
                });
                *out = if is_key || overwrite || in_prev_rect {
                    pixel
                } else {
                    blend_over(pixel, *out)
                };
            }
        }

        // Snapshot the canvas before this frame's disposal takes effect.
        let layout = self.layout;
        let pixels = image::pack_pixels(layout, &self.canvas);
        let has_alpha = image::argb_has_alpha(&self.canvas);
        let image = Image::from_parts(canvas_dims, layout, pixels, has_alpha, Metadata::none());

        // Deferred disposal: clear the rectangle to transparent for the next frame.
        if dispose_bg {
            for row in 0..h as usize {
                let start = (y as usize + row) * cw_us + x as usize;
                self.canvas[start..start + w_us].fill(0);
            }
        }
        self.prev = Some(PrevFrame {
            rect: (x, y, w, h),
            dispose_background: dispose_bg,
            full_canvas,
            was_key: is_key,
        });
        Ok(image)
    }
}

/// Alpha-blend a source pixel over a destination pixel, native ARGB in/out.
///
/// This mirrors libwebp `BlendPixelNonPremult` / `BlendPixelRowNonPremult`
/// exactly: an opaque source overwrites, a fully transparent source keeps the
/// destination, and otherwise the non-premultiplied integer blend is applied.
#[expect(
    clippy::cast_possible_truncation,
    reason = "blended channels are provably in 0..=255 (libwebp BlendPixelNonPremult); \
              the shift/mask arithmetic reproduces the reference bit for bit"
)]
#[must_use]
pub fn blend_over(src: u32, dst: u32) -> u32 {
    let src_a = (src >> 24) & 0xff;
    if src_a == 0xff {
        return src; // opaque source overwrites (BlendPixelRow skips it)
    }
    if src_a == 0 {
        return dst; // fully transparent source keeps the destination
    }
    let dst_a = (dst >> 24) & 0xff;
    let dst_factor_a = (dst_a * (256 - src_a)) >> 8;
    let blend_a = src_a + dst_factor_a; // provably in 1..=255
    let scale = (1u32 << 24) / blend_a;
    let channel = |shift: u32| -> u32 {
        let src_c = (src >> shift) & 0xff;
        let dst_c = (dst >> shift) & 0xff;
        let unscaled = src_c * src_a + dst_c * dst_factor_a;
        // Widen for the product (fits u32 by construction, per libwebp's assert).
        (((u64::from(unscaled) * u64::from(scale)) >> 24) as u32) & 0xff
    };
    (blend_a << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Map a container-layer [`AnmfHeader`] to the public [`FrameMeta`].
#[must_use]
pub const fn frame_meta(header: AnmfHeader) -> FrameMeta {
    FrameMeta {
        x: header.x,
        y: header.y,
        dimensions: header.dims,
        duration_ms: header.duration_ms,
        blend: if header.flags.do_not_blend() {
            BlendMode::Overwrite
        } else {
            BlendMode::Blend
        },
        dispose: if header.flags.dispose_background() {
            DisposalMode::Background
        } else {
            DisposalMode::Keep
        },
    }
}

/// Decode an animated WebP into a lazy frame iterator, decoding each frame
/// through the caller-supplied [`FrameDecoder`].
///
/// This is the codec-agnostic entry point: a codec crate wraps it with its own
/// default decoder (the `lossless` codec's `decode_frames` passes a `Vp8lFrameDecoder`;
/// the umbrella `webpkit` crate passes a decoder that handles both codecs).
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] if the input is not an animation,
/// [`Error::InvalidContainer`] if the `ANIM` chunk is missing/malformed,
/// [`Error::MissingImage`] if there are no frames, [`Error::LimitExceeded`] when
/// the canvas or a frame exceeds `options.max_pixels`, or a container error.
pub fn decode_frames_with_decoder<'a, D: FrameDecoder + Clone>(
    input: &'a [u8],
    options: &DecodeOptions,
    decoder: D,
) -> Result<Frames<'a, D>> {
    Frames::new(Cow::Borrowed(input), options.clone(), decoder)
}
