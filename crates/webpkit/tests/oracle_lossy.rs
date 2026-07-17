//! Differential oracle for the VP8 (lossy) decoder: cross-check the `lossy` codec
//! against the libwebp C reference, in-process.
//!
//! Enabled only with `--features oracle` (which links `libwebp-sys` and the
//! vendored reference library); never part of a normal build. The plan is a
//! two-level check, because a VP8 *decode* is bit-exact per RFC 6386 but the
//! final YUV→RGB conversion is an implementation choice:
//!
//! - **Level A** (bit-exact reconstruction): our reconstructed YUV planes equal
//!   libwebp's `WebPDecodeYUV` planes. That validates the boolean decoder,
//!   headers, prediction, IDCT and loop filter independently of color
//!   conversion. It needs crate internals, so it lands in-crate (`src`) with the
//!   pixel pipeline.
//! - **Level B** (color conversion): our public [`webpkit::lossy::decode`] RGBA equals
//!   `WebPDecodeRGBA`, exercised here through the public API.
//!
//! Until the pixel pipeline lands, this file verifies the *harness itself*:
//! libwebp encodes a lossy stream, our container layer classifies it as lossy,
//! our key-frame header parser agrees on its dimensions against a real stream,
//! and both libwebp decode paths (RGBA and planar YUV) round-trip — so the FFI
//! bindings and the differential plumbing are proven before we rely on them.
#![cfg(feature = "oracle")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::many_single_char_names,
    reason = "test-only integration crate: unwrap/panic are the accepted style for \
              provably-infallible conversions and reference-library successes, and \
              the short w/h/u/v/y names mirror the libwebp plane FFI they wrap"
)]

use proptest::prelude::*;
use webpkit::container::reader::{ImageChunk, locate_image};
use webpkit_lossy_proptest::arbitrary_lossy_rgb;

/// Encode `rgb` (`width * height * 3` bytes) as a lossy VP8 WebP at `quality`
/// (`0.0..=100.0`) with the libwebp reference simple API.
fn libwebp_encode_lossy(rgb: &[u8], width: u32, height: u32, quality: f32) -> Vec<u8> {
    let (w, h) = (
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    );
    let stride = i32::try_from(width * 3).unwrap();
    let mut out: *mut u8 = core::ptr::null_mut();
    // SAFETY: `rgb` holds `width * height * 3` bytes at the given stride; libwebp
    // writes the freshly allocated stream pointer into `out`.
    let size =
        unsafe { libwebp_sys::WebPEncodeRGB(rgb.as_ptr(), w, h, stride, quality, &raw mut out) };
    assert!(!out.is_null() && size > 0, "libwebp lossy encode failed");
    // SAFETY: on success libwebp guarantees `out` points at `size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out, size) }.to_vec();
    // SAFETY: `out` was allocated by libwebp and is freed exactly once here.
    unsafe { libwebp_sys::WebPFree(out.cast()) };
    bytes
}

/// Decode `webp` with libwebp into `(width, height, rgba)`.
fn libwebp_decode_rgba(webp: &[u8]) -> (u32, u32, Vec<u8>) {
    let (mut w, mut h) = (0i32, 0i32);
    // SAFETY: `webp`/`len` describe a valid buffer; libwebp allocates the output
    // and writes the dimensions through the out-pointers.
    let ptr =
        unsafe { libwebp_sys::WebPDecodeRGBA(webp.as_ptr(), webp.len(), &raw mut w, &raw mut h) };
    assert!(!ptr.is_null(), "libwebp RGBA decode failed");
    let len = usize::try_from(w).unwrap() * usize::try_from(h).unwrap() * 4;
    // SAFETY: libwebp returned `w * h * 4` valid RGBA bytes at `ptr`.
    let rgba = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    // SAFETY: `ptr` was allocated by libwebp and is freed exactly once here.
    unsafe { libwebp_sys::WebPFree(ptr.cast()) };
    (u32::try_from(w).unwrap(), u32::try_from(h).unwrap(), rgba)
}

/// Decode `webp` with libwebp into packed planar YUV 4:2:0: `(width, height, y,
/// u, v)`, with each plane's stride collapsed to its exact width.
fn libwebp_decode_yuv(webp: &[u8]) -> (u32, u32, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut w, mut h) = (0i32, 0i32);
    let mut u_ptr: *mut u8 = core::ptr::null_mut();
    let mut v_ptr: *mut u8 = core::ptr::null_mut();
    let (mut y_stride, mut uv_stride) = (0i32, 0i32);
    // SAFETY: every out-parameter is a writable local; `webp`/`len` is a valid
    // buffer. libwebp allocates one buffer backing all three planes and writes
    // the plane pointers, dimensions and strides through the out-pointers.
    let y_ptr = unsafe {
        libwebp_sys::WebPDecodeYUV(
            webp.as_ptr(),
            webp.len(),
            &raw mut w,
            &raw mut h,
            &raw mut u_ptr,
            &raw mut v_ptr,
            &raw mut y_stride,
            &raw mut uv_stride,
        )
    };
    assert!(!y_ptr.is_null(), "libwebp YUV decode failed");
    let (width, height) = (usize::try_from(w).unwrap(), usize::try_from(h).unwrap());
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let (ys, uvs) = (
        usize::try_from(y_stride).unwrap(),
        usize::try_from(uv_stride).unwrap(),
    );
    let y = copy_plane(y_ptr, ys, width, height);
    let u = copy_plane(u_ptr, uvs, cw, ch);
    let v = copy_plane(v_ptr, uvs, cw, ch);
    // SAFETY: `y_ptr` heads the single libwebp allocation backing all three
    // planes; freeing it once releases them.
    unsafe { libwebp_sys::WebPFree(y_ptr.cast()) };
    (
        u32::try_from(w).unwrap(),
        u32::try_from(h).unwrap(),
        y,
        u,
        v,
    )
}

/// Copy `rows` rows of `cols` bytes from a strided plane at `ptr` into a packed
/// `cols * rows` buffer.
fn copy_plane(ptr: *const u8, stride: usize, cols: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        // SAFETY: libwebp's plane has `rows` rows spaced `stride >= cols` bytes
        // apart from `ptr`; the `cols` bytes at row `r` are all in-bounds.
        let row = unsafe { std::slice::from_raw_parts(ptr.add(r * stride), cols) };
        out.extend_from_slice(row);
    }
    out
}

/// The raw `VP8 ` payload of a WebP file, via the shared container dispatcher.
fn vp8_payload(webp: &[u8]) -> Vec<u8> {
    match locate_image(webp).unwrap() {
        ImageChunk::Lossy(payload) => payload.to_vec(),
        ImageChunk::Lossless(_) => panic!("expected a lossy VP8 stream"),
    }
}

/// A small deterministic opaque RGB gradient (integer-only, no reference needed).
fn gradient_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for y in 0..height {
        for x in 0..width {
            let r = u8::try_from((x * 255) / width).unwrap_or(0);
            let g = u8::try_from((y * 255) / height).unwrap_or(0);
            let b = u8::try_from(((x + y) * 255) / (width + height)).unwrap_or(0);
            out.extend_from_slice(&[r, g, b]);
        }
    }
    out
}

