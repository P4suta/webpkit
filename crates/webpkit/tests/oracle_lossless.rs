//! L5 differential oracle: cross-check the `lossless` codec against libwebp in-process.
//!
//! Enabled only with `--features oracle` (which links `libwebp-sys` and the C
//! reference library); never part of a normal build. Over proptest-generated
//! images it asserts three byte-exact identities against libwebp:
//!
//! 1. our decode == libwebp decode (of a libwebp-encoded stream),
//! 2. libwebp decode of *our* encode == the source,
//! 3. our decode of *libwebp's* encode == the source.
//!
//! It also covers **animation compositing** in-process: libwebp-sys 0.14.4 binds
//! the demux `WebPAnimDecoder` API (and its `build.rs` compiles the demux/mux C
//! by default), so [`libwebp_anim_composite`] composites an animation with
//! libwebp's `WebPAnimDecoder` while the `lossless` codec composites the same file with
//! [`webpkit::lossless::decode_frames`] + `Frames::composited`, and the two are asserted
//! equal (canvas dimensions and per-frame RGBA). No `img2webp`/`webpmux`
//! subprocess is involved — the earlier claim that the demux API was unbound is
//! stale as of libwebp-sys 0.14.4.
//!
//! Any divergence from the reference implementation — on any input the fuzzer or
//! proptest reaches — fails here mechanically, without a human comparing output.
#![cfg(feature = "oracle")]
#![expect(
    clippy::unwrap_used,
    reason = "this is a test-only integration crate where unwrap is the accepted \
              style (see clippy.toml `allow-unwrap-in-tests`); the config exempts \
              `#[test]` bodies but not the module-level FFI/strategy helpers here, \
              which unwrap only provably-infallible conversions and reference-lib \
              successes"
)]

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

/// Encode `rgba` (width*height*4 bytes) losslessly with the libwebp reference.
fn libwebp_encode_lossless(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    );
    let stride = i32::try_from(width * 4).unwrap();
    let mut out: *mut u8 = core::ptr::null_mut();
    // SAFETY: `rgba` holds `width * height * 4` bytes with the given stride;
    // libwebp writes the newly allocated stream pointer into `out`.
    let size =
        unsafe { libwebp_sys::WebPEncodeLosslessRGBA(rgba.as_ptr(), w, h, stride, &raw mut out) };
    assert!(!out.is_null() && size > 0, "libwebp lossless encode failed");
    // SAFETY: on success libwebp guarantees `out` points at `size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out, size) }.to_vec();
    // SAFETY: `out` was allocated by libwebp and is freed exactly once.
    unsafe { libwebp_sys::WebPFree(out.cast()) };
    bytes
}

/// Decode `webp` with the libwebp reference into `(width, height, rgba)`.
fn libwebp_decode(webp: &[u8]) -> (u32, u32, Vec<u8>) {
    let (mut w, mut h) = (0i32, 0i32);
    // SAFETY: `webp`/`len` describe a valid buffer; libwebp allocates the output
    // and writes the dimensions through the out-pointers.
    let ptr =
        unsafe { libwebp_sys::WebPDecodeRGBA(webp.as_ptr(), webp.len(), &raw mut w, &raw mut h) };
    assert!(!ptr.is_null(), "libwebp decode failed");
    let len = usize::try_from(w).unwrap() * usize::try_from(h).unwrap() * 4;
    // SAFETY: libwebp returned `w * h * 4` valid RGBA bytes at `ptr`.
    let rgba = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    // SAFETY: `ptr` was allocated by libwebp and is freed exactly once.
    unsafe { libwebp_sys::WebPFree(ptr.cast()) };
    (u32::try_from(w).unwrap(), u32::try_from(h).unwrap(), rgba)
}

/// A live libwebp demuxer, deleted exactly once when dropped. Same guard pattern
/// as [`AnimDecoder`]: even if an assertion unwinds mid-use, `Drop` still runs
/// `WebPDemuxDelete` exactly once, so the demuxer can neither leak nor double-free.
struct Demuxer(*mut libwebp_sys::WebPDemuxer);

impl Drop for Demuxer {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null demuxer returned by `WebPDemuxInternal`;
        // it is deleted exactly once — here — and never used afterwards.
        unsafe { libwebp_sys::WebPDemuxDelete(self.0) };
    }
}

