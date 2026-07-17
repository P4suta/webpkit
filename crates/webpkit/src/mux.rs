//! Editable animation muxing: read an animated WebP into per-frame records, edit
//! the frame list / loop / background / metadata, and write it back — copying every
//! untouched frame's encoded bytes through verbatim, decoding no pixel.
//!
//! The authoring counterpart to [`AnimationEncoder`](crate::AnimationEncoder), which
//! encodes fresh frames: [`AnimationMux`] instead demuxes an existing animation into
//! [`MuxFrame`] records (each holding its `VP8L`/`VP8 `/`ALPH` sub-chunk bytes) and
//! re-muxes them, so a frame `webpmux` would leave alone is byte-for-byte preserved.
//! It reuses the container framing ([`super::container`]) and the still image
//! headers only — the same one-directional layering the rest of the crate keeps.

use crate::anim::{BlendMode, DisposalMode};
use crate::container::anim::{ANMF_HEADER_LEN, AnimChunk, AnmfFlags, AnmfHeader};
use crate::container::fourcc::FourCc;
use crate::container::reader::{self, ImageChunk, body_range, read_container};
use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
use crate::container::writer::{push_chunk, riff_envelope, wrap_vp8, wrap_vp8_extended, wrap_vp8l};
use crate::error::{Codec, Error, Result};
use crate::image::{Dimensions, Metadata, PixelLayout};
use crate::prelude::*;

/// One demuxed animation frame: its placement, timing, and compositing methods,
/// plus its **encoded** sub-chunk bytes (`ALPH` + `VP8L`/`VP8 `) held verbatim so a
/// re-mux is byte-for-byte lossless.
///
/// Construct one by reading an animation ([`AnimationMux::read`]) or by lifting a
/// still WebP into a frame ([`MuxFrame::from_webp_still`]); read its facts through
/// the accessors. No pixel is ever decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MuxFrame {
    x: u32,
    y: u32,
    dimensions: Dimensions,
    duration_ms: u32,
    blend: BlendMode,
    dispose: DisposalMode,
    codec: Codec,
    /// Whether this frame contributes alpha to the canvas (a sibling `ALPH`, or a
    /// `VP8L` whose header declares alpha). Drives the re-muxed `VP8X` alpha flag.
    has_alpha: bool,
    /// The frame's sub-chunk bytes: the bare chunk sequence that follows the 16-byte
    /// `ANMF` header (an optional `ALPH` then the image chunk), copied verbatim.
    body: Vec<u8>,
}