/// Deterministic integer pseudo-noise RGB — drives high-detail content that
/// libwebp codes with intra-4×4 macroblocks and non-trivial loop filtering.
fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for y in 0..height {
        for x in 0..width {
            let mut s = x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503))
                .wrapping_add(0x9e37_79b9);
            s ^= s >> 13;
            s = s.wrapping_mul(0x85eb_ca6b);
            s ^= s >> 16;
            out.push(u8::try_from(s & 0xff).unwrap_or(0));
            out.push(u8::try_from((s >> 8) & 0xff).unwrap_or(0));
            out.push(u8::try_from((s >> 16) & 0xff).unwrap_or(0));
        }
    }
    out
}

/// A high-contrast 4×4 checkerboard — sharp edges that exercise the deblocking
/// filter and vertical/horizontal predictors.
fn checker_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for y in 0..height {
        for x in 0..width {
            let v = if (x / 4 + y / 4) % 2 == 0 { 30 } else { 220 };
            out.extend_from_slice(&[v, v, v]);
        }
    }
    out
}

/// A flat single-color RGB fill. libwebp codes almost every macroblock of such
/// content as skipped, so it emits the per-macroblock skip flag (`use_skip`) that
/// stresses the decoder's skip path.
fn solid_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for _ in 0..width * height {
        out.extend_from_slice(&[90, 150, 210]);
    }
    out
}

/// Fine vertical stripes (period 2 px): a flat 16×16 luma predictor fits them
/// poorly while the 4×4 directional predictors fit the local structure, so our Best
/// encoder codes several macroblocks as intra-4×4 — the content that exercises the
/// encoder's i4x4 mode-emission and no-Y2 token path against libwebp.
fn stripe_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for _ in 0..height {
        for x in 0..width {
            let v = if (x / 2) % 2 == 0 { 220 } else { 20 };
            out.extend_from_slice(&[v, v, v]);
        }
    }
    out
}

/// A named RGB content generator (`width`, `height`) → interleaved RGB bytes.
type ContentGen = fn(u32, u32) -> Vec<u8>;

/// Regenerate the committed lossy conformance fixtures under `tests/fixtures/`:
/// for each case a real libwebp `VP8 ` payload and its `WebPDecodeRGBA` golden.
/// Run explicitly: `cargo test -p webpkit --features oracle --test oracle_lossy -- --ignored gen`.
#[test]
#[ignore = "regenerates committed fixtures; run explicitly"]
fn gen_fixtures() {
    let cases: [(&str, u32, u32, f32, ContentGen); 4] = [
        ("noise_32x24_q30", 32, 24, 30.0, noise_rgb),
        ("checker_16x16_q20", 16, 16, 20.0, checker_rgb),
        ("gradient_17x13_q80", 17, 13, 80.0, gradient_rgb),
        ("noise_5x9_q50", 5, 9, 50.0, noise_rgb),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, w, h, q, generate) in cases {
        let webp = libwebp_encode_lossy(&generate(w, h), w, h, q);
        let payload = vp8_payload(&webp);
        let (_, _, rgba) = libwebp_decode_rgba(&webp);
        std::fs::write(dir.join(format!("{name}.vp8")), &payload).unwrap();
        std::fs::write(dir.join(format!("{name}.rgba")), &rgba).unwrap();
    }
}

#[test]
fn decode_matches_libwebp_across_content_size_and_quality() {
    // Sizes cover MB-aligned, non-aligned (odd width/height → chroma edges), and
    // 1×1; qualities span aggressive (intra-4×4, strong filter) to lossless-ish.
    let sizes = [
        (1u32, 1u32),
        (5, 9),
        (16, 16),
        (17, 13),
        (32, 24),
        (48, 33),
        (64, 64),
    ];
    let qualities = [8.0f32, 40.0, 75.0, 95.0, 100.0];
    let contents: [(&str, ContentGen); 3] = [
        ("gradient", gradient_rgb),
        ("noise", noise_rgb),
        ("checker", checker_rgb),
    ];
    for &(w, h) in &sizes {
        for &q in &qualities {
            for (name, generate) in contents {
                let rgb = generate(w, h);
                let webp = libwebp_encode_lossy(&rgb, w, h, q);
                let payload = vp8_payload(&webp);
                let (rw, rh, reference) = libwebp_decode_rgba(&webp);
                assert_eq!((rw, rh), (w, h));
                let ours = webpkit::lossy::decode(&payload)
                    .unwrap_or_else(|e| panic!("{name} {w}x{h} q{q}: decode failed: {e:?}"));
                assert_eq!((ours.width(), ours.height()), (w, h));
                let diffs = ours
                    .as_bytes()
                    .iter()
                    .zip(&reference)
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(
                    diffs,
                    0,
                    "{name} {w}x{h} q{q}: {diffs}/{} RGBA bytes differ from libwebp",
                    reference.len()
                );
            }
        }
    }
}

/// A two-region field (gradient top half, pseudo-noise bottom half) so the
/// advanced encoder is nudged into using distinct macroblock segments.
fn mixed_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for y in 0..height {
        for x in 0..width {
            if y < height / 2 {
                let g = u8::try_from((x * 255) / width).unwrap_or(0);
                out.extend_from_slice(&[g, g / 2, 255 - g]);
            } else {
                let mut s = x
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(y.wrapping_mul(40_503))
                    .wrapping_add(0x9e37_79b9);
                s ^= s >> 13;
                s = s.wrapping_mul(0x85eb_ca6b);
                s ^= s >> 16;
                out.push(u8::try_from(s & 0xff).unwrap_or(0));
                out.push(u8::try_from((s >> 8) & 0xff).unwrap_or(0));
                out.push(u8::try_from((s >> 16) & 0xff).unwrap_or(0));
            }
        }
    }
    out
}