/// Demux `webp` and return the bytes of its first `fourcc` chunk (`fourcc` is a
/// 4-byte NUL-terminated C string, e.g. `b"ICCP\0"`, `b"EXIF\0"`, `b"XMP \0"` —
/// note the trailing space in `XMP `), or `None` when that chunk is absent.
fn libwebp_demux_chunk(webp: &[u8], fourcc: &[u8]) -> Option<Vec<u8>> {
    // A borrowed view of `webp`; the demuxer reads these bytes until it is
    // deleted, and `webp` outlives this call.
    let data = libwebp_sys::WebPData {
        bytes: webp.as_ptr(),
        size: webp.len(),
    };
    // SAFETY: `data` describes the valid `webp` buffer (kept alive for the whole
    // call); `allow_partial` is 0, a null state out-pointer is permitted, and we
    // pass the demux ABI version the header expects.
    let dmux = unsafe {
        libwebp_sys::WebPDemuxInternal(
            &raw const data,
            0,
            core::ptr::null_mut(),
            i32::try_from(libwebp_sys::WEBP_DEMUX_ABI_VERSION).unwrap(),
        )
    };
    assert!(!dmux.is_null(), "WebPDemuxInternal failed");
    // From here the demuxer is owned by the guard and freed exactly once, even if
    // an assertion below unwinds.
    let _guard = Demuxer(dmux);

    // SAFETY: `WebPChunkIterator` is plain-old-data (pointers + integers), so an
    // all-zero bit pattern is a valid, unpopulated iterator for GetChunk to fill.
    let mut iter: libwebp_sys::WebPChunkIterator = unsafe { core::mem::zeroed() };
    // SAFETY: `dmux` is live; `fourcc` is a 4-byte NUL-terminated C string; `iter`
    // is a writable out-parameter; `chunk_number` 1 selects the first such chunk.
    let found = unsafe {
        libwebp_sys::WebPDemuxGetChunk(
            dmux,
            fourcc.as_ptr().cast::<core::ffi::c_char>(),
            1,
            &raw mut iter,
        )
    };
    let result = if found != 0 {
        // SAFETY: on success libwebp points `iter.chunk` at `size` valid bytes it
        // owns (valid until the iterator is released), so copy them out now.
        Some(unsafe { std::slice::from_raw_parts(iter.chunk.bytes, iter.chunk.size) }.to_vec())
    } else {
        None
    };
    // SAFETY: `iter` is a valid iterator (zero-initialized, then optionally filled
    // by GetChunk); releasing it is a no-op for chunk iterators and safe either
    // way, and it is released exactly once here.
    unsafe { libwebp_sys::WebPDemuxReleaseChunkIterator(&raw mut iter) };
    result
    // `_guard` drops here, deleting the demuxer exactly once.
}

/// A live libwebp demux `WebPAnimDecoder`, deleted exactly once when dropped.
///
/// Owning the decoder in a guard means [`libwebp_anim_composite`] can `assert!`
/// freely: even if an assertion unwinds mid-decode, `Drop` still runs
/// `WebPAnimDecoderDelete` exactly once, so the decoder can neither leak nor be
/// double-freed.
struct AnimDecoder(*mut libwebp_sys::WebPAnimDecoder);

impl Drop for AnimDecoder {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null decoder returned by `WebPAnimDecoderNew`;
        // it is deleted exactly once — here — and never used afterwards.
        unsafe { libwebp_sys::WebPAnimDecoderDelete(self.0) };
    }
}

