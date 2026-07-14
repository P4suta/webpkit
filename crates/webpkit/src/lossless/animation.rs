//! The lossless (`VP8L`) animation glue.
//!
//! The codec-agnostic animation walker (frame location + canvas compositing) lives
//! in [`crate::anim`]; this module supplies the lossless piece: the default
//! [`Vp8lFrameDecoder`] (decodes a `VP8L` frame, rejects a lossy one) and the
//! [`decode_frames`] entry points that wire it in. The umbrella `webpkit` crate
//! supplies its own both-codecs decoder instead.

use crate::anim::decode_frames_with_decoder;
use crate::error::{Error, Result};
use crate::lossless::vp8l;
use crate::stream::{DecodeOptions, DecodedFrame, FrameDecoder, FramePayload};

pub use crate::anim::{AnimInfo, BlendMode, CompositedFrame, DisposalMode, Frame, FrameMeta};

/// A lazy per-frame iterator over a lossless animation — each frame decoded as
/// `VP8L` by [`Vp8lFrameDecoder`].
pub type Frames<'a> = crate::anim::Frames<'a, Vp8lFrameDecoder>;
/// A compositing iterator that paints each lossless frame onto the persistent
/// canvas (see [`crate::anim::CompositedFrames`]).
pub type CompositedFrames<'a> = crate::anim::CompositedFrames<'a, Vp8lFrameDecoder>;

/// The default animation [`FrameDecoder`] for a bare `lossless` codec.
///
/// Decodes a `VP8L` (lossless) frame and rejects a lossy `VP8 ` one with
/// [`Error::UnsupportedFeature`]. The umbrella `webpkit` crate supplies a decoder
/// that also handles lossy frames.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vp8lFrameDecoder;

impl FrameDecoder for Vp8lFrameDecoder {
    fn decode_frame(
        &self,
        frame: FramePayload<'_>,
        options: &DecodeOptions,
    ) -> Result<DecodedFrame> {
        if let Some(payload) = frame.vp8l {
            // Guard allocation against a hostile frame size before decoding; also
            // capture the VP8L declared-alpha bit for key-frame detection (libwebp
            // keys on the declared flag, not a pixel scan).
            let (w, h, alpha_used) = vp8l::decode::peek_header(payload)?;
            let pixels = u64::from(w) * u64::from(h);
            if let Some(limit) = options.max_pixels.filter(|&l| pixels > l) {
                return Err(Error::LimitExceeded { pixels, limit });
            }
            let decoded = vp8l::decode::decode(payload)?;
            if decoded.width != frame.dims.width() || decoded.height != frame.dims.height() {
                return Err(Error::InvalidContainer);
            }
            Ok(DecodedFrame {
                argb: decoded.argb,
                alpha_used,
            })
        } else if frame.vp8.is_some() {
            Err(Error::UnsupportedFeature)
        } else {
            Err(Error::MissingImage)
        }
    }
}

/// Decode an animated WebP into a lazy [`Frames`] iterator (RGBA8 output),
/// decoding each frame as `VP8L` (a lossy frame is rejected — the umbrella
/// `webpkit` crate injects a decoder that handles both).
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] if the input is not an animation (or a frame is
/// lossy `VP8`), [`Error::InvalidContainer`] if the `ANIM` chunk is
/// missing/malformed, [`Error::MissingImage`] if there are no frames, or a
/// container error for a malformed file.
pub fn decode_frames(input: &[u8]) -> Result<Frames<'_>> {
    decode_frames_with_decoder(input, &DecodeOptions::default(), Vp8lFrameDecoder)
}