/// Encode `rgb` (`width*height*3`) as a lossy VP8 WebP with the ADVANCED encoder,
/// exposing knobs the simple API hides: `segments` (1..=4), `filter_type`
/// (0 simple / 1 strong → the decoder's "normal" filter), `filter_strength`
/// (0..=100), `filter_sharpness` (0..=7). Reaches the decoder's segmentation /
/// loop-filter-delta / normal-filter paths that `WebPEncodeRGB` never emits.
#[expect(
    clippy::too_many_arguments,
    reason = "each argument is a distinct libwebp WebPConfig knob under test \
              (segments / filter_type / strength / sharpness); bundling them into \
              a struct would only obscure the encoder feature matrix"
)]
fn libwebp_encode_advanced(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: f32,
    segments: i32,
    filter_type: i32,
    filter_strength: i32,
    filter_sharpness: i32,
    partitions: i32,
) -> Vec<u8> {
    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = 4;
    config.segments = segments;
    config.filter_type = filter_type;
    config.filter_strength = filter_strength;
    config.filter_sharpness = filter_sharpness;
    config.autofilter = 0;
    // 0..=3 selects 1/2/4/8 token partitions (exercises the streaming decoder's
    // per-partition suspend/resume; ignored by the one-shot decode's result).
    config.partitions = partitions;
    // SAFETY: `config` is a fully-initialized WebPConfig.
    assert!(
        unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0,
        "invalid encoder config"
    );

    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 0;
    picture.width = i32::try_from(width).unwrap();
    picture.height = i32::try_from(height).unwrap();
    let stride = i32::try_from(width * 3).unwrap();
    // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGB(&raw mut picture, rgb.as_ptr(), stride) } != 0,
        "WebPPictureImportRGB failed"
    );

    let mut writer = std::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
    // SAFETY: `WebPMemoryWriterInit` initializes the whole struct in place.
    unsafe { libwebp_sys::WebPMemoryWriterInit(writer.as_mut_ptr()) };
    let mut writer = unsafe { writer.assume_init() };
    picture.writer = Some(libwebp_sys::WebPMemoryWrite);
    picture.custom_ptr = (&raw mut writer).cast();

    // SAFETY: `config`/`picture` are fully set up; the writer callback appends
    // the stream to `writer` (which outlives this call).
    let ok = unsafe { libwebp_sys::WebPEncode(&raw const config, &raw mut picture) };
    assert!(
        ok != 0 && picture.error_code == libwebp_sys::WebPEncodingError::VP8_ENC_OK,
        "advanced encode failed: {:?}",
        picture.error_code
    );
    // SAFETY: on success `writer.mem` points at `writer.size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(writer.mem, writer.size) }.to_vec();
    // SAFETY: free the writer buffer and the picture's planes exactly once.
    unsafe { libwebp_sys::WebPMemoryWriterClear(&raw mut writer) };
    unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
    bytes
}

#[test]
fn decode_matches_libwebp_with_advanced_encoder_features() {
    // Streams the simple WebPEncodeRGB path cannot produce: multiple segments
    // and the strong ("normal") loop filter with explicit strength/sharpness.
    // Both Level B (RGBA) and Level A (YUV) must stay byte-exact vs libwebp —
    // validating the segmentation / loop-filter-delta / normal-filter decode
    // paths on real streams.
    let (w, h) = (96u32, 64u32);
    let rgb = mixed_rgb(w, h);
    // (segments, filter_type, filter_strength, filter_sharpness)
    let configs = [(4, 1, 60, 4), (3, 1, 30, 0), (2, 0, 20, 2), (4, 1, 0, 0)];
    for (segs, ftype, fstr, fsharp) in configs {
        for &q in &[20.0f32, 55.0, 90.0] {
            let webp = libwebp_encode_advanced(&rgb, w, h, q, segs, ftype, fstr, fsharp, 0);
            let payload = vp8_payload(&webp);
            let label = format!("segs={segs} ft={ftype} fs={fstr} sh={fsharp} q={q}");

            let (rw, rh, reference) = libwebp_decode_rgba(&webp);
            let ours = webpkit::lossy::decode(&payload).expect("webpkit::lossy decode");
            assert_eq!((ours.width(), ours.height()), (rw, rh), "{label}: dims");
            assert_eq!(
                ours.as_bytes(),
                reference.as_slice(),
                "{label}: RGBA differs"
            );

            let (_, _, oy, ou, ov) = webpkit::lossy::__reconstruct_yuv(&payload).unwrap();
            let (_, _, ly, lu, lv) = libwebp_decode_yuv(&webp);
            assert_eq!((oy, ou, ov), (ly, lu, lv), "{label}: YUV planes differ");
        }
    }
}

/// Push `webp` to a fresh [`webpkit::lossy::IncrementalDecoder`] in `chunk`-byte slices,
/// draining finalized rows as they arrive. Returns the final `into_image` plus the
/// concatenated drained rows; asserts drained rows are contiguous from row 0.
fn stream_in_chunks(webp: &[u8], chunk: usize) -> (webpkit::lossy::Image, Vec<u8>) {
    let mut dec = webpkit::lossy::IncrementalDecoder::new();
    let mut drained: Vec<u8> = Vec::new();
    let mut next_row = 0u32;
    let mut off = 0;
    while off < webp.len() {
        let end = (off + chunk).min(webp.len());
        dec.push(&webp[off..end]).unwrap();
        if let Some(rows) = dec.drain_rows() {
            assert_eq!(rows.first_row, next_row, "drained rows are not contiguous");
            next_row += rows.rows;
            drained.extend_from_slice(rows.as_bytes());
        }
        off = end;
    }
    let image = dec.into_image().unwrap();
    (image, drained)
}

#[test]
fn streamed_decode_matches_one_shot_and_libwebp() {
    // The committed fixtures are single-partition; here we sweep 1/2/4/8 token
    // partitions and all three filter types, streaming through the public
    // IncrementalDecoder at several chunk granularities (incl. one byte at a
    // time — the suspend/resume worst case). Streamed pixels must equal both the
    // one-shot webpkit::lossy::decode and libwebp's WebPDecodeRGBA, and drained rows
    // must reassemble to the same image.
    let (w, h) = (80u32, 48u32);
    let rgb = mixed_rgb(w, h);
    // (segments, encoder filter_type, filter_strength, filter_sharpness). The
    // encoder's filter_type is 0=simple / 1=strong; with strength 0 the bitstream
    // carries no filter, so these three exercise the decoder's none/simple/normal.
    let filters = [(1, 1, 40, 0), (4, 0, 30, 3), (2, 1, 0, 0)];
    for (segs, ftype, fstr, fsharp) in filters {
        for partitions in 0..=3 {
            for &q in &[30.0f32, 80.0] {
                let webp =
                    libwebp_encode_advanced(&rgb, w, h, q, segs, ftype, fstr, fsharp, partitions);
                let payload = vp8_payload(&webp);
                let label = format!("ft={ftype} parts={} q={q}", 1i32 << partitions);

                let one_shot = webpkit::lossy::decode(&payload).expect("one-shot decode");
                let (_, _, reference) = libwebp_decode_rgba(&webp);
                assert_eq!(
                    one_shot.as_bytes(),
                    reference.as_slice(),
                    "{label}: one-shot vs libwebp"
                );

                // All chunk sizes are < webp.len(), so the true row-streaming path
                // is exercised (a single whole-file push would short-circuit to the
                // one-shot decode with no drainable rows, which the check above
                // already covers).
                for &chunk in &[1usize, 3, 17, 64] {
                    let (image, drained) = stream_in_chunks(&webp, chunk);
                    assert_eq!(
                        image.as_bytes(),
                        one_shot.as_bytes(),
                        "{label} chunk={chunk}: streamed into_image differs"
                    );
                    assert_eq!(
                        drained,
                        one_shot.as_bytes(),
                        "{label} chunk={chunk}: drained rows differ"
                    );
                }
            }
        }
    }
}