/// Composite an animated `webp` with libwebp's demux `WebPAnimDecoder` into
/// `(canvas_width, canvas_height, per-frame RGBA)`.
///
/// Each returned buffer is exactly `canvas_w * canvas_h * 4` bytes: libwebp
/// composites every frame onto the canvas (honoring the `ANMF` blend/dispose
/// model) *before* returning it, so the result lines up frame-for-frame with
/// the `lossless` codec's [`webpkit::lossless::Frames::composited`] output.
fn libwebp_anim_composite(webp: &[u8]) -> (u32, u32, Vec<Vec<u8>>) {
    // A borrowed view of `webp`; the decoder holds a demuxer over these bytes
    // until it is deleted, and `webp` outlives this call.
    let data = libwebp_sys::WebPData {
        bytes: webp.as_ptr(),
        size: webp.len(),
    };
    // SAFETY: `WebPAnimDecoderOptions` is plain-old-data whose only non-integer
    // field is a `repr(u32)` enum with a `0` variant, so an all-zero bit pattern
    // is a valid instance for `WebPAnimDecoderOptionsInit` to fill in.
    let mut options: libwebp_sys::WebPAnimDecoderOptions = unsafe { core::mem::zeroed() };
    // SAFETY: `options` points at a live, writable value; the safe wrapper passes
    // the demux ABI version internally.
    let init = unsafe { libwebp_sys::WebPAnimDecoderOptionsInit(&raw mut options) };
    assert!(init != 0, "WebPAnimDecoderOptionsInit failed");
    options.color_mode = libwebp_sys::WEBP_CSP_MODE::MODE_RGBA;

    // SAFETY: `data` describes the valid `webp` buffer (kept alive for the whole
    // call), `options` is initialized, and the safe wrapper passes the demux ABI
    // version internally.
    let dec = unsafe { libwebp_sys::WebPAnimDecoderNew(&raw const data, &raw const options) };
    assert!(!dec.is_null(), "WebPAnimDecoderNew failed");
    // From here the decoder is owned by the guard and freed exactly once, even if
    // an assertion below unwinds.
    let _guard = AnimDecoder(dec);

    let mut info = libwebp_sys::WebPAnimInfo::default();
    // SAFETY: `dec` is a live decoder; `info` is a writable out-parameter.
    let got_info = unsafe { libwebp_sys::WebPAnimDecoderGetInfo(dec, &raw mut info) };
    assert!(got_info != 0, "WebPAnimDecoderGetInfo failed");
    let (canvas_w, canvas_h) = (info.canvas_width, info.canvas_height);
    let frame_len = usize::try_from(canvas_w).unwrap() * usize::try_from(canvas_h).unwrap() * 4;

    let mut frames = Vec::new();
    // SAFETY: `dec` is live; `HasMoreFrames` only reads decoder state.
    while unsafe { libwebp_sys::WebPAnimDecoderHasMoreFrames(dec) } > 0 {
        let mut buf: *mut u8 = core::ptr::null_mut();
        let mut timestamp: core::ffi::c_int = 0;
        // SAFETY: `dec` is live; libwebp writes the address of its internal,
        // canvas-sized RGBA buffer into `buf` and the frame end timestamp into
        // `timestamp`.
        let got =
            unsafe { libwebp_sys::WebPAnimDecoderGetNext(dec, &raw mut buf, &raw mut timestamp) };
        assert!(got != 0 && !buf.is_null(), "WebPAnimDecoderGetNext failed");
        // SAFETY: on success `buf` points at `frame_len` valid, already-composited
        // RGBA bytes owned by the decoder (valid until the next GetNext/Delete),
        // so copy them out immediately.
        let frame = unsafe { std::slice::from_raw_parts(buf, frame_len) }.to_vec();
        frames.push(frame);
    }
    assert_eq!(
        frames.len(),
        usize::try_from(info.frame_count).unwrap(),
        "libwebp yielded a different frame count than it reported",
    );
    (canvas_w, canvas_h, frames)
    // `_guard` drops here, deleting the decoder exactly once.
}

/// Composite an animated `webp` with the `lossless` codec into the same
/// `(canvas_width, canvas_height, per-frame RGBA)` shape as
/// [`libwebp_anim_composite`], so the two can be compared directly.
fn webpkit_anim_composite(webp: &[u8]) -> webpkit::lossless::Result<(u32, u32, Vec<Vec<u8>>)> {
    let frames = webpkit::lossless::decode_frames(webp)?;
    let canvas = frames.anim_info().canvas;
    let mut out = Vec::new();
    for frame in frames.composited() {
        // Each `CompositedFrame` is the whole canvas at one point in time; its
        // default layout is RGBA8, matching libwebp's `MODE_RGBA`.
        out.push(frame?.image().as_bytes().to_vec());
    }
    Ok((canvas.width(), canvas.height(), out))
}