/// Decode an animated WebP into a lazy [`Frames`] iterator with explicit
/// [`DecodeOptions`] (output layout, per-frame pixel limit), decoding each frame
/// as `VP8L`.
///
/// # Errors
///
/// The same errors as [`decode_frames`], plus [`Error::LimitExceeded`] when the
/// canvas or a frame exceeds `options.max_pixels`.
pub fn decode_frames_with<'a>(input: &'a [u8], options: &DecodeOptions) -> Result<Frames<'a>> {
    decode_frames_with_decoder(input, options, Vp8lFrameDecoder)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{
        BlendMode, DecodedFrame, DisposalMode, FrameDecoder, FramePayload, Vp8lFrameDecoder,
        decode_frames, decode_frames_with,
    };
    use crate::anim::{Compositor, blend_over, decode_anmf, frame_meta};
    use crate::container::anim::{AnimChunk, AnmfFlags, AnmfHeader};
    use crate::container::fourcc::FourCc;
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::container::writer::{push_chunk, riff_envelope};
    use crate::error::{Error, Result};
    use crate::image::{self, Dimensions, Metadata, PixelLayout};
    use crate::lossless::decoder::DecodeOptions;
    use crate::lossless::prelude::*;

    /// A frame spec for the test assembler.
    struct FrameSpec {
        header: AnmfHeader,
        argb: Vec<u32>,
    }

    fn full_frame(dims: Dimensions, argb: Vec<u32>, duration: u32, flags: AnmfFlags) -> FrameSpec {
        FrameSpec {
            header: AnmfHeader {
                x: 0,
                y: 0,
                dims,
                duration_ms: duration,
                flags,
            },
            argb,
        }
    }

    /// Assemble a valid animated WebP from frame specs.
    fn assemble(
        canvas: Dimensions,
        background: u32,
        loop_count: u16,
        frames: &[FrameSpec],
    ) -> Vec<u8> {
        let has_alpha = frames.iter().any(|f| image::argb_has_alpha(&f.argb));
        let flags = Vp8xFlags::for_output(&Metadata::none(), has_alpha).with_animation();
        let mut body = Vec::new();
        push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, canvas));
        push_chunk(
            &mut body,
            FourCc::ANIM,
            &AnimChunk {
                background,
                loop_count,
            }
            .build(),
        );
        for frame in frames {
            let payload = crate::lossless::vp8l::encode::encode(
                frame.header.dims.width(),
                frame.header.dims.height(),
                &frame.argb,
            );
            let mut frame_body = frame.header.build().to_vec();
            push_chunk(&mut frame_body, FourCc::VP8L, &payload);
            push_chunk(&mut body, FourCc::ANMF, &frame_body);
        }
        riff_envelope(&body)
    }

    fn solid(dims: Dimensions, argb: u32) -> Vec<u32> {
        vec![argb; usize::try_from(dims.pixel_count()).unwrap()]
    }

    #[test]
    fn frames_iterator_decodes_each_frame() {
        let dims = Dimensions::new(4, 3).unwrap();
        let f0 = solid(dims, 0xFF00_00FF); // opaque blue
        let f1 = solid(dims, 0xFF00_FF00); // opaque green
        let file = assemble(
            dims,
            0,
            0,
            &[
                full_frame(dims, f0.clone(), 100, AnmfFlags(0)),
                full_frame(dims, f1.clone(), 40, AnmfFlags(0)),
            ],
        );
        let frames: Vec<_> = decode_frames(&file).unwrap().map(Result::unwrap).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].meta().duration_ms, 100);
        assert_eq!(frames[1].meta().duration_ms, 40);
        assert_eq!(
            frames[0].image().as_bytes(),
            image::pack_pixels(PixelLayout::Rgba8, &f0)
        );
        assert_eq!(
            frames[1].image().as_bytes(),
            image::pack_pixels(PixelLayout::Rgba8, &f1)
        );
    }

    #[test]
    fn frames_walk_respects_declared_riff_size() {
        // A chunk whose size crosses the declared RIFF size must be rejected, not
        // read into trailing bytes — matching the still path's clamping.
        let dims = Dimensions::new(4, 4).unwrap();
        let mut file = assemble(
            dims,
            0,
            0,
            &[full_frame(
                dims,
                solid(dims, 0xFF11_2233),
                100,
                AnmfFlags(0),
            )],
        );
        assert_eq!(decode_frames(&file).unwrap().count(), 1);
        // Shrink the declared RIFF size so the frame's chunk crosses body_end.
        let cut = u32::try_from(file.len() - 8 - 6).unwrap();
        file[4..8].copy_from_slice(&cut.to_le_bytes());
        // The walk must clamp to the declared size rather than read past it, so
        // it must not silently yield the (now out-of-bounds) frame.
        assert!(
            decode_frames(&file).map(Iterator::count) != Ok(1),
            "walk must not read a chunk past the declared RIFF size"
        );
    }

    #[test]
    fn anim_info_reports_canvas_background_and_loop() {
        let dims = Dimensions::new(8, 8).unwrap();
        // Background native ARGB 0x80123456 -> RGBA [0x12, 0x34, 0x56, 0x80].
        let file = assemble(
            dims,
            0x8012_3456,
            7,
            &[full_frame(
                dims,
                solid(dims, 0xFF00_0000),
                100,
                AnmfFlags(0),
            )],
        );
        let info = decode_frames(&file).unwrap().anim_info();
        assert_eq!(info.canvas, dims);
        assert_eq!(info.loop_count, 7);
        assert_eq!(info.background_rgba, [0x12, 0x34, 0x56, 0x80]);
    }

    #[test]
    fn composited_full_canvas_equals_each_frame() {
        let dims = Dimensions::new(4, 4).unwrap();
        let f0 = solid(dims, 0xFF11_2233);
        let f1 = solid(dims, 0xFF44_5566);
        let file = assemble(
            dims,
            0,
            0,
            &[
                full_frame(dims, f0.clone(), 100, AnmfFlags(0)),
                full_frame(dims, f1.clone(), 100, AnmfFlags(0)),
            ],
        );
        let composited: Vec<_> = decode_frames(&file)
            .unwrap()
            .composited()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            composited[0].image().as_bytes(),
            image::pack_pixels(PixelLayout::Rgba8, &f0)
        );
        assert_eq!(
            composited[1].image().as_bytes(),
            image::pack_pixels(PixelLayout::Rgba8, &f1)
        );
    }

    #[test]
    fn composited_dispose_background_clears_previous_rect() {
        // Canvas 4x4. Frame 0: full-canvas opaque red, dispose=background.
        // Frame 1: a 2x2 opaque green tile at (2,2), no dispose, blend.
        // After frame 0 displays, its rect (whole canvas) is cleared to
        // transparent; frame 1 then blends its tile onto a transparent canvas,
        // so only the tile is non-transparent.
        let canvas = Dimensions::new(4, 4).unwrap();
        let tile = Dimensions::new(2, 2).unwrap();
        let f0 = full_frame(
            canvas,
            solid(canvas, 0xFFFF_0000),
            100,
            AnmfFlags::from_parts(false, true), // blend, dispose=background
        );
        let f1 = FrameSpec {
            header: AnmfHeader {
                x: 2,
                y: 2,
                dims: tile,
                duration_ms: 100,
                flags: AnmfFlags(0), // blend, keep
            },
            argb: solid(tile, 0xFF00_FF00),
        };
        let file = assemble(canvas, 0, 0, &[f0, f1]);
        let frames: Vec<_> = decode_frames(&file)
            .unwrap()
            .composited()
            .map(Result::unwrap)
            .collect();
        // Frame 1 canvas: transparent everywhere except the bottom-right 2x2.
        let out = image::unpack_pixels(PixelLayout::Rgba8, frames[1].image().as_bytes());
        for y in 0..4u32 {
            for x in 0..4u32 {
                let px = out[(y * 4 + x) as usize];
                if x >= 2 && y >= 2 {
                    assert_eq!(px, 0xFF00_FF00, "tile pixel at ({x},{y})");
                } else {
                    assert_eq!(px, 0, "cleared pixel at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn blend_over_matches_libwebp_reference() {
        // Opaque source overwrites.
        assert_eq!(blend_over(0xFF12_3456, 0x0000_0000), 0xFF12_3456);
        // Fully transparent source keeps the destination.
        assert_eq!(blend_over(0x0012_3456, 0xFF65_4321), 0xFF65_4321);
        // Partial alpha over opaque dst: values hand-derived from libwebp's
        // non-premultiplied integer formula (BlendPixelNonPremult).
        // src=0x80FF0000 over dst=0xFF0000FF -> 0xFF7F007E.
        assert_eq!(blend_over(0x80FF_0000, 0xFF00_00FF), 0xFF7F_007E);
        // Partial alpha over partial dst: src_a=0x40, dst_a=0x80 ->
        // dst_factor_a=96, blend_a=0xA0; src=0x4000FF00 over dst=0x80FF0000.
        assert_eq!(blend_over(0x4000_FF00, 0x80FF_0000), 0xA098_6500);
    }

    proptest! {
        /// Over the whole input space, `blend_over` obeys libwebp's opaque and
        /// transparent shortcuts and produces the alpha channel `src_a +
        /// ((dst_a * (256 - src_a)) >> 8)` — recomputed here independently of
        /// `blend_over`'s internals.
        #[test]
        fn blend_over_alpha_and_shortcuts(src in any::<u32>(), dst in any::<u32>()) {
            let out = blend_over(src, dst);
            let src_a = (src >> 24) & 0xff;
            if src_a == 0xff {
                prop_assert_eq!(out, src);
            } else if src_a == 0 {
                prop_assert_eq!(out, dst);
            } else {
                let dst_a = (dst >> 24) & 0xff;
                let dst_factor_a = (dst_a * (256 - src_a)) >> 8;
                prop_assert_eq!((out >> 24) & 0xff, src_a + dst_factor_a);
            }
        }
    }

    #[test]
    fn frame_meta_maps_flags() {
        let header = AnmfHeader {
            x: 6,
            y: 4,
            dims: Dimensions::new(2, 2).unwrap(),
            duration_ms: 33,
            flags: AnmfFlags::from_parts(true, true),
        };
        let meta = frame_meta(header);
        assert_eq!((meta.x, meta.y, meta.duration_ms), (6, 4, 33));
        assert_eq!(meta.blend, BlendMode::Overwrite);
        assert_eq!(meta.dispose, DisposalMode::Background);
    }

    #[test]
    fn decode_frames_rejects_a_still_image() {
        let dims = Dimensions::new(2, 2).unwrap();
        let still = crate::container::writer::wrap_vp8l(&crate::lossless::vp8l::encode::encode(
            2,
            2,
            &solid(dims, 0xFF00_0000),
        ));
        assert_eq!(
            decode_frames(&still).unwrap_err(),
            Error::UnsupportedFeature
        );
    }

    #[test]
    fn composited_respects_output_layout() {
        let dims = Dimensions::new(2, 2).unwrap();
        let argb = solid(dims, 0xFF10_2030);
        let file = assemble(
            dims,
            0,
            0,
            &[full_frame(dims, argb.clone(), 100, AnmfFlags(0))],
        );
        let opts = DecodeOptions::default().layout(PixelLayout::Bgra8);
        let frame = decode_frames_with(&file, &opts)
            .unwrap()
            .composited()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            frame.image().as_bytes(),
            image::pack_pixels(PixelLayout::Bgra8, &argb)
        );
    }

    #[test]
    fn compositor_promotes_frame_after_full_canvas_background_dispose() {
        // libwebp `IsKeyFrame` promotes a frame to a key frame when its predecessor
        // disposed a full-canvas (or key) frame to background — even a partial-alpha
        // blend frame. A key frame *overwrites*, so its own pixels (including the
        // RGB under alpha 0) appear verbatim rather than being blended onto the
        // transparent canvas. This is the reference behavior our oracle validates;
        // failing to promote here would diverge from libwebp.
        let canvas = Dimensions::new(2, 2).unwrap();
        let full = |flags: AnmfFlags| AnmfHeader {
            x: 0,
            y: 0,
            dims: canvas,
            duration_ms: 0,
            flags,
        };
        let after_bg_dispose = |src: u32, alpha_used: bool| -> Vec<u32> {
            let mut compositor = Compositor::new(canvas, PixelLayout::Rgba8);
            // Frame 0: opaque full-canvas key frame, dispose=background.
            compositor
                .paint(
                    full(AnmfFlags::from_parts(false, true)),
                    false,
                    &solid(canvas, 0xFFFF_FFFF),
                )
                .unwrap();
            // Frame 1: full-canvas blend frame -> promoted to key (overwrite).
            let img = compositor
                .paint(full(AnmfFlags(0)), alpha_used, &solid(canvas, src))
                .unwrap();
            image::unpack_pixels(PixelLayout::Rgba8, img.as_bytes())
        };

        // Partial-alpha (a=0x7F) source is overwritten verbatim (R stays 0xFF), not
        // blended (which would give blend_over(src, 0) with R=0xFE).
        let src = 0x7FFF_0000;
        assert_ne!(
            blend_over(src, 0),
            src,
            "blend and overwrite must differ here"
        );
        for px in &after_bg_dispose(src, true) {
            assert_eq!(
                *px, src,
                "promoted key frame overwrites with its own pixels"
            );
        }

        // A fully transparent source keeps its RGB under alpha 0 (overwrite copies
        // it), matching libwebp's memcpy of the frame onto the zero-filled canvas.
        let transparent = 0x00FF_0000;
        for px in &after_bg_dispose(transparent, true) {
            assert_eq!(*px, transparent, "overwrite preserves RGB under alpha 0");
        }
    }

    // ---- Direct-path helpers for the `decode_anmf` and `Compositor::paint` tests. ----

    /// A test [`FrameDecoder`] for the lossy branch: the `VP8 ` payload's first four
    /// bytes are the little-endian `width`/`height`; byte 4 is a tag; the returned
    /// solid pixel encodes both the tag and (if present) the first `ALPH` byte, so a
    /// test can observe which `VP8 `/`ALPH` chunk was selected and whether alpha was
    /// seen. It mirrors the real umbrella decoder's guards (pixel limit on the
    /// frame dims, dimension-match) so the `decode_anmf` tests stay meaningful.
    #[derive(Debug, Clone, Copy)]
    struct TestFrameDecoder;

    impl FrameDecoder for TestFrameDecoder {
        fn decode_frame(
            &self,
            frame: FramePayload<'_>,
            options: &DecodeOptions,
        ) -> Result<DecodedFrame> {
            let vp8 = frame.vp8.ok_or(Error::MissingImage)?;
            let pixels = frame.dims.pixel_count();
            if let Some(limit) = options.max_pixels.filter(|&l| pixels > l) {
                return Err(Error::LimitExceeded { pixels, limit });
            }
            let w = u32::from(u16::from_le_bytes([vp8[0], vp8[1]]));
            let h = u32::from(u16::from_le_bytes([vp8[2], vp8[3]]));
            let dims = Dimensions::new(w, h).unwrap();
            if dims != frame.dims {
                return Err(Error::InvalidContainer);
            }
            let tag = u32::from(vp8[4]);
            let alph_byte = frame.alph.map_or(0u32, |a| u32::from(a[0]));
            let n = usize::try_from(dims.pixel_count()).unwrap();
            let pixel = 0xFF00_0000 | (tag << 8) | alph_byte;
            Ok(DecodedFrame {
                argb: vec![pixel; n],
                alpha_used: frame.alph.is_some(),
            })
        }
    }

    /// Assemble one `ANMF` chunk body (the 16-byte header followed by sub-chunks).
    fn anmf_bytes(header: AnmfHeader, chunks: &[(FourCc, &[u8])]) -> Vec<u8> {
        let mut data = header.build().to_vec();
        for &(id, payload) in chunks {
            push_chunk(&mut data, id, payload);
        }
        data
    }

    /// A frame header at `(x, y)` with a `w`×`h` rectangle.
    fn hdr(x: u32, y: u32, w: u32, h: u32, flags: AnmfFlags) -> AnmfHeader {
        AnmfHeader {
            x,
            y,
            dims: Dimensions::new(w, h).unwrap(),
            duration_ms: 0,
            flags,
        }
    }

    /// Paint a sequence of `(header, alpha_used, argb)` frames through one
    /// [`Compositor`] and return each snapshot as native-ARGB pixels.
    fn paint_seq(canvas: Dimensions, frames: &[(AnmfHeader, bool, Vec<u32>)]) -> Vec<Vec<u32>> {
        let mut compositor = Compositor::new(canvas, PixelLayout::Rgba8);
        frames
            .iter()
            .map(|(header, alpha_used, argb)| {
                let image = compositor.paint(*header, *alpha_used, argb).unwrap();
                image::unpack_pixels(PixelLayout::Rgba8, image.as_bytes())
            })
            .collect()
    }

    #[test]
    fn composited_frame_reports_duration() {
        let dims = Dimensions::new(2, 2).unwrap();
        let file = assemble(
            dims,
            0,
            0,
            &[
                full_frame(dims, solid(dims, 0xFF00_0000), 100, AnmfFlags(0)),
                full_frame(dims, solid(dims, 0xFF00_0001), 40, AnmfFlags(0)),
            ],
        );
        let frames: Vec<_> = decode_frames(&file)
            .unwrap()
            .composited()
            .map(Result::unwrap)
            .collect();
        assert_eq!(frames[0].duration_ms(), 100);
        assert_eq!(frames[1].duration_ms(), 40);
    }

    #[test]
    fn decode_anmf_keeps_first_vp8l_chunk() {
        // A duplicate `VP8L` chunk is skipped: the first one wins.
        let dims = Dimensions::new(2, 2).unwrap();
        let first = solid(dims, 0xFF11_2233);
        let second = solid(dims, 0xFF44_5566);
        let p1 = crate::lossless::vp8l::encode::encode(2, 2, &first);
        let p2 = crate::lossless::vp8l::encode::encode(2, 2, &second);
        let data = anmf_bytes(
            hdr(0, 0, 2, 2, AnmfFlags(0)),
            &[(FourCc::VP8L, &p1), (FourCc::VP8L, &p2)],
        );
        let (_h, _a, argb) =
            decode_anmf(&data, &Vp8lFrameDecoder, &DecodeOptions::default()).unwrap();
        assert_eq!(argb, first);
    }

    #[test]
    fn decode_anmf_vp8l_pixel_limit_boundary() {
        // width*height = 16. The guard uses `pixels > limit` on the *product*.
        let dims = Dimensions::new(4, 4).unwrap();
        let content = solid(dims, 0xFF33_4455);
        let payload = crate::lossless::vp8l::encode::encode(4, 4, &content);
        let data = anmf_bytes(hdr(0, 0, 4, 4, AnmfFlags(0)), &[(FourCc::VP8L, &payload)]);
        // Over the limit -> LimitExceeded reporting the product, not the sum/quotient.
        let over = DecodeOptions::default().max_pixels(10);
        assert_eq!(
            decode_anmf(&data, &Vp8lFrameDecoder, &over).unwrap_err(),
            Error::LimitExceeded {
                pixels: 16,
                limit: 10,
            },
        );
        // Exactly at the limit is allowed (`>`, not `>=`).
        let at = DecodeOptions::default().max_pixels(16);
        let (_h, _a, argb) = decode_anmf(&data, &Vp8lFrameDecoder, &at).unwrap();
        assert_eq!(argb, content);
    }

    #[test]
    fn decode_anmf_rejects_dimension_mismatch() {
        // The `VP8L` payload is 4x4 but the ANMF header claims 4x5: exactly one
        // axis differs, so the `||` dimension check must still reject it.
        let payload = crate::lossless::vp8l::encode::encode(
            4,
            4,
            &solid(Dimensions::new(4, 4).unwrap(), 0xFF00_0000),
        );
        let data = anmf_bytes(hdr(0, 0, 4, 5, AnmfFlags(0)), &[(FourCc::VP8L, &payload)]);
        assert_eq!(
            decode_anmf(&data, &Vp8lFrameDecoder, &DecodeOptions::default()).unwrap_err(),
            Error::InvalidContainer,
        );
    }

    #[test]
    fn decode_anmf_uses_first_vp8_and_needs_hook() {
        // Two `VP8 ` chunks: the first wins, and the injected hook is required to
        // decode at all (without a match on `VP8 `, the frame is MissingImage).
        let vp8_a: &[u8] = &[2, 0, 2, 0, 0x11];
        let vp8_b: &[u8] = &[2, 0, 2, 0, 0x22];
        let data = anmf_bytes(
            hdr(0, 0, 2, 2, AnmfFlags(0)),
            &[(FourCc::VP8, vp8_a), (FourCc::VP8, vp8_b)],
        );
        let (_h, alpha_used, argb) =
            decode_anmf(&data, &TestFrameDecoder, &DecodeOptions::default()).unwrap();
        assert!(!alpha_used);
        assert_eq!(argb, vec![0xFF00_1100u32; 4]);
    }

    #[test]
    fn decode_anmf_uses_first_alph_and_reports_presence() {
        // Two `ALPH` chunks: the first wins, its presence flips the reported
        // `alpha_used`, and the hook's returned dims equal the header (accepted).
        let vp8: &[u8] = &[2, 0, 2, 0, 0x11];
        let alph_a: &[u8] = &[0xAB];
        let alph_b: &[u8] = &[0xCD];
        let data = anmf_bytes(
            hdr(0, 0, 2, 2, AnmfFlags(0)),
            &[
                (FourCc::VP8, vp8),
                (FourCc::ALPH, alph_a),
                (FourCc::ALPH, alph_b),
            ],
        );
        let (_h, alpha_used, argb) =
            decode_anmf(&data, &TestFrameDecoder, &DecodeOptions::default()).unwrap();
        assert!(alpha_used);
        assert_eq!(argb, vec![0xFF00_11ABu32; 4]);
    }

    #[test]
    fn decode_anmf_vp8_pixel_limit_boundary() {
        // The lossy path guards on `header.dims.pixel_count()` (= 4) with `>`.
        let vp8: &[u8] = &[2, 0, 2, 0, 0x11];
        let data = anmf_bytes(hdr(0, 0, 2, 2, AnmfFlags(0)), &[(FourCc::VP8, vp8)]);
        let over = DecodeOptions::default().max_pixels(3);
        assert_eq!(
            decode_anmf(&data, &TestFrameDecoder, &over).unwrap_err(),
            Error::LimitExceeded {
                pixels: 4,
                limit: 3,
            },
        );
        // Exactly at the limit is allowed (`>`, not `>=`).
        let at = DecodeOptions::default().max_pixels(4);
        let (_h, _a, argb) = decode_anmf(&data, &TestFrameDecoder, &at).unwrap();
        assert_eq!(argb, vec![0xFF00_1100u32; 4]);
    }

    #[test]
    fn paint_rejects_frames_outside_the_canvas() {
        let canvas = Dimensions::new(5, 5).unwrap();
        let fill = 0xFF12_3456u32;
        // A valid offset frame paints fine: the guard sums `x+w`/`y+h`, it does not
        // multiply them (2*3 = 6 would spuriously exceed the 5-wide canvas).
        let mut ok = Compositor::new(canvas, PixelLayout::Rgba8);
        let img = ok
            .paint(
                hdr(2, 2, 3, 3, AnmfFlags(0)),
                false,
                &solid(Dimensions::new(3, 3).unwrap(), fill),
            )
            .unwrap();
        let pixels = image::unpack_pixels(PixelLayout::Rgba8, img.as_bytes());
        assert_eq!(pixels[2 * 5 + 2], fill); // painted at (2,2)
        assert_eq!(pixels[0], 0); // untouched corner
        // Out of bounds in x: x+w = 6 > 5.
        let mut bx = Compositor::new(canvas, PixelLayout::Rgba8);
        let argb_x = solid(Dimensions::new(4, 2).unwrap(), fill);
        assert_eq!(
            bx.paint(hdr(2, 0, 4, 2, AnmfFlags(0)), false, &argb_x)
                .unwrap_err(),
            Error::InvalidContainer,
        );
        // Out of bounds in y: y+h = 6 > 5.
        let mut by = Compositor::new(canvas, PixelLayout::Rgba8);
        let argb_y = solid(Dimensions::new(2, 4).unwrap(), fill);
        assert_eq!(
            by.paint(hdr(0, 2, 2, 4, AnmfFlags(0)), false, &argb_y)
                .unwrap_err(),
            Error::InvalidContainer,
        );
    }

    #[test]
    fn paint_full_canvas_frame_is_key_and_overwrites() {
        // A full-canvas no-alpha, no-blend frame is a key frame: it clears the
        // canvas and overwrites verbatim, so a semi-transparent source is copied
        // (alpha 0x80) rather than blended over the red frame 0 (alpha 0xFF).
        let canvas = Dimensions::new(2, 2).unwrap();
        let red = 0xFFFF_0000u32;
        let green = 0x8000_FF00u32;
        let out = paint_seq(
            canvas,
            &[
                (hdr(0, 0, 2, 2, AnmfFlags(0)), false, solid(canvas, red)),
                (hdr(0, 0, 2, 2, AnmfFlags(0)), false, solid(canvas, green)),
            ],
        );
        let blended = blend_over(green, red);
        assert_ne!(blended, green, "blend and overwrite must differ here");
        assert_eq!(out[1], vec![green; 4]);
    }

    #[test]
    fn paint_partial_frame_is_not_promoted_to_key() {
        // A frame that does not cover the whole canvas, following a plain "keep"
        // frame, is NOT a key frame: it blends over the retained red and leaves the
        // uncovered right column untouched (rather than clearing + overwriting).
        let canvas = Dimensions::new(2, 2).unwrap();
        let red = 0xFFFF_0000u32;
        let green = 0x8000_FF00u32;
        let out = paint_seq(
            canvas,
            &[
                (hdr(0, 0, 2, 2, AnmfFlags(0)), false, solid(canvas, red)),
                (
                    hdr(0, 0, 1, 2, AnmfFlags(0)),
                    false,
                    solid(Dimensions::new(1, 2).unwrap(), green),
                ),
            ],
        );
        let blended = blend_over(green, red);
        assert_ne!(blended, green, "blend and overwrite must differ here");
        // indices (2x2): (0,0)=0 (1,0)=1 (0,1)=2 (1,1)=3
        assert_eq!(out[1], vec![blended, red, blended, red]);
    }

    #[test]
    fn paint_promoted_key_after_background_dispose_predecessor() {
        // Frame 0: full-canvas key frame, dispose=background.
        // Frame 1: partial frame promoted to key by frame 0's bg dispose, itself
        //          dispose=background -> prev.was_key=true, prev.full_canvas=false.
        // Frame 2: promoted to key by `prev.dispose_background && (full_canvas ||
        //          was_key)` — the `was_key` disjunct is what carries it. So its
        //          transparent-RGB pixels are overwritten verbatim across the canvas.
        let canvas = Dimensions::new(4, 1).unwrap();
        let red = 0xFFFF_0000u32;
        let green = 0xFF00_FF00u32;
        let t = 0x00FF_0000u32; // transparent, RGB present
        let bg = AnmfFlags::from_parts(false, true);
        let out = paint_seq(
            canvas,
            &[
                (hdr(0, 0, 4, 1, bg), false, solid(canvas, red)),
                (
                    hdr(0, 0, 2, 1, bg),
                    false,
                    solid(Dimensions::new(2, 1).unwrap(), green),
                ),
                (hdr(0, 0, 4, 1, AnmfFlags(0)), true, solid(canvas, t)),
            ],
        );
        assert_eq!(out[2], vec![t; 4]);
    }

    #[test]
    fn paint_raw_overlap_keeps_frame_pixels_in_disposed_rect() {
        // Frame 0: full-canvas opaque red, keep.
        // Frame 1: a 2x2 tile at (1,1), dispose=background (not a key frame).
        // Frame 2: a full-canvas blend frame of transparent-RGB pixels. libwebp's
        // FindBlendRangeAtRow keeps frame 2's *raw* pixels wherever they overlap
        // frame 1's disposed rect; elsewhere it blends the transparent source over
        // the retained red (which yields red). So only the 2x2 overlap shows RGB.
        let canvas = Dimensions::new(4, 4).unwrap();
        let red = 0xFFFF_0000u32;
        let green = 0xFF00_FF00u32;
        let t = 0x00AA_BBCCu32; // transparent, RGB present
        let bg = AnmfFlags::from_parts(false, true);
        let out = paint_seq(
            canvas,
            &[
                (hdr(0, 0, 4, 4, AnmfFlags(0)), false, solid(canvas, red)),
                (
                    hdr(1, 1, 2, 2, bg),
                    false,
                    solid(Dimensions::new(2, 2).unwrap(), green),
                ),
                (hdr(0, 0, 4, 4, AnmfFlags(0)), true, solid(canvas, t)),
            ],
        );
        // 4x4 indices of the (1,1)-(2,2) overlap: 5, 6, 9, 10.
        let mut expected = vec![red; 16];
        for idx in [5usize, 6, 9, 10] {
            expected[idx] = t;
        }
        assert_eq!(out[2], expected);
    }

    #[test]
    fn paint_background_dispose_clears_exact_rect() {
        // Frame 1 paints a single pixel at (1,1) then disposes it to transparent.
        // Frame 2 overwrites only (0,0), so its snapshot reveals the retained
        // canvas: exactly (1,1) is transparent and every other pixel stays red.
        let canvas = Dimensions::new(3, 3).unwrap();
        let red = 0xFFFF_0000u32;
        let green = 0xFF00_FF00u32;
        let blue = 0xFF00_00FFu32;
        let bg = AnmfFlags::from_parts(false, true);
        let overwrite = AnmfFlags::from_parts(true, false);
        let out = paint_seq(
            canvas,
            &[
                (hdr(0, 0, 3, 3, AnmfFlags(0)), false, solid(canvas, red)),
                (hdr(1, 1, 1, 1, bg), false, vec![green]),
                (hdr(0, 0, 1, 1, overwrite), false, vec![blue]),
            ],
        );
        // 3x3 indices: (0,0)=0, (1,1)=4.
        let mut expected = vec![red; 9];
        expected[0] = blue;
        expected[4] = 0;
        assert_eq!(out[2], expected);
    }

    #[test]
    fn paint_reads_the_correct_source_row() {
        // A 2x2 frame with distinct rows: row 0 = [a,a], row 1 = [b,b]. If the
        // per-row source offset were computed wrong, row 1 would repeat row 0.
        let canvas = Dimensions::new(2, 2).unwrap();
        let a = 0xFF11_1111u32;
        let b = 0xFF22_2222u32;
        let out = paint_seq(
            canvas,
            &[(hdr(0, 0, 2, 2, AnmfFlags(0)), false, vec![a, a, b, b])],
        );
        assert_eq!(out[0], vec![a, a, b, b]);
    }
}