#[test]
fn decoder_matches_libwebp_on_skip_heavy_frames() {
    // Encode flat content with a low-effort libwebp method (`VP8EncLoop`, method
    // ≤ 2): nearly every macroblock has an all-zero residual, so libwebp turns on
    // per-macroblock skip (use_skip) and emits NO residual tokens for those blocks.
    // This exercises the skip decode path: a decoder that always parsed residuals
    // would desync on such a stream (method-4 streams, which every other oracle
    // test uses, never skip, so they do not exercise it). Our decode
    // (one-shot AND streamed through the incremental decoder) must equal libwebp's
    // WebPDecodeRGBA byte for byte.
    let (w, h) = (64u32, 64u32);
    let mut saw_skip = false;
    for &q in &[5.0f32, 20.0, 50.0] {
        let webp = libwebp_encode_method(&solid_rgb(w, h), w, h, q, 1);
        let payload = vp8_payload(&webp);
        // A method-1 flat encode uses skip; whether a given `q` does or not, the
        // decode must match libwebp — and the aggregate `saw_skip` pins that at
        // least one of these payloads truly exercises the skip path.
        let uses_skip =
            webpkit::lossy::__frame_uses_skip(&payload).expect("skip-heavy payload should parse");
        saw_skip |= uses_skip;

        let (rw, rh, reference) = libwebp_decode_rgba(&webp);
        assert_eq!((rw, rh), (w, h));

        let one_shot = webpkit::lossy::decode(&payload).expect("webpkit::lossy decode");
        assert_eq!((one_shot.width(), one_shot.height()), (w, h));
        assert_eq!(
            one_shot.as_bytes(),
            reference.as_slice(),
            "q{q}: one-shot RGBA differs from libwebp"
        );

        // The skip decode must also survive suspend/resume at every granularity.
        for &chunk in &[1usize, 3, 17, 64] {
            let (image, drained) = stream_in_chunks(&webp, chunk);
            assert_eq!(
                image.as_bytes(),
                one_shot.as_bytes(),
                "q{q} chunk={chunk}: streamed differs from one-shot"
            );
            assert_eq!(
                drained,
                one_shot.as_bytes(),
                "q{q} chunk={chunk}: drained rows differ"
            );
        }
    }
    assert!(
        saw_skip,
        "the skip test must exercise a real use_skip stream"
    );
}

#[test]
fn reconstructed_yuv_matches_libwebp_level_a() {
    // Level A: our reconstructed Y/U/V planes equal libwebp's WebPDecodeYUV
    // byte-for-byte, isolating reconstruction (boolean decode + intra prediction
    // + IDCT + loop filter) from the YUV→RGB conversion. VP8 reconstruction is
    // bit-exact per RFC 6386, so any mismatch here is a reconstruction bug, not a
    // color-conversion rounding difference.
    let sizes = [
        (1u32, 1u32),
        (5, 9),
        (16, 16),
        (17, 13),
        (32, 24),
        (48, 33),
        (64, 64),
    ];
    let qualities = [8.0f32, 40.0, 75.0, 95.0, 100.0];
    let contents: [(&str, ContentGen); 3] = [
        ("gradient", gradient_rgb),
        ("noise", noise_rgb),
        ("checker", checker_rgb),
    ];
    for &(w, h) in &sizes {
        for &q in &qualities {
            for (name, generate) in contents {
                let webp = libwebp_encode_lossy(&generate(w, h), w, h, q);
                let payload = vp8_payload(&webp);
                let (ow, oh, oy, ou, ov) = webpkit::lossy::__reconstruct_yuv(&payload)
                    .unwrap_or_else(|| panic!("{name} {w}x{h} q{q}: reconstruct failed"));
                let (lw, lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
                assert_eq!((ow, oh), (lw, lh), "{name} {w}x{h} q{q}: dims");
                assert_eq!(oy, ly, "{name} {w}x{h} q{q}: Y plane differs");
                assert_eq!(ou, lu, "{name} {w}x{h} q{q}: U plane differs");
                assert_eq!(ov, lv, "{name} {w}x{h} q{q}: V plane differs");
            }
        }
    }
}

#[test]
fn public_decode_yuv_matches_libwebp_level_a() {
    // The same Level-A cross-check as above, but through the crate facade
    // `webpkit::decode_yuv` on the whole WebP file (not the raw payload via the
    // hidden `__reconstruct_yuv` hook): the public `YuvImage` planes must equal
    // libwebp's `WebPDecodeYUV` byte-for-byte, so the exposed API carries the same
    // bit-exact reconstruction. Odd sides exercise the ceil-halved chroma shape.
    let sizes = [(1u32, 1u32), (5, 9), (17, 13), (48, 33), (64, 64)];
    let qualities = [8.0f32, 55.0, 100.0];
    let contents: [(&str, ContentGen); 3] = [
        ("gradient", gradient_rgb),
        ("noise", noise_rgb),
        ("checker", checker_rgb),
    ];
    for &(w, h) in &sizes {
        for &q in &qualities {
            for (name, generate) in contents {
                let webp = libwebp_encode_lossy(&generate(w, h), w, h, q);
                let yuv = webpkit::decode_yuv(&webp)
                    .unwrap_or_else(|_| panic!("{name} {w}x{h} q{q}: decode_yuv failed"));
                let (lw, lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
                assert_eq!(
                    (yuv.width(), yuv.height()),
                    (lw, lh),
                    "{name} {w}x{h} q{q}: dims"
                );
                assert_eq!(
                    (yuv.chroma_width(), yuv.chroma_height()),
                    (lw.div_ceil(2), lh.div_ceil(2)),
                    "{name} {w}x{h} q{q}: chroma dims"
                );
                assert_eq!(yuv.y(), ly, "{name} {w}x{h} q{q}: Y plane differs");
                assert_eq!(yuv.u(), lu, "{name} {w}x{h} q{q}: U plane differs");
                assert_eq!(yuv.v(), lv, "{name} {w}x{h} q{q}: V plane differs");
            }
        }
    }
}

#[test]
fn harness_encodes_classifies_and_round_trips_a_lossy_stream() {
    let (width, height) = (32u32, 24u32);
    let rgb = gradient_rgb(width, height);
    let webp = libwebp_encode_lossy(&rgb, width, height, 75.0);

    // Our container layer classifies libwebp's output as a lossy VP8 image.
    let payload = vp8_payload(&webp);
    assert!(!payload.is_empty());

    // Our key-frame header parser agrees on the dimensions of a real stream.
    let dims = webpkit::lossy::peek_dimensions(&payload).unwrap();
    assert_eq!((dims.width(), dims.height()), (width, height));

    // Level B: our decoded RGBA equals libwebp's WebPDecodeRGBA byte-for-byte.
    let px = usize::try_from(width).unwrap() * usize::try_from(height).unwrap();
    let (rw, rh, reference) = libwebp_decode_rgba(&webp);
    assert_eq!((rw, rh), (width, height));
    assert_eq!(reference.len(), px * 4);

    let ours =
        webpkit::lossy::decode(&payload).expect("webpkit::lossy should decode a VP8 key frame");
    assert_eq!((ours.width(), ours.height()), (width, height));
    let diffs = ours
        .as_bytes()
        .iter()
        .zip(&reference)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        diffs,
        0,
        "decoded RGBA differs from libwebp in {diffs}/{} bytes",
        reference.len()
    );

    let (yw, yh, y, u, v) = libwebp_decode_yuv(&webp);
    assert_eq!((yw, yh), (width, height));
    assert_eq!(y.len(), px);
    let chroma =
        usize::try_from(width.div_ceil(2)).unwrap() * usize::try_from(height.div_ceil(2)).unwrap();
    assert_eq!(u.len(), chroma);
    assert_eq!(v.len(), chroma);
}

proptest! {
    // Each case encodes a random small frame with libwebp and cross-checks our
    // decode; cap the count to keep the differential brisk.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Over random frames at random quality, our decoded RGBA equals libwebp's
    /// `WebPDecodeRGBA` byte-for-byte — the fixed matrix's randomized companion,
    /// reaching encoder outputs the enumerated cases miss.
    #[test]
    fn decode_matches_libwebp_over_random_frames(
        (w, h, rgb) in arbitrary_lossy_rgb(),
        quality in 1.0f32..=100.0,
    ) {
        let webp = libwebp_encode_lossy(&rgb, w, h, quality);
        let payload = vp8_payload(&webp);
        let (rw, rh, reference) = libwebp_decode_rgba(&webp);
        let ours = webpkit::lossy::decode(&payload).unwrap();
        prop_assert_eq!((ours.width(), ours.height()), (rw, rh));
        prop_assert_eq!(ours.as_bytes(), reference.as_slice());
    }
}

// ---- encoder oracle: our-encode -> libwebp-decode -------------------------

/// Expand interleaved RGB (`w*h*3`) to opaque RGBA (`w*h*4`) for our encoder.
fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
    }
    out
}