/// Arbitrary small opaque animations: a `1..=16` square canvas and `1..=4`
/// full-canvas frames. Alpha is forced to `255`, so every frame is an opaque
/// full-canvas key frame — libwebp and the `lossless` codec both composite it to exactly the
/// frame's own pixels — which keeps the differential focused on the muxer +
/// per-frame decode path without depending on partial-alpha blend agreement.
fn arbitrary_animation() -> impl Strategy<Value = (u32, u32, Vec<Vec<u8>>)> {
    (1u32..=16, 1u32..=16, 1usize..=4)
        .prop_flat_map(|(width, height, frame_count)| {
            let len = usize::try_from(width * height * 4).unwrap();
            let frame = proptest::collection::vec(any::<u8>(), len..=len);
            (
                Just(width),
                Just(height),
                proptest::collection::vec(frame, frame_count..=frame_count),
            )
        })
        .prop_map(|(width, height, mut frames)| {
            for frame in &mut frames {
                for px in frame.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
            (width, height, frames)
        })
}

/// A frame in a synthesized sub-rectangle animation: its placement, blend/dispose
/// mode, and RGBA pixels. Alpha is quantized to `{0, 128, 255}` so full, partial,
/// and no transparency all appear without pinning any single alpha value.
#[derive(Clone, Debug)]
struct SubRectFrame {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    overwrite: bool,
    dispose_bg: bool,
    rgba: Vec<u8>,
}

/// Arbitrary animations whose frames are sub-rectangles (`x`/`y` possibly non-zero,
/// size possibly `< canvas`) with mixed partial alpha and every blend/dispose
/// combination. This reaches the compositor paths the opaque full-canvas
/// `arbitrary_animation` never does: partial-alpha blend over prior canvas,
/// background disposal of a sub-rectangle, and key-frame promotion — the exact
/// logic `Compositor::paint` gets wrong when it keys on the previous frame. Offsets
/// are emitted as `2 * half` because the `ANMF` header stores them in 2-px units.
fn arbitrary_subrect_animation() -> impl Strategy<Value = (u32, u32, Vec<SubRectFrame>)> {
    (2u32..=8, 2u32..=8).prop_flat_map(|(cw, ch)| {
        let frame = (1u32..=cw, 1u32..=ch).prop_flat_map(move |(w, h)| {
            let len = usize::try_from(w * h * 4).unwrap();
            (
                0..=(cw - w) / 2,
                0..=(ch - h) / 2,
                any::<bool>(),
                any::<bool>(),
                proptest::collection::vec(any::<u8>(), len..=len),
            )
                .prop_map(move |(xh, yh, overwrite, dispose_bg, mut rgba)| {
                    for px in rgba.chunks_exact_mut(4) {
                        px[3] = match px[3] % 3 {
                            0 => 0,
                            1 => 128,
                            _ => 255,
                        };
                    }
                    SubRectFrame {
                        x: xh * 2,
                        y: yh * 2,
                        w,
                        h,
                        overwrite,
                        dispose_bg,
                        rgba,
                    }
                })
        });
        (Just(cw), Just(ch), proptest::collection::vec(frame, 1..=4))
    })
}

/// Arbitrary small RGBA images (both sides `1..=48`).
fn arbitrary_image() -> impl Strategy<Value = (u32, u32, Vec<u8>)> {
    (1u32..=48, 1u32..=48).prop_flat_map(|(width, height)| {
        let len = usize::try_from(width * height * 4).unwrap();
        (
            Just(width),
            Just(height),
            proptest::collection::vec(any::<u8>(), len..=len),
        )
    })
}

/// Arbitrary opaque images (`alpha = 255`). libwebp's default lossless encode is
/// **not** byte-exact for fully-transparent pixels — it discards the invisible
/// RGB under `alpha == 0` (the `exact` flag, which we do not set, would keep it).
/// Forcing opacity makes libwebp's encode a true identity, so a source
/// comparison isolates *our* decoder rather than libwebp's cleanup.
fn arbitrary_opaque_image() -> impl Strategy<Value = (u32, u32, Vec<u8>)> {
    arbitrary_image().prop_map(|(width, height, mut rgba)| {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        (width, height, rgba)
    })
}

proptest! {
    /// The suspend/resume streaming decoder reproduces the one-shot decode over
    /// every input split, on **libwebp-authored** payloads. libwebp's lossless
    /// encoder uses predictor + cross-color transforms that our own encoder does
    /// not emit, so this is the cross-transform coverage the in-crate proptests
    /// (subtract-green only) cannot reach for the streaming path. Opaque images
    /// keep libwebp's encode a lossless identity (see `arbitrary_opaque_image`).
    #[test]
    fn stream_equals_one_shot_on_libwebp_payloads((width, height, rgba) in arbitrary_opaque_image()) {
        let webp = libwebp_encode_lossless(&rgba, width, height);
        prop_assert!(
            webpkit::lossless::__vp8l_stream_equals_one_shot(&webp),
            "streaming decode diverged from one-shot on a libwebp-authored payload"
        );
    }
}

proptest! {
    /// Decoding a libwebp-encoded stream yields byte-identical pixels in the `lossless` codec
    /// and libwebp.
    #[test]
    fn our_decode_matches_libwebp((width, height, rgba) in arbitrary_image()) {
        let webp = libwebp_encode_lossless(&rgba, width, height);
        let (lw, lh, lref) = libwebp_decode(&webp);
        let (dims, ours) = webpkit::lossless::decode_rgba(&webp)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless decode failed: {e}")))?;
        prop_assert_eq!((dims.width(), dims.height()), (lw, lh));
        prop_assert_eq!(ours, lref);
    }

    /// libwebp decodes *our* encoder output back to the exact source.
    #[test]
    fn libwebp_decodes_our_encode((width, height, rgba) in arbitrary_image()) {
        let dims = webpkit::lossless::Dimensions::new(width, height).unwrap();
        let image = webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, &rgba).unwrap();
        let webp = webpkit::lossless::encode(image, &webpkit::lossless::EncoderConfig::default())
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless encode failed: {e}")))?;
        let (lw, lh, lref) = libwebp_decode(&webp);
        prop_assert_eq!((lw, lh), (width, height));
        prop_assert_eq!(lref, rgba);
    }

    /// the `lossless` codec decodes *libwebp's* encoder output back to the exact source. Uses
    /// opaque images so libwebp's encode is a true identity (see
    /// `arbitrary_opaque_image`).
    #[test]
    fn our_decode_of_libwebp_encode((width, height, rgba) in arbitrary_opaque_image()) {
        let webp = libwebp_encode_lossless(&rgba, width, height);
        let (dims, ours) = webpkit::lossless::decode_rgba(&webp)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless decode failed: {e}")))?;
        prop_assert_eq!((dims.width(), dims.height()), (width, height));
        prop_assert_eq!(ours, rgba);
    }
}