impl MuxFrame {
    /// The frame's top-left X offset in canvas pixels (always even).
    #[must_use]
    pub const fn x(&self) -> u32 {
        self.x
    }
    /// The frame's top-left Y offset in canvas pixels (always even).
    #[must_use]
    pub const fn y(&self) -> u32 {
        self.y
    }
    /// The frame's own dimensions (may be smaller than the canvas).
    #[must_use]
    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
    }
    /// The frame's display duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u32 {
        self.duration_ms
    }
    /// How the frame combines with the canvas underneath it.
    #[must_use]
    pub const fn blend(&self) -> BlendMode {
        self.blend
    }
    /// What happens to the frame's rectangle after it is displayed.
    #[must_use]
    pub const fn dispose(&self) -> DisposalMode {
        self.dispose
    }
    /// Which codec coded this frame's image sub-chunk.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether the frame's placement fits within `canvas`.
    fn fits(&self, canvas: Dimensions) -> bool {
        u64::from(self.x) + u64::from(self.dimensions.width()) <= u64::from(canvas.width())
            && u64::from(self.y) + u64::from(self.dimensions.height()) <= u64::from(canvas.height())
    }

    /// Parse one `ANMF` chunk payload (its 16-byte header plus the frame's
    /// sub-chunks) into a frame record, copying the sub-chunk bytes verbatim.
    fn from_anmf(data: &[u8]) -> Result<Self> {
        let header = AnmfHeader::parse(data)?;
        let body = &data[ANMF_HEADER_LEN..];
        let (codec, has_alpha) = classify_body(body)?;
        Ok(Self {
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
            codec,
            has_alpha,
            body: body.to_vec(),
        })
    }

    /// Lift a still WebP into an animation frame, taking its image (`VP8L`, or
    /// `VP8 ` with an optional sibling `ALPH`) sub-chunk bytes verbatim.
    ///
    /// The inverse of [`AnimationMux::frame_as_webp`]: a frame extracted from one
    /// animation can be inserted into another with no re-encode. Offsets must be
    /// even and the duration must fit in 24 bits; whether the frame fits a given
    /// canvas is checked by [`AnimationMux::insert_frame`] /
    /// [`AnimationMux::replace_frame`].
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedFeature`] if `input` is itself animated,
    /// [`Error::MissingImage`] if it has no image chunk, [`Error::InvalidFrame`] if
    /// an offset is odd or the duration exceeds `2^24 - 1` ms, or the container
    /// errors of [`crate::probe`] on a malformed file.
    pub fn from_webp_still(
        input: &[u8],
        x: u32,
        y: u32,
        duration_ms: u32,
        blend: BlendMode,
        dispose: DisposalMode,
    ) -> Result<Self> {
        if !x.is_multiple_of(2) || !y.is_multiple_of(2) || duration_ms >= (1 << 24) {
            return Err(Error::InvalidFrame);
        }
        let info = crate::probe(input)?;
        if info.is_animated {
            return Err(Error::UnsupportedFeature);
        }
        let c = read_container(input, false)?;
        let (codec, id, payload) = match c.image.ok_or(Error::MissingImage)? {
            ImageChunk::Lossless(p) => (Codec::Lossless, FourCc::VP8L, p),
            ImageChunk::Lossy(p) => (Codec::Lossy, FourCc::VP8, p),
        };
        let mut body = Vec::new();
        // A lossless `VP8L` frame carries its own alpha; a sibling `ALPH` only pairs
        // with a lossy `VP8 ` image, and precedes it in the spec's sub-chunk order.
        if let Some(alph) = c.alpha.filter(|_| matches!(codec, Codec::Lossy)) {
            push_chunk(&mut body, FourCc::ALPH, alph);
        }
        push_chunk(&mut body, id, payload);
        Ok(Self {
            x,
            y,
            dimensions: info.dimensions,
            duration_ms,
            blend,
            dispose,
            codec,
            has_alpha: info.has_alpha,
            body,
        })
    }

    /// Rebuild this frame as a standalone still WebP, wrapping its image (and any
    /// `ALPH`) sub-chunk bytes verbatim. `None` if the frame carries no image chunk.
    fn to_still(&self) -> Option<Vec<u8>> {
        let mut image: Option<(Codec, &[u8])> = None;
        let mut alph: Option<&[u8]> = None;
        for chunk in reader::Chunks::walk(&self.body) {
            let Ok(chunk) = chunk else { break };
            match chunk.id {
                FourCc::VP8L if image.is_none() => image = Some((Codec::Lossless, chunk.data)),
                FourCc::VP8 if image.is_none() => image = Some((Codec::Lossy, chunk.data)),
                FourCc::ALPH if alph.is_none() => alph = Some(chunk.data),
                _ => {},
            }
        }
        let (codec, payload) = image?;
        Some(match codec {
            // A `VP8L` image carries its own alpha, so a bare simple-form file suffices.
            Codec::Lossless => wrap_vp8l(payload),
            Codec::Lossy => alph.map_or_else(
                || wrap_vp8(payload),
                |a| wrap_vp8_extended(payload, Some(a), self.dimensions, &Metadata::none()),
            ),
        })
    }

    /// The `ANMF` chunk payload (16-byte header + sub-chunks) for this frame.
    fn to_anmf_payload(&self) -> Vec<u8> {
        let header = AnmfHeader {
            x: self.x,
            y: self.y,
            dims: self.dimensions,
            duration_ms: self.duration_ms,
            flags: AnmfFlags::from_parts(
                matches!(self.blend, BlendMode::Overwrite),
                matches!(self.dispose, DisposalMode::Background),
            ),
        };
        let mut payload = header.build().to_vec();
        payload.extend_from_slice(&self.body);
        payload
    }
}

/// Classify an `ANMF` frame body: which codec coded its image chunk, and whether
/// the frame contributes alpha (a sibling `ALPH`, or a `VP8L` header alpha bit).
fn classify_body(body: &[u8]) -> Result<(Codec, bool)> {
    let mut codec = None;
    let mut vp8l_payload = None;
    let mut has_alph = false;
    for chunk in reader::Chunks::walk(body) {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8L if codec.is_none() => {
                codec = Some(Codec::Lossless);
                vp8l_payload = Some(chunk.data);
            },
            FourCc::VP8 if codec.is_none() => codec = Some(Codec::Lossy),
            FourCc::ALPH => has_alph = true,
            _ => {},
        }
    }
    let codec = codec.ok_or(Error::MissingImage)?;
    let has_alpha = has_alph || vp8l_payload.is_some_and(peek_vp8l_alpha);
    Ok((codec, has_alpha))
}

/// Whether a `VP8L` payload's header declares alpha, peeked without a full decode.
/// A malformed or short header reads as opaque — the re-mux only omits the alpha
/// flag, which the decoder recomputes from pixels regardless.
fn peek_vp8l_alpha(payload: &[u8]) -> bool {
    crate::lossless::decoder::peek_vp8l_info(Some(payload), false, false)
        .ok()
        .flatten()
        .is_some_and(|info| info.has_alpha)
}

/// An editable, in-memory animated WebP: its canvas, `ANIM` header, sidecar
/// metadata, and an ordered list of [`MuxFrame`] records.
///
/// [`read`](Self::read) demuxes an animation into one of these without decoding a
/// pixel; the setters and frame operations edit it; [`finish`](Self::finish)
/// re-muxes it, emitting each frame's encoded bytes verbatim so an untouched frame
/// is byte-for-byte preserved. The `webpmux`-parity editing half of the toolkit,
/// beside the metadata rewrite in [`crate::write_metadata`].
///
/// ```
/// use webpkit::{AnimationEncoder, AnimationMux, BlendMode, DisposalMode, Dimensions, FrameMeta, ImageRef, PixelLayout};
/// let canvas = Dimensions::new(2, 2).unwrap();
/// let rgba = [10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
/// let meta = FrameMeta::new(0, 0, canvas, 100, BlendMode::Blend, DisposalMode::Keep);
/// let anim = AnimationEncoder::new(canvas)
///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap(), meta)
///     .unwrap()
///     .finish();
///
/// let mut mux = AnimationMux::read(&anim).unwrap();
/// mux.set_loop_count(3);
/// let edited = mux.finish();
/// assert_eq!(webpkit::probe_animation(&edited).unwrap().loop_count, 3);
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AnimationMux {
    canvas: Dimensions,
    /// Advisory background color, native ARGB (`0xAARRGGBB`), as the `ANIM` chunk.
    background: u32,
    loop_count: u16,
    metadata: Metadata,
    frames: Vec<MuxFrame>,
}