/// PSNR (dB) of a decoded RGBA buffer against the source RGB, over RGB channels.
fn psnr_rgba_vs_rgb(rgba: &[u8], rgb: &[u8]) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (dec, src) in rgba.chunks_exact(4).zip(rgb.chunks_exact(3)) {
        for c in 0..3 {
            let d = f64::from(dec[c]) - f64::from(src[c]);
            se = d.mul_add(d, se);
            n += 1.0;
        }
    }
    if se < 1.0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / (se / n)).log10()
}

/// Our lossy encoder's output is a valid VP8 stream that libwebp reads at the
/// right size and — because VP8 reconstruction is bit-exact per RFC 6386 —
/// reconstructs to **exactly** our own reconstructed YUV planes (Level A), while
/// reproducing the source within a quality floor. Chained with the in-crate
/// self-consistency test (our decode == our reconstruction), this proves our
/// encoded bitstream decodes identically in our decoder and in libwebp.
#[test]
fn our_encode_is_read_bit_exactly_by_libwebp() {
    let sizes = [(16u32, 16u32), (17, 13), (32, 24), (48, 33)];
    let qualities = [30u8, 75, 95];
    let contents: [(&str, ContentGen); 3] = [
        ("gradient", gradient_rgb),
        ("noise", noise_rgb),
        ("checker", checker_rgb),
    ];
    for &(w, h) in &sizes {
        for &q in &qualities {
            for (name, generate) in contents {
                let rgb = generate(w, h);
                let rgba = rgb_to_rgba(&rgb);
                let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
                let img =
                    webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba)
                        .unwrap();
                let cfg = webpkit::lossy::LossyConfig::new().with_quality(q);
                let webp = webpkit::lossy::encode(img, &cfg).unwrap();
                let payload = vp8_payload(&webp);

                // Validity: libwebp reads our output at the right dimensions.
                let (rw, rh, dec_rgba) = libwebp_decode_rgba(&webp);
                assert_eq!((rw, rh), (w, h), "{name} {w}x{h} q{q}: libwebp dims");

                // Level A: libwebp reconstructs our stream to exactly our own
                // reconstruction, plane for plane.
                let (_ow, _oh, oy, ou, ov) = webpkit::lossy::__reconstruct_yuv(&payload)
                    .unwrap_or_else(|| panic!("{name} {w}x{h} q{q}: our reconstruct failed"));
                let (_lw, _lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
                assert_eq!(oy, ly, "{name} {w}x{h} q{q}: luma differs from libwebp");
                assert_eq!(ou, lu, "{name} {w}x{h} q{q}: U differs from libwebp");
                assert_eq!(ov, lv, "{name} {w}x{h} q{q}: V differs from libwebp");

                // Fidelity floor against the source: a "not garbage" guard.
                // Random noise is near-incompressible (DC-only prediction plus
                // 4:2:0 subsampling can't hold it, even at high quality), so it
                // gets a loose floor; compressible content gets a quality-scaled
                // one. Competitive PSNR is an LE8 goal — the real validation above
                // is the byte-exact Level A match.
                let psnr = psnr_rgba_vs_rgb(&dec_rgba, &rgb);
                let floor = if name == "noise" {
                    9.0
                } else if q >= 95 {
                    26.0
                } else if q >= 75 {
                    18.0
                } else {
                    12.0
                };
                assert!(
                    psnr >= floor,
                    "{name} {w}x{h} q{q}: PSNR {psnr:.2} < {floor}"
                );
            }
        }
    }
}

/// Our lossy encoder's extended `VP8X + ALPH + VP8 ` container — carrying a
/// LOSSLESS alpha plane — is read by libwebp's `WebPDecodeRGBA`, and the decoded
/// alpha channel equals the source byte-for-byte. This proves the container framing
/// and the `ALPH` chunk (its 1-byte header, spatial filter and green-lane VP8L
/// payload) are standard-valid, not merely self-consistent with our own decoder.
#[test]
fn our_alpha_container_is_read_byte_exactly_by_libwebp() {
    // Sizes span MB-aligned and odd (chroma-edge) cases; each builds a diagonal
    // alpha ramp with fully-transparent and fully-opaque corners.
    let sizes = [(16u32, 16u32), (17, 13), (32, 24)];
    for &(w, h) in &sizes {
        let mut rgba = Vec::with_capacity(usize::try_from(w * h * 4).unwrap());
        let mut source_alpha = Vec::with_capacity(usize::try_from(w * h).unwrap());
        for y in 0..h {
            for x in 0..w {
                let a = if x < 3 && y < 3 {
                    0
                } else if x + 3 >= w && y + 3 >= h {
                    255
                } else {
                    u8::try_from(((x + y) * 255) / (w + h - 2)).unwrap_or(255)
                };
                source_alpha.push(a);
                let r = u8::try_from((x * 255) / w).unwrap_or(0);
                let g = u8::try_from((y * 255) / h).unwrap_or(0);
                rgba.extend_from_slice(&[r, g, 100, a]);
            }
        }
        let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
        let img =
            webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
        let cfg = webpkit::lossy::LossyConfig::new().with_quality(90);
        let webp = webpkit::lossy::encode(img, &cfg).unwrap();

        // Standard-valid to libwebp: it reads the extended container at the right
        // size and recovers the alpha lane exactly (alpha is lossless).
        let (rw, rh, dec_rgba) = libwebp_decode_rgba(&webp);
        assert_eq!((rw, rh), (w, h), "{w}x{h}: libwebp dims");
        let dec_alpha: Vec<u8> = dec_rgba.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(
            dec_alpha, source_alpha,
            "{w}x{h}: libwebp alpha not byte-exact"
        );
    }
}