proptest! {
    // The `Best` families run per-tile forward transforms (a full i8 sweep for
    // cross-color), so cap the case count to keep the differential brisk.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// libwebp decodes the Tier 3 `Best` encoder output back to the exact source.
    /// This is the authoritative check that our forward-transform emit — the
    /// predictor / cross-color / palette headers, the reduced palette working
    /// width, and the nested tile / color-map sub-images — is valid VP8L the
    /// reference decoder reads. Opaque images keep libwebp's *decode* a true
    /// identity (small ones also drive the palette family, which needs
    /// `<= 256` distinct colors), and the `lossless` codec's own decode of the same bytes is
    /// checked alongside.
    #[test]
    fn libwebp_decodes_our_best_encode((width, height, rgba) in arbitrary_opaque_image()) {
        let dims = webpkit::lossless::Dimensions::new(width, height).unwrap();
        let image = webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, &rgba).unwrap();
        let config = webpkit::lossless::EncoderConfig::default().with_effort(webpkit::lossless::Effort::level(9));
        let webp = webpkit::lossless::encode(image, &config)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless Best encode failed: {e}")))?;
        // webpkit::lossless round-trips its own Best output...
        let (odims, ours) = webpkit::lossless::decode_rgba(&webp)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless decode failed: {e}")))?;
        prop_assert_eq!((odims.width(), odims.height()), (width, height));
        prop_assert_eq!(&ours, &rgba);
        // ...and so does the reference decoder.
        let (lw, lh, lref) = libwebp_decode(&webp);
        prop_assert_eq!((lw, lh), (width, height));
        prop_assert_eq!(lref, rgba);
    }
}