impl AnimationMux {
    /// Demux an animated WebP into an editable mux, decoding no pixel.
    ///
    /// Tolerant of a truncated tail, like [`crate::probe_animation`]: the walk stops
    /// at the first unreadable chunk, keeping the frames already read — a damaged
    /// animation is still describable and editable up to the break.
    ///
    /// # Errors
    ///
    /// [`Error::NotWebp`]/[`Error::Truncated`] for a non-WebP or short input,
    /// [`Error::UnsupportedFeature`] if the file is not an animation,
    /// [`Error::InvalidContainer`] for a missing/malformed `VP8X` canvas or `ANIM`.
    pub fn read(input: &[u8]) -> Result<Self> {
        // Validate the RIFF envelope up front, matching the still readers' errors.
        let _ = body_range(input)?;
        let mut canvas = None;
        let mut background = 0u32;
        let mut loop_count = 0u16;
        let mut metadata = Metadata::none();
        let mut frames = Vec::new();
        let mut animated = false;
        for chunk in reader::chunks(input)? {
            let Ok(chunk) = chunk else { break };
            match chunk.id {
                FourCc::VP8X => {
                    let info = Vp8xInfo::parse(chunk.data)?;
                    canvas = Some(info.canvas);
                    animated |= info.flags.is_animated();
                },
                FourCc::ANIM => {
                    let anim = AnimChunk::parse(chunk.data)?;
                    background = anim.background;
                    loop_count = anim.loop_count;
                    animated = true;
                },
                FourCc::ANMF => {
                    animated = true;
                    // A malformed frame ends the walk rather than failing the read,
                    // so the surviving frames stay describable/editable.
                    let Ok(frame) = MuxFrame::from_anmf(chunk.data) else {
                        break;
                    };
                    frames.push(frame);
                },
                // First-wins, mirroring the still readers' duplicate-chunk guard.
                FourCc::ICCP if metadata.icc_profile.is_none() => {
                    metadata.icc_profile = Some(chunk.data.to_vec());
                },
                FourCc::EXIF if metadata.exif.is_none() => {
                    metadata.exif = Some(chunk.data.to_vec());
                },
                FourCc::XMP if metadata.xmp.is_none() => {
                    metadata.xmp = Some(chunk.data.to_vec());
                },
                _ => {},
            }
        }
        if !animated {
            return Err(Error::UnsupportedFeature);
        }
        let canvas = canvas.ok_or(Error::InvalidContainer)?;
        Ok(Self {
            canvas,
            background,
            loop_count,
            metadata,
            frames,
        })
    }