/// Encode `rgb` (`width*height*3`) as a lossy VP8 WebP at libwebp `method`
/// (`0..=6`). Only the low-effort methods (`VP8EncLoop`, method ≤ 2) turn on the
/// per-macroblock skip probability; method 4 (what `WebPEncodeRGB` and the
/// advanced helper use) never does, which is why every other oracle stream leaves
/// the decoder's skip path untouched. This helper reaches that path.
fn libwebp_encode_method(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: f32,
    method: i32,
) -> Vec<u8> {
    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = method;
    // SAFETY: `config` is a fully-initialized WebPConfig.
    assert!(
        unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0,
        "invalid encoder config"
    );

    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 0;
    picture.width = i32::try_from(width).unwrap();
    picture.height = i32::try_from(height).unwrap();
    let stride = i32::try_from(width * 3).unwrap();
    // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGB(&raw mut picture, rgb.as_ptr(), stride) } != 0,
        "WebPPictureImportRGB failed"
    );

    let mut writer = std::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
    // SAFETY: `WebPMemoryWriterInit` initializes the whole struct in place.
    unsafe { libwebp_sys::WebPMemoryWriterInit(writer.as_mut_ptr()) };
    let mut writer = unsafe { writer.assume_init() };
    picture.writer = Some(libwebp_sys::WebPMemoryWrite);
    picture.custom_ptr = (&raw mut writer).cast();

    // SAFETY: `config`/`picture` are fully set up; the writer callback appends the
    // stream to `writer` (which outlives this call).
    let ok = unsafe { libwebp_sys::WebPEncode(&raw const config, &raw mut picture) };
    assert!(
        ok != 0 && picture.error_code == libwebp_sys::WebPEncodingError::VP8_ENC_OK,
        "method-{method} encode failed: {:?}",
        picture.error_code
    );
    // SAFETY: on success `writer.mem` points at `writer.size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(writer.mem, writer.size) }.to_vec();
    // SAFETY: free the writer buffer and the picture's planes exactly once.
    unsafe { libwebp_sys::WebPMemoryWriterClear(&raw mut writer) };
    unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
    bytes
}