/// Structure-bearing opaque RGBA images, one per transform family, so the
/// reference decoder validates the *actual* predictor / cross-color / palette
/// emit rather than the Tier 0/1/2 floor that random noise collapses to. Each is
/// `width * height * 4` RGBA bytes: a smooth grayscale gradient (predictor), a
/// green<->red/blue correlated ramp with green kept `< 128` so the signed
/// cross-color multiplier cancels red and blue (cross-color), and a scattered
/// eight-color field (palette).
fn best_transform_fixtures() -> [(u32, u32, Vec<u8>); 4] {
    let mut gradient = Vec::new();
    for y in 0..16u8 {
        for x in 0..16u8 {
            let v = (x + y) * 8; // max (15 + 15) * 8 = 240, fits a u8
            gradient.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut correlated = Vec::new();
    for i in 0..64u8 {
        let g = (i * 2) & 0x7f; // max 126 < 128
        correlated.extend_from_slice(&[g >> 1, g, g >> 2, 255]);
    }
    let palette: [[u8; 3]; 8] = [
        [10, 20, 30],
        [200, 40, 60],
        [70, 220, 90],
        [100, 110, 240],
        [33, 66, 99],
        [240, 240, 10],
        [15, 250, 250],
        [250, 15, 130],
    ];
    let mut paletted = Vec::new();
    for i in 0..256usize {
        let c = palette[(i * 7 + i / 16) % palette.len()];
        paletted.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    // Regional image: top red-dominant, bottom blue-dominant, exercising the
    // meta-Huffman multi-group emit (matching the crate's meta round-trip test).
    let mut regional = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            if y < 8 {
                regional.extend_from_slice(&[
                    u8::try_from((x * 16 + y) & 0xff).unwrap(),
                    u8::try_from(x & 7).unwrap(),
                    0,
                    255,
                ]);
            } else {
                regional.extend_from_slice(&[
                    0,
                    u8::try_from(x & 7).unwrap(),
                    u8::try_from((x * 16 + (y - 8)) & 0xff).unwrap(),
                    255,
                ]);
            }
        }
    }
    [
        (16, 16, gradient),
        (8, 8, correlated),
        (16, 16, paletted),
        (16, 16, regional),
    ]
}

/// The reference decoder reproduces the source from each `Best` transform-family
/// emit. Paired with the in-crate assertions that Best beats the floor on these
/// same shapes, this proves the predictor, cross-color, and palette forward
/// emits are valid VP8L — not merely self-consistent with our own decoder.
#[test]
fn libwebp_decodes_best_transform_families() {
    for (width, height, rgba) in best_transform_fixtures() {
        let dims = webpkit::lossless::Dimensions::new(width, height).unwrap();
        let image =
            webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, &rgba)
                .unwrap();
        let config = webpkit::lossless::EncoderConfig::default()
            .with_effort(webpkit::lossless::Effort::level(9));
        let webp =
            webpkit::lossless::encode(image, &config).expect("webpkit::lossless Best encode");
        // webpkit::lossless decodes its own Best output.
        let (odims, ours) =
            webpkit::lossless::decode_rgba(&webp).expect("webpkit::lossless decode");
        assert_eq!((odims.width(), odims.height()), (width, height));
        assert_eq!(ours, rgba);
        // The reference decoder reproduces the source from our transform emit.
        let (lw, lh, lref) = libwebp_decode(&webp);
        assert_eq!((lw, lh), (width, height));
        assert_eq!(lref, rgba, "libwebp disagreed on a Best transform emit");
    }
}

/// The committed animation fixture (a 16x16 three-frame lossless animation
/// authored by `img2webp`). Compositing it with libwebp's `WebPAnimDecoder` and
/// with the `lossless` codec must agree canvas-dimensions and RGBA byte-for-byte on every frame.
/// This is the deterministic, encoder-independent anchor of the animation
/// differential (it exercises libwebp's real transparent-dispose/blend model,
/// not our synthesized frames).
const ANIMATION_FIXTURE: &[u8] = include_bytes!(
    "../../webpkit-lossless-conformance/fixtures/decode/animation_frames/input.webp"
);