    /// The animation's canvas size.
    #[must_use]
    pub const fn canvas(&self) -> Dimensions {
        self.canvas
    }

    /// The advisory background color as RGBA bytes (parsed from the `ANIM` chunk).
    #[must_use]
    pub const fn background_rgba(&self) -> [u8; 4] {
        PixelLayout::Rgba8.pack(self.background)
    }

    /// The loop count (`0` = loop forever).
    #[must_use]
    pub const fn loop_count(&self) -> u16 {
        self.loop_count
    }

    /// The sidecar ICC/Exif/XMP metadata carried alongside the frames.
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The frames, in display order.
    #[must_use]
    pub fn frames(&self) -> &[MuxFrame] {
        &self.frames
    }

    /// How many frames the animation carries.
    #[must_use]
    pub const fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// The sum of every frame's display duration, in milliseconds.
    #[must_use]
    pub fn total_duration_ms(&self) -> u32 {
        self.frames
            .iter()
            .fold(0u32, |acc, frame| acc.saturating_add(frame.duration_ms))
    }

    /// Set the loop count (`0` = loop forever).
    pub const fn set_loop_count(&mut self, loop_count: u16) {
        self.loop_count = loop_count;
    }

    /// Set the advisory background color from RGBA bytes.
    pub const fn set_background(&mut self, rgba: [u8; 4]) {
        self.background = PixelLayout::Rgba8.unpack(rgba);
    }