#[test]
fn our_skip_encode_is_read_bit_exactly_by_libwebp() {
    // Our Balanced encoder codes a flat image with per-macroblock skip (no residual
    // tokens for the skippable blocks). libwebp must read that skip-using stream and
    // reconstruct it to exactly our own YUV planes (Level A) — proving our skip
    // context evolution (`NzContext::skip_mb`) matches libwebp's decoder, the
    // encoder-side counterpart of `decoder_matches_libwebp_on_skip_heavy_frames`.
    let (w, h) = (64u32, 64u32);
    let rgba = rgb_to_rgba(&solid_rgb(w, h));
    let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
    let img =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(75)
        .with_effort(webpkit::lossy::Effort::level(4));
    let webp = webpkit::lossy::encode(img, &cfg).unwrap();
    let payload = vp8_payload(&webp);

    // Non-vacuity: our encoder really codes skip on this frame.
    assert_eq!(
        webpkit::lossy::__frame_uses_skip(&payload),
        Some(true),
        "our Balanced encoder should code per-macroblock skip on a flat image"
    );

    // Validity + Level A: libwebp reads our stream at the right size and
    // reconstructs it to exactly our own reconstruction, plane for plane.
    let (rw, rh, _dec_rgba) = libwebp_decode_rgba(&webp);
    assert_eq!((rw, rh), (w, h), "libwebp dims");
    let (_ow, _oh, oy, ou, ov) =
        webpkit::lossy::__reconstruct_yuv(&payload).expect("our reconstruct failed");
    let (_lw, _lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
    assert_eq!(oy, ly, "luma differs from libwebp");
    assert_eq!(ou, lu, "U differs from libwebp");
    assert_eq!(ov, lv, "V differs from libwebp");
}

#[test]
fn our_i4x4_encode_is_read_bit_exactly_by_libwebp() {
    // Our Best encoder codes detailed vertical stripes with several intra-4×4
    // (`B_PRED`) macroblocks. libwebp must read that i4x4-using stream and
    // reconstruct it to exactly our own YUV planes (Level A) — proving our i4x4 mode
    // emission (the `kBModesProba` top/left context threading) and no-Y2 (`first=0`,
    // `ac_type=3`) token path match libwebp's decoder, the encoder-side counterpart
    // of the decoder's i4x4 oracle coverage.
    let (w, h) = (64u32, 64u32);
    let rgb = stripe_rgb(w, h);
    let rgba = rgb_to_rgba(&rgb);
    let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
    let img =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(50)
        .with_effort(webpkit::lossy::Effort::level(9));
    let webp = webpkit::lossy::encode(img, &cfg).unwrap();
    let payload = vp8_payload(&webp);

    // Non-vacuity: our encoder really codes intra-4×4 on this frame.
    assert_eq!(
        webpkit::lossy::__frame_uses_i4x4(&payload),
        Some(true),
        "our Best encoder should code intra-4×4 on detailed content"
    );

    // Validity + Level A: libwebp reads our stream at the right size and reconstructs
    // it to exactly our own reconstruction, plane for plane.
    let (rw, rh, _dec_rgba) = libwebp_decode_rgba(&webp);
    assert_eq!((rw, rh), (w, h), "libwebp dims");
    let (_ow, _oh, oy, ou, ov) =
        webpkit::lossy::__reconstruct_yuv(&payload).expect("our reconstruct failed");
    let (_lw, _lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
    assert_eq!(oy, ly, "luma differs from libwebp");
    assert_eq!(ou, lu, "U differs from libwebp");
    assert_eq!(ov, lv, "V differs from libwebp");
}

#[test]
fn our_filtered_encode_is_read_bit_exactly_by_libwebp() {
    // Our Balanced encoder deblocks a blocky image with the in-loop filter (a
    // frame-final pass) and codes the matching filter header. libwebp must read that
    // filtered stream and reconstruct it to exactly our own filtered YUV planes
    // (Level A) — proving the per-macroblock filter strengths we apply match the ones
    // libwebp re-derives from our header, the encoder-side counterpart of the
    // decoder's normal-filter oracle coverage.
    let (w, h) = (48u32, 48u32);
    let rgba = rgb_to_rgba(&checker_rgb(w, h));
    let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
    let img =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(50)
        .with_effort(webpkit::lossy::Effort::level(4));
    let webp = webpkit::lossy::encode(img, &cfg).unwrap();
    let payload = vp8_payload(&webp);

    // Non-vacuity: our encoder really codes a non-zero deblocking filter here.
    assert!(
        webpkit::lossy::__frame_filter_level(&payload).is_some_and(|level| level > 0),
        "our Balanced encoder should code a non-zero loop-filter level on blocky content"
    );

    // Validity + Level A: libwebp reads our stream at the right size and reconstructs
    // it to exactly our own (filtered) reconstruction, plane for plane.
    let (rw, rh, _dec_rgba) = libwebp_decode_rgba(&webp);
    assert_eq!((rw, rh), (w, h), "libwebp dims");
    let (_ow, _oh, oy, ou, ov) =
        webpkit::lossy::__reconstruct_yuv(&payload).expect("our reconstruct failed");
    let (_lw, _lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
    assert_eq!(oy, ly, "luma differs from libwebp");
    assert_eq!(ou, lu, "U differs from libwebp");
    assert_eq!(ov, lv, "V differs from libwebp");
}

#[test]
fn our_segmented_encode_is_read_bit_exactly_by_libwebp() {
    // Our Balanced encoder partitions mixed flat-vs-noise content into multiple
    // quantizer segments (integer k-means on per-macroblock luma AC energy). libwebp
    // must read that segmented stream and reconstruct it to exactly our own YUV planes
    // (Level A) — proving our segment header, per-macroblock segment-id map and
    // per-segment quantizers are the exact bit-inverse of libwebp's decoder, the
    // encoder-side counterpart of the decoder's advanced-segmentation oracle coverage.
    let (w, h) = (96u32, 64u32);
    let rgb = mixed_rgb(w, h);
    let rgba = rgb_to_rgba(&rgb);
    let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
    let img =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(60)
        .with_effort(webpkit::lossy::Effort::level(4));
    let webp = webpkit::lossy::encode(img, &cfg).unwrap();
    let payload = vp8_payload(&webp);

    // Non-vacuity: our encoder really codes more than one segment here.
    let count =
        webpkit::lossy::__frame_segment_count(&payload).expect("segmented payload should parse");
    assert!(
        count >= 2,
        "our Balanced encoder should use multiple segments, got {count}"
    );

    // Validity + Level A: libwebp reads our stream at the right size and reconstructs
    // it to exactly our own reconstruction, plane for plane.
    let (rw, rh, _dec_rgba) = libwebp_decode_rgba(&webp);
    assert_eq!((rw, rh), (w, h), "libwebp dims");
    let (_ow, _oh, oy, ou, ov) =
        webpkit::lossy::__reconstruct_yuv(&payload).expect("our reconstruct failed");
    let (_lw, _lh, ly, lu, lv) = libwebp_decode_yuv(&webp);
    assert_eq!(oy, ly, "luma differs from libwebp");
    assert_eq!(ou, lu, "U differs from libwebp");
    assert_eq!(ov, lv, "V differs from libwebp");
}

/// A live libwebp demuxer, deleted exactly once when dropped. Same guard pattern
/// as the reference decoders above: even if an assertion unwinds mid-use, `Drop`
/// still runs `WebPDemuxDelete` exactly once, so the demuxer can neither leak nor
/// double-free. Mirrors the `lossless` codec's metadata demux oracle.
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
    // A borrowed view of `webp`; the demuxer reads these bytes until it is deleted,
    // and `webp` outlives this call.
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

/// `encode_image`'s metadata-bearing lossy output is valid to libwebp's demuxer and
/// its metadata chunks survive byte-exact. The Preserve case proves the whole
/// `VP8X`+`ICCP`+`VP8 `+`EXIF`+`XMP ` file the writer emits is well-formed to the
/// reference demuxer/decoder; the `StripPrivate` case proves the privacy strip
/// actually removes the Exif/XMP chunks while keeping ICC. The ICC body is an odd
/// length, so this also checks the RIFF pad byte is not counted in the chunk size.
/// Mirrors the `lossless` codec's `encode_image_metadata_survives_libwebp`.
#[test]
fn encode_image_metadata_survives_libwebp_demux() {
    // A 16x16 opaque RGB gradient (no ALPH, so the file is VP8X+ICCP+VP8+EXIF+XMP).
    let (w, h) = (16u32, 16u32);
    let rgb = gradient_rgb(w, h);
    let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    let dims = webpkit::lossy::Dimensions::new(w, h).unwrap();
    let icc = b"icc-lossy".to_vec(); // 9 bytes: odd -> RIFF pad
    let exif = b"exif-lossy".to_vec();
    let xmp = b"<x:xmpmeta/>".to_vec();
    let img = webpkit::lossy::Image::new(dims, webpkit::lossy::PixelLayout::Rgba8, rgba)
        .unwrap()
        .with_metadata(
            webpkit::lossy::Metadata::none()
                .with_icc_profile(icc.clone())
                .with_exif(exif.clone())
                .with_xmp(xmp.clone()),
        );

    // Preserve (default): the reference demuxer reproduces the canvas and every
    // metadata chunk survives byte-exact; libwebp decodes the pixels at the right
    // size (a lossy decode, so only dimensions are pinned).
    let cfg = webpkit::lossy::LossyConfig::new().with_quality(90);
    let file = webpkit::lossy::encode_image(&img, &cfg).unwrap();
    let (lw, lh, _rgba) = libwebp_decode_rgba(&file);
    assert_eq!((lw, lh), (w, h), "libwebp disagreed on canvas dimensions");
    assert_eq!(libwebp_demux_chunk(&file, b"ICCP\0"), Some(icc.clone()));
    assert_eq!(libwebp_demux_chunk(&file, b"EXIF\0"), Some(exif));
    assert_eq!(libwebp_demux_chunk(&file, b"XMP \0"), Some(xmp));

    // StripPrivate: ICC survives, Exif and XMP are gone.
    let stripped = webpkit::lossy::encode_image(
        &img,
        &webpkit::lossy::LossyConfig::new()
            .with_quality(90)
            .with_metadata_policy(webpkit::lossy::MetadataPolicy::StripPrivate),
    )
    .unwrap();
    assert_eq!(libwebp_demux_chunk(&stripped, b"ICCP\0"), Some(icc));
    assert_eq!(libwebp_demux_chunk(&stripped, b"EXIF\0"), None);
    assert_eq!(libwebp_demux_chunk(&stripped, b"XMP \0"), None);
}

// ---- sharp-YUV tolerance oracle -------------------------------------------

/// A saturated vertical color split (`left` on the left half, `right` on the right):
/// the canonical case the plain 4:2:0 box downsample handles poorly, since a chroma
/// sample straddling the seam averages two opposite hues and the decoder then bleeds
/// that muddy value back across the edge.
fn split_rgb(width: u32, height: u32, left: [u8; 3], right: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for _y in 0..height {
        for x in 0..width {
            let px = if x < width / 2 { left } else { right };
            out.extend_from_slice(&px);
        }
    }
    out
}

/// A coarse two-hue checkerboard (`block`-pixel cells) — chroma edges in both axes.
fn checker2_rgb(width: u32, height: u32, a: [u8; 3], b: [u8; 3], block: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(width * height * 3).unwrap());
    for y in 0..height {
        for x in 0..width {
            let px = if ((x / block) + (y / block)).is_multiple_of(2) {
                a
            } else {
                b
            };
            out.extend_from_slice(&px);
        }
    }
    out
}

/// JPEG full-range chroma (Cb, Cr) of an 8-bit RGB triple (BT.601 primaries).
fn rgb_to_chroma(r: f64, g: f64, b: f64) -> (f64, f64) {
    let cb = 0.5f64.mul_add(
        b,
        (-0.331_264f64).mul_add(g, (-0.168_736f64).mul_add(r, 128.0)),
    );
    let cr = (-0.081_312f64).mul_add(b, (-0.418_688f64).mul_add(g, 0.5f64.mul_add(r, 128.0)));
    (cb, cr)
}

/// Chroma-only PSNR (dB) of a decoded RGBA buffer against the source RGB, measured
/// over the (Cb, Cr) plane so it scores the color-subsampling fidelity in isolation
/// from luma. Returns `99.0` for an essentially exact match.
fn chroma_psnr_rgba_vs_rgb(rgba: &[u8], rgb: &[u8]) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (dec, src) in rgba.chunks_exact(4).zip(rgb.chunks_exact(3)) {
        let (dcb, dcr) = rgb_to_chroma(f64::from(dec[0]), f64::from(dec[1]), f64::from(dec[2]));
        let (scb, scr) = rgb_to_chroma(f64::from(src[0]), f64::from(src[1]), f64::from(src[2]));
        let db = dcb - scb;
        let dr = dcr - scr;
        se = db.mul_add(db, se);
        se = dr.mul_add(dr, se);
        n += 2.0;
    }
    if se < 1.0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / (se / n)).log10()
}