#[test]
fn anim_composited_matches_libwebp_on_fixture() {
    let (lw, lh, libwebp_frames) = libwebp_anim_composite(ANIMATION_FIXTURE);
    let (ow, oh, our_frames) = webpkit_anim_composite(ANIMATION_FIXTURE)
        .expect("webpkit::lossless failed to decode the animation fixture");

    assert_eq!(
        (ow, oh),
        (lw, lh),
        "canvas dimensions disagree: webpkit::lossless {ow}x{oh} vs libwebp {lw}x{lh}",
    );
    assert_eq!(
        our_frames.len(),
        libwebp_frames.len(),
        "frame count disagrees: webpkit::lossless {} vs libwebp {}",
        our_frames.len(),
        libwebp_frames.len(),
    );
    for (i, (ours, reference)) in our_frames.iter().zip(&libwebp_frames).enumerate() {
        assert_eq!(
            ours.len(),
            reference.len(),
            "frame {i} RGBA byte length disagrees",
        );
        assert_eq!(ours, reference, "frame {i} composited RGBA disagrees");
    }
}

proptest! {
    /// Compositing a `lossless`-codec-synthesized animation with libwebp's `WebPAnimDecoder`
    /// and with the `lossless` codec agrees on canvas dimensions and per-frame RGBA. This drives
    /// the muxer (`AnimationEncoder`) + libwebp's demux + both compositors in one
    /// loop; opaque full-canvas frames keep the comparison independent of
    /// partial-alpha blend agreement (see `arbitrary_animation`).
    #[test]
    fn anim_composited_matches_libwebp_for_synthesized(
        (width, height, frames) in arbitrary_animation(),
    ) {
        let canvas = webpkit::lossless::Dimensions::new(width, height).unwrap();
        let frame_meta = webpkit::lossless::FrameMeta::new(
            0,
            0,
            canvas,
            40,
            webpkit::lossless::BlendMode::Blend,
            webpkit::lossless::DisposalMode::Keep,
        );
        // The type-state builder becomes `HasFrames` after the first frame, so
        // seed it once and then fold the remaining frames into the same state.
        let first = webpkit::lossless::ImageRef::new(canvas, webpkit::lossless::PixelLayout::Rgba8, &frames[0]).unwrap();
        let mut encoder = webpkit::AnimationEncoder::new(canvas)
            .add_frame(first, frame_meta)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless add_frame failed: {e}")))?;
        for frame in &frames[1..] {
            let image = webpkit::lossless::ImageRef::new(canvas, webpkit::lossless::PixelLayout::Rgba8, frame).unwrap();
            encoder = encoder
                .add_frame(image, frame_meta)
                .map_err(|e| TestCaseError::fail(format!("webpkit::lossless add_frame failed: {e}")))?;
        }
        let webp = encoder.finish();

        let (lw, lh, libwebp_frames) = libwebp_anim_composite(&webp);
        let (ow, oh, our_frames) = webpkit_anim_composite(&webp)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless decode failed: {e}")))?;

        prop_assert_eq!((ow, oh), (width, height));
        prop_assert_eq!((lw, lh), (width, height));
        prop_assert_eq!(our_frames.len(), frames.len());
        prop_assert_eq!(&our_frames, &libwebp_frames);
    }
}

proptest! {
    // A lossless VP8L encode per frame plus two full composites per case, so keep
    // the case count modest.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Compositing a sub-rectangle, partial-alpha, mixed blend/dispose animation
    /// with libwebp's `WebPAnimDecoder` and with the `lossless` codec agrees on every
    /// composited frame byte-for-byte. Unlike `arbitrary_animation` (opaque,
    /// full-canvas, every frame a key frame), this drives the blend-over-canvas,
    /// sub-rectangle background-disposal, and key-frame-promotion paths — the exact
    /// `Compositor::paint` logic that keying on the *previous* frame's disposal
    /// corrupts. It is the external, encoder-independent check on that fix.
    #[test]
    fn anim_composited_matches_libwebp_subrect_partial_alpha(
        (cw, ch, frames) in arbitrary_subrect_animation(),
    ) {
        let canvas = webpkit::lossless::Dimensions::new(cw, ch).unwrap();
        let to_meta = |f: &SubRectFrame| {
            webpkit::lossless::FrameMeta::new(
                f.x,
                f.y,
                webpkit::lossless::Dimensions::new(f.w, f.h).unwrap(),
                40,
                if f.overwrite {
                    webpkit::lossless::BlendMode::Overwrite
                } else {
                    webpkit::lossless::BlendMode::Blend
                },
                if f.dispose_bg {
                    webpkit::lossless::DisposalMode::Background
                } else {
                    webpkit::lossless::DisposalMode::Keep
                },
            )
        };
        // The type-state builder becomes `HasFrames` after the first frame.
        let dims0 = webpkit::lossless::Dimensions::new(frames[0].w, frames[0].h).unwrap();
        let first =
            webpkit::lossless::ImageRef::new(dims0, webpkit::lossless::PixelLayout::Rgba8, &frames[0].rgba)
                .unwrap();
        let mut encoder = webpkit::AnimationEncoder::new(canvas)
            .add_frame(first, to_meta(&frames[0]))
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless add_frame failed: {e}")))?;
        for f in &frames[1..] {
            let dims = webpkit::lossless::Dimensions::new(f.w, f.h).unwrap();
            let image =
                webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, &f.rgba)
                    .unwrap();
            encoder = encoder
                .add_frame(image, to_meta(f))
                .map_err(|e| TestCaseError::fail(format!("webpkit::lossless add_frame failed: {e}")))?;
        }
        let webp = encoder.finish();

        let (lw, lh, libwebp_frames) = libwebp_anim_composite(&webp);
        let (ow, oh, our_frames) = webpkit_anim_composite(&webp)
            .map_err(|e| TestCaseError::fail(format!("webpkit::lossless decode failed: {e}")))?;

        prop_assert_eq!((ow, oh), (cw, ch));
        prop_assert_eq!((lw, lh), (cw, ch));
        prop_assert_eq!(our_frames.len(), frames.len());
        prop_assert_eq!(&our_frames, &libwebp_frames);
    }
}