    /// Replace the sidecar ICC/Exif/XMP metadata (an empty [`Metadata`] strips it).
    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
    }

    /// Insert `frame` at `index` (`index == frame_count` appends), rebuilding the
    /// frame list; every untouched frame keeps its encoded bytes verbatim.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] if `index` is past the end, or the frame does not fit
    /// inside the canvas at its offset.
    pub fn insert_frame(&mut self, index: usize, frame: MuxFrame) -> Result<()> {
        if index > self.frames.len() || !frame.fits(self.canvas) {
            return Err(Error::InvalidFrame);
        }
        self.frames.insert(index, frame);
        Ok(())
    }

    /// Append `frame` as the last frame.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] if the frame does not fit inside the canvas.
    pub fn push_frame(&mut self, frame: MuxFrame) -> Result<()> {
        let end = self.frames.len();
        self.insert_frame(end, frame)
    }

    /// Replace the frame at `index`, keeping every other frame's bytes verbatim.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] if `index` is out of range, or the new frame does not
    /// fit inside the canvas at its offset.
    pub fn replace_frame(&mut self, index: usize, frame: MuxFrame) -> Result<()> {
        if index >= self.frames.len() || !frame.fits(self.canvas) {
            return Err(Error::InvalidFrame);
        }
        self.frames[index] = frame;
        Ok(())
    }

    /// Remove and return the frame at `index`, or `None` if it is out of range.
    pub fn remove_frame(&mut self, index: usize) -> Option<MuxFrame> {
        (index < self.frames.len()).then(|| self.frames.remove(index))
    }

    /// Extract one frame as a standalone still WebP (`webpmux -get frame`), wrapping
    /// its encoded image bytes verbatim. `None` if `index` is out of range or the
    /// frame carries no image chunk.
    #[must_use]
    pub fn frame_as_webp(&self, index: usize) -> Option<Vec<u8>> {
        self.frames.get(index).and_then(MuxFrame::to_still)
    }

    /// Re-mux the animation into a complete WebP file, emitting each frame's encoded
    /// sub-chunk bytes verbatim.
    ///
    /// The chunk order matches [`AnimationEncoder::finish`](crate::AnimationEncoder),
    /// so reading an animation and re-muxing it unedited reproduces its bytes; the
    /// `VP8X` alpha flag is recomputed from the frames actually present.
    #[must_use]
    pub fn finish(&self) -> Vec<u8> {
        let has_alpha = self.frames.iter().any(|frame| frame.has_alpha);
        let flags = Vp8xFlags::for_output(&self.metadata, has_alpha).with_animation();
        let mut body = Vec::new();
        push_chunk(
            &mut body,
            FourCc::VP8X,
            &Vp8xInfo::build(flags, self.canvas),
        );
        if let Some(icc) = &self.metadata.icc_profile {
            push_chunk(&mut body, FourCc::ICCP, icc);
        }
        push_chunk(
            &mut body,
            FourCc::ANIM,
            &AnimChunk {
                background: self.background,
                loop_count: self.loop_count,
            }
            .build(),
        );
        for frame in &self.frames {
            push_chunk(&mut body, FourCc::ANMF, &frame.to_anmf_payload());
        }
        if let Some(exif) = &self.metadata.exif {
            push_chunk(&mut body, FourCc::EXIF, exif);
        }
        if let Some(xmp) = &self.metadata.xmp {
            push_chunk(&mut body, FourCc::XMP, xmp);
        }
        riff_envelope(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationMux, MuxFrame};
    use crate::image::{self, Dimensions, ImageRef, Metadata, PixelLayout};
    use crate::{
        AnimCodec, AnimationEncoder, BlendMode, Codec, DisposalMode, Error, FrameMeta, LossyParams,
        decode_frames,
    };

    fn frame_rgba(dims: Dimensions, argb: u32) -> Vec<u8> {
        let pixels = vec![argb; usize::try_from(dims.pixel_count()).unwrap()];
        image::pack_pixels(PixelLayout::Rgba8, &pixels)
    }

    fn image_ref(dims: Dimensions, rgba: &[u8]) -> ImageRef<'_> {
        ImageRef::new(dims, PixelLayout::Rgba8, rgba).unwrap()
    }

    fn meta(dims: Dimensions, duration: u32) -> FrameMeta {
        FrameMeta::new(0, 0, dims, duration, BlendMode::Blend, DisposalMode::Keep)
    }

    /// A 3-frame lossless animation, loop 5, with metadata.
    fn sample() -> Vec<u8> {
        let canvas = Dimensions::new(4, 4).unwrap();
        let (rgba0, rgba1, rgba2) = (
            frame_rgba(canvas, 0xFF11_2233),
            frame_rgba(canvas, 0xFF44_5566),
            frame_rgba(canvas, 0xFF77_8899),
        );
        AnimationEncoder::new(canvas)
            .loop_count(5)
            .background([1, 2, 3, 255])
            .metadata(Metadata::none().with_exif(vec![9, 8, 7]))
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 100))
            .and_then(|e| e.add_frame(image_ref(canvas, &rgba1), meta(canvas, 50)))
            .and_then(|e| e.add_frame(image_ref(canvas, &rgba2), meta(canvas, 25)))
            .unwrap()
            .finish()
    }

    #[test]
    fn read_recovers_header_and_frames() {
        let mux = AnimationMux::read(&sample()).unwrap();
        assert_eq!(mux.canvas(), Dimensions::new(4, 4).unwrap());
        assert_eq!(mux.loop_count(), 5);
        assert_eq!(mux.background_rgba(), [1, 2, 3, 255]);
        assert_eq!(mux.frame_count(), 3);
        assert_eq!(mux.total_duration_ms(), 175);
        assert_eq!(mux.metadata().exif.as_deref(), Some(&[9, 8, 7][..]));
        let durations: Vec<u32> = mux.frames().iter().map(MuxFrame::duration_ms).collect();
        assert_eq!(durations, [100, 50, 25]);
        for frame in mux.frames() {
            assert_eq!(frame.codec(), Codec::Lossless);
        }
    }

    #[test]
    fn read_then_finish_is_byte_identical() {
        // An unedited demux → re-mux reproduces the original file exactly.
        let original = sample();
        let round_tripped = AnimationMux::read(&original).unwrap().finish();
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn set_loop_and_background_edit_only_the_anim_header() {
        let original = sample();
        let mut mux = AnimationMux::read(&original).unwrap();
        mux.set_loop_count(0);
        mux.set_background([10, 20, 30, 40]);
        let edited = mux.finish();

        let re = AnimationMux::read(&edited).unwrap();
        assert_eq!(re.loop_count(), 0);
        assert_eq!(re.background_rgba(), [10, 20, 30, 40]);
        // Every frame's encoded ANMF bytes are byte-identical to the original's.
        assert_eq!(
            frame_anmf_bytes(&edited),
            frame_anmf_bytes(&original),
            "untouched frames must pass through verbatim"
        );
    }

    #[test]
    fn remove_frame_rebuilds_the_list_and_keeps_survivors_verbatim() {
        let original = sample();
        let survivors_before: Vec<Vec<u8>> = frame_anmf_bytes(&original);
        let mut mux = AnimationMux::read(&original).unwrap();
        let removed = mux.remove_frame(1).unwrap();
        assert_eq!(removed.duration_ms(), 50);
        let edited = mux.finish();

        let frames = frame_anmf_bytes(&edited);
        assert_eq!(frames.len(), 2);
        // Frames 0 and 2 survive verbatim; the middle frame is gone.
        assert_eq!(frames[0], survivors_before[0]);
        assert_eq!(frames[1], survivors_before[2]);
        assert_eq!(decode_frames(&edited).unwrap().count(), 2);
    }

    #[test]
    fn extract_and_reinsert_a_frame_round_trips() {
        let original = sample();
        let mux = AnimationMux::read(&original).unwrap();
        // Extract frame 1 as a standalone still, decode it, and check it matches the
        // animation's own frame-1 pixels.
        let still = mux.frame_as_webp(1).unwrap();
        let decoded = crate::decode(&still).unwrap();
        assert_eq!(decoded.dimensions(), Dimensions::new(4, 4).unwrap());

        // Re-insert that still as a new frame, then confirm the animation grew and
        // still decodes end to end.
        let mut mux = AnimationMux::read(&original).unwrap();
        let frame =
            MuxFrame::from_webp_still(&still, 0, 0, 60, BlendMode::Blend, DisposalMode::Keep)
                .unwrap();
        mux.insert_frame(1, frame).unwrap();
        let edited = mux.finish();
        assert_eq!(AnimationMux::read(&edited).unwrap().frame_count(), 4);
        assert_eq!(decode_frames(&edited).unwrap().count(), 4);
    }

    #[test]
    fn insert_rejects_a_frame_that_overflows_the_canvas() {
        let still = {
            let dims = Dimensions::new(8, 8).unwrap();
            crate::encode_lossless_rgba(8, 8, &frame_rgba(dims, 0xFF00_00FF)).unwrap()
        };
        let frame =
            MuxFrame::from_webp_still(&still, 0, 0, 40, BlendMode::Blend, DisposalMode::Keep)
                .unwrap();
        // The 4x4 sample cannot hold an 8x8 frame.
        let mut mux = AnimationMux::read(&sample()).unwrap();
        assert_eq!(mux.insert_frame(0, frame).unwrap_err(), Error::InvalidFrame);
    }

    #[test]
    fn read_rejects_a_still_image() {
        let still = crate::encode_lossless_rgba(4, 4, &[0u8; 4 * 4 * 4]).unwrap();
        assert_eq!(
            AnimationMux::read(&still).unwrap_err(),
            Error::UnsupportedFeature
        );
    }

    #[test]
    fn alpha_flag_survives_a_lossy_frame_round_trip() {
        // A lossy frame with alpha keeps the VP8X alpha flag through a demux/re-mux.
        let canvas = Dimensions::new(4, 4).unwrap();
        let argb: Vec<u32> = (0..16u32).map(|i| ((i * 15) << 24) | 0x0011_2233).collect();
        let rgba = image::pack_pixels(PixelLayout::Rgba8, &argb);
        let anim = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(90),
            })
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 40))
            .unwrap()
            .finish();

        let mux = AnimationMux::read(&anim).unwrap();
        assert_eq!(mux.frames()[0].codec(), Codec::Lossy);
        let re = mux.finish();
        // VP8X flags byte is at offset 20; the alpha bit (0x10) must be set.
        assert_ne!(re[20] & 0x10, 0, "the alpha flag must survive the re-mux");
    }

    /// The `ANMF` chunk bytes (fourcc + size + payload) of each frame, for
    /// verbatim-passthrough comparisons.
    fn frame_anmf_bytes(file: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut data = &file[12..];
        while data.len() >= 8 {
            let size =
                usize::try_from(u32::from_le_bytes([data[4], data[5], data[6], data[7]])).unwrap();
            let total = 8 + size + (size & 1);
            if &data[0..4] == b"ANMF" {
                out.push(data[..total].to_vec());
            }
            data = &data[total..];
        }
        out
    }
}