/// Encode `rgb` (`width*height*3`) with the libwebp ADVANCED encoder and
/// `use_sharp_yuv = 1` — the reference luminance-guided (`libsharpyuv`) chroma path
/// this subsystem ports. Method/quality mirror our own encode so the chroma metric
/// compares the two sharp conversions, not the surrounding encoder settings.
fn libwebp_encode_sharp_yuv(rgb: &[u8], width: u32, height: u32, quality: f32) -> Vec<u8> {
    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = 4;
    config.use_sharp_yuv = 1;
    // SAFETY: `config` is a fully-initialized WebPConfig.
    assert!(
        unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0,
        "invalid encoder config"
    );

    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 0;
    picture.width = i32::try_from(width).unwrap();
    picture.height = i32::try_from(height).unwrap();
    let stride = i32::try_from(width * 3).unwrap();
    // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGB(&raw mut picture, rgb.as_ptr(), stride) } != 0,
        "WebPPictureImportRGB failed"
    );

    let mut writer = std::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
    // SAFETY: `WebPMemoryWriterInit` initializes the whole struct in place.
    unsafe { libwebp_sys::WebPMemoryWriterInit(writer.as_mut_ptr()) };
    let mut writer = unsafe { writer.assume_init() };
    picture.writer = Some(libwebp_sys::WebPMemoryWrite);
    picture.custom_ptr = (&raw mut writer).cast();

    // SAFETY: `config`/`picture` are fully set up; the writer appends to `writer`.
    let ok = unsafe { libwebp_sys::WebPEncode(&raw const config, &raw mut picture) };
    assert!(
        ok != 0 && picture.error_code == libwebp_sys::WebPEncodingError::VP8_ENC_OK,
        "sharp encode failed: {:?}",
        picture.error_code
    );
    // SAFETY: on success `writer.mem` points at `writer.size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(writer.mem, writer.size) }.to_vec();
    // SAFETY: free the writer buffer and the picture's planes exactly once.
    unsafe { libwebp_sys::WebPMemoryWriterClear(&raw mut writer) };
    unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
    bytes
}

/// Encode `rgb` with our lossy encoder at `quality`, toggling sharp-YUV via the public
/// [`webpkit::lossy::LossyTuning`], and return the decoded RGBA (via libwebp, which
/// reconstructs our stream identically to our own decoder — proven by the Level-A
/// tests above), so all three streams are scored through one decoder.
fn ours_decode_rgba(rgb: &[u8], width: u32, height: u32, quality: u8, sharp: bool) -> Vec<u8> {
    let rgba = rgb_to_rgba(rgb);
    let dims = webpkit::lossy::Dimensions::new(width, height).unwrap();
    let img =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(quality)
        .with_tuning(webpkit::lossy::LossyTuning::new().with_sharp_yuv(sharp));
    let webp = webpkit::lossy::encode(img, &cfg).unwrap();
    let (_w, _h, rgba) = libwebp_decode_rgba(&webp);
    rgba
}

/// TOLERANCE oracle for the sharp-YUV subsystem. On saturated-edge content our
/// fixed-point luminance-guided chroma must (1) beat our own plain 2×2 box path in
/// chroma PSNR — the whole point of the subsystem — and (2) land within a tolerance
/// band of libwebp's float `libsharpyuv` at the same quality. Byte-exactness is not
/// expected (fixed-point gamma reformulation); only chroma parity.
#[allow(
    clippy::print_stderr,
    reason = "an oracle diagnostic: emit the measured per-case chroma PSNR (ours-box / \
              ours-sharp / cwebp-sharp) so a tolerance regression is legible in the log"
)]
#[test]
fn sharp_yuv_chroma_is_within_tolerance_of_cwebp_and_beats_box() {
    // Improvement must be real, not noise; parity band absorbs the encoder + gamma
    // reformulation gap between our integer path and libwebp's float one.
    const MIN_IMPROVEMENT_DB: f64 = 0.5;
    const PARITY_BAND_DB: f64 = 3.0;

    let (w, h) = (64u32, 48u32);
    let cases: [(&str, Vec<u8>); 4] = [
        ("red|blue", split_rgb(w, h, [230, 20, 20], [20, 20, 230])),
        ("red|green", split_rgb(w, h, [230, 20, 20], [20, 220, 20])),
        (
            "magenta|cyan",
            split_rgb(w, h, [235, 20, 235], [20, 235, 235]),
        ),
        (
            "checker",
            checker2_rgb(w, h, [235, 20, 60], [20, 120, 235], 8),
        ),
    ];
    let quality = 90u8;
    let mut total_gain = 0.0f64;
    for (name, rgb) in &cases {
        let box_rgba = ours_decode_rgba(rgb, w, h, quality, false);
        let sharp_rgba = ours_decode_rgba(rgb, w, h, quality, true);
        let cwebp_rgba = {
            let webp = libwebp_encode_sharp_yuv(rgb, w, h, f32::from(quality));
            let (_w, _h, rgba) = libwebp_decode_rgba(&webp);
            rgba
        };

        let box_psnr = chroma_psnr_rgba_vs_rgb(&box_rgba, rgb);
        let sharp_psnr = chroma_psnr_rgba_vs_rgb(&sharp_rgba, rgb);
        let cwebp_psnr = chroma_psnr_rgba_vs_rgb(&cwebp_rgba, rgb);
        eprintln!(
            "sharp_yuv[{name}]: box={box_psnr:.2}dB ours-sharp={sharp_psnr:.2}dB \
             cwebp-sharp={cwebp_psnr:.2}dB (gain {:+.2}, vs-cwebp {:+.2})",
            sharp_psnr - box_psnr,
            sharp_psnr - cwebp_psnr,
        );

        assert!(
            sharp_psnr >= box_psnr + MIN_IMPROVEMENT_DB,
            "{name}: sharp chroma PSNR {sharp_psnr:.2} must beat box {box_psnr:.2} by \
             >= {MIN_IMPROVEMENT_DB} dB on a saturated edge",
        );
        assert!(
            sharp_psnr >= cwebp_psnr - PARITY_BAND_DB,
            "{name}: sharp chroma PSNR {sharp_psnr:.2} outside the parity band of \
             cwebp -sharp_yuv {cwebp_psnr:.2} (band {PARITY_BAND_DB} dB)",
        );
        total_gain += sharp_psnr - box_psnr;
    }
    let case_count = f64::from(u8::try_from(cases.len()).unwrap());
    assert!(
        total_gain / case_count >= MIN_IMPROVEMENT_DB,
        "mean chroma gain over the box path must exceed {MIN_IMPROVEMENT_DB} dB",
    );
}