/// `encode_image`'s metadata-bearing output is valid to libwebp and its metadata
/// chunks survive byte-exact. The Preserve case proves the whole
/// `VP8X`+`ICCP`+`VP8L`+`EXIF`+`XMP ` file the writer emits is well-formed to the
/// reference demuxer/decoder; the `StripPrivate` case proves the privacy strip
/// actually removes the Exif/XMP chunks while keeping ICC. The ICC body is an odd
/// length, so this also checks the RIFF pad byte is not counted in the chunk size.
#[test]
fn encode_image_metadata_survives_libwebp() {
    // 4x2 fully-opaque RGBA.
    let rgba: Vec<u8> = (0..8u8)
        .flat_map(|i| [i * 10, i * 10 + 1, i * 10 + 2, 255])
        .collect();
    let dims = webpkit::lossless::Dimensions::new(4, 2).unwrap();
    let icc = b"icc-bytes".to_vec(); // 9 bytes: odd -> RIFF pad
    let exif = b"exif-bytes".to_vec();
    let xmp = b"<x:xmpmeta/>".to_vec();
    let img =
        webpkit::lossless::Image::new(dims, webpkit::lossless::PixelLayout::Rgba8, rgba.clone())
            .unwrap()
            .with_metadata(
                webpkit::lossless::Metadata::none()
                    .with_icc_profile(icc.clone())
                    .with_exif(exif.clone())
                    .with_xmp(xmp.clone()),
            );

    // Preserve (default): the reference decoder reproduces the pixels and every
    // metadata chunk survives byte-exact.
    let file = webpkit::lossless::encode_image(&img, &webpkit::lossless::EncoderConfig::default())
        .unwrap();
    let (lw, lh, lref) = libwebp_decode(&file);
    assert_eq!((lw, lh), (4, 2), "libwebp disagreed on canvas dimensions");
    assert_eq!(lref, rgba, "libwebp disagreed on the decoded pixels");
    assert_eq!(libwebp_demux_chunk(&file, b"ICCP\0"), Some(icc.clone()));
    assert_eq!(libwebp_demux_chunk(&file, b"EXIF\0"), Some(exif));
    assert_eq!(libwebp_demux_chunk(&file, b"XMP \0"), Some(xmp));

    // StripPrivate: ICC survives, Exif and XMP are gone.
    let stripped = webpkit::lossless::encode_image(
        &img,
        &webpkit::lossless::EncoderConfig::default()
            .with_metadata_policy(webpkit::lossless::MetadataPolicy::StripPrivate),
    )
    .unwrap();
    assert_eq!(libwebp_demux_chunk(&stripped, b"ICCP\0"), Some(icc));
    assert_eq!(libwebp_demux_chunk(&stripped, b"EXIF\0"), None);
    assert_eq!(libwebp_demux_chunk(&stripped, b"XMP \0"), None);
}
