//! Differential oracle for the umbrella `webp` decode path, focused on the `ALPH`
//! (lossy transparent) feature: cross-check `webpkit::decode` against the libwebp C
//! reference, in-process.
//!
//! Enabled only with `--features oracle` (which links `libwebp-sys` and the
//! vendored reference library); never part of a normal build.
//!
//! The lossy RGB of a `VP8 ` image is already proven byte-exact against libwebp by
//! the `lossy` codec's own oracle. What is new here is the sibling `ALPH` alpha plane:
//! we encode synthetic RGBA (with an alpha channel that varies *independently* of
//! the RGB, so a wrong green-channel extraction cannot pass silently) through the
//! libwebp ADVANCED encoder, sweeping `alpha_compression ∈ {raw, lossless}` and
//! `alpha_filtering ∈ {none, fast, best}` across MB-aligned / odd / tiny sizes,
//! then assert `webpkit::decode(...).as_bytes()` equals `WebPDecodeRGBA` byte-for-byte.
//! A completeness check confirms the matrix actually exercised both compression
//! methods and more than one spatial un-filter.
#![cfg(feature = "oracle")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only differential oracle: unwrap/panic are the accepted style for \
              provably-infallible conversions and reference-library successes, and the \
              synthetic-pixel generator truncates to u8 on purpose"
)]

use webpkit::alpha::{AlphaCompression, AlphaFilter, parse_header};
use webpkit::container::reader::locate_image_with_alpha;

/// Encode `rgba` (`width * height * 4` bytes) as a lossy VP8 WebP that carries its
/// alpha in an `ALPH` chunk, via the libwebp ADVANCED encoder so the alpha knobs
/// are under test: `alpha_compression` (0 raw / 1 lossless), `alpha_filtering`
/// (0 none / 1 fast / 2 best). `alpha_quality` is pinned to 100 so the alpha is
/// stored exactly (no lossy quantization / dithering), isolating the decode path.
fn libwebp_encode_lossy_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: f32,
    alpha_compression: i32,
    alpha_filtering: i32,
) -> Vec<u8> {
    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = 4;
    config.alpha_compression = alpha_compression;
    config.alpha_filtering = alpha_filtering;
    config.alpha_quality = 100;
    // SAFETY: `config` is a fully-initialized WebPConfig.
    assert!(
        unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0,
        "invalid encoder config"
    );

    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 0; // lossy: import converts RGBA -> YUVA
    picture.width = i32::try_from(width).unwrap();
    picture.height = i32::try_from(height).unwrap();
    let stride = i32::try_from(width * 4).unwrap();
    // SAFETY: `rgba` holds `width*height*4` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGBA(&raw mut picture, rgba.as_ptr(), stride) } != 0,
        "WebPPictureImportRGBA failed"
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

/// Deterministic synthetic RGBA (`width * height * 4`) whose alpha channel is an
/// independent function of position (never derivable from the RGB), so a wrong
/// alpha/green extraction cannot pass. `pattern` selects an alpha shape that steers
/// the encoder toward a different spatial filter.
fn synth_rgba(width: u32, height: u32, pattern: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // RGB: a smooth diagonal gradient plus a coarse block, unrelated to A.
            let r = ((x * 5).wrapping_add(y * 3)) as u8;
            let g = ((x * 3) ^ (y * 7)) as u8;
            let b = (x.wrapping_add(y).wrapping_mul(11)) as u8;
            let a = match pattern {
                0 => ((x * 255) / width.max(1)) as u8, // horizontal ramp -> horizontal filter
                1 => ((y * 255) / height.max(1)) as u8, // vertical ramp -> vertical filter
                2 => (((x + y) * 9) & 0xff) as u8,     // diagonal gradient
                3 => {
                    if (x / 2 + y / 2) % 2 == 0 {
                        0x20
                    } else {
                        0xF0
                    }
                }, // 2x2 checker -> none/gradient
                _ => 0xC0u8.wrapping_add((x.wrapping_mul(y)) as u8), // busy
            };
            out.push(r);
            out.push(g);
            out.push(b);
            out.push(a);
        }
    }
    out
}

#[test]
fn decode_matches_libwebp_on_transparent_lossy_images() {
    // (width, height): MB-aligned, odd, single-column/row, tiny.
    let sizes = [(16u32, 16u32), (15, 17), (1, 1), (33, 3), (7, 20), (2, 2)];
    let mut seen_compression: [bool; 2] = [false, false];
    let mut seen_filter: [bool; 4] = [false, false, false, false];
    let mut checked = 0u32;

    for (i, &(w, h)) in sizes.iter().enumerate() {
        for alpha_compression in [0i32, 1] {
            for alpha_filtering in [0i32, 1, 2] {
                let pattern = (i as u32 + alpha_filtering as u32) % 5;
                let rgba = synth_rgba(w, h, pattern);
                let file = libwebp_encode_lossy_rgba(
                    &rgba,
                    w,
                    h,
                    90.0,
                    alpha_compression,
                    alpha_filtering,
                );

                // The file must be a transparent lossy image with a sibling ALPH.
                let located = locate_image_with_alpha(&file).unwrap();
                let alph = located.alpha.unwrap_or_else(|| {
                    panic!(
                        "no ALPH chunk for {w}x{h} comp={alpha_compression} filt={alpha_filtering}"
                    )
                });
                let (header, _) = parse_header(alph).unwrap();
                seen_compression[match header.compression {
                    AlphaCompression::None => 0,
                    AlphaCompression::Lossless => 1,
                }] = true;
                seen_filter[header.filter as usize] = true;

                // Golden: libwebp's own decode. Ours must be byte-identical.
                let (gw, gh, golden) = libwebp_decode_rgba(&file);
                let ours = webpkit::decode(&file).unwrap();
                assert_eq!((ours.width(), ours.height()), (gw, gh), "dims {w}x{h}");
                assert_eq!(
                    ours.as_bytes(),
                    golden.as_slice(),
                    "RGBA mismatch at {w}x{h} comp={alpha_compression} filt={alpha_filtering} pat={pattern}"
                );
                // Sanity: the alpha we produced is genuinely non-trivial for at least
                // some cases (guards against an all-opaque false pass).
                assert!(ours.has_alpha() || w * h == 1, "expected alpha at {w}x{h}");
                checked += 1;
            }
        }
    }

    assert!(checked >= 36, "expected a full matrix, ran {checked}");
    assert!(
        seen_compression[0] && seen_compression[1],
        "matrix must exercise both raw and lossless alpha (saw {seen_compression:?})"
    );
    let filters_hit = seen_filter.iter().filter(|&&b| b).count();
    assert!(
        filters_hit >= 2,
        "matrix must exercise more than one spatial un-filter (saw {seen_filter:?})"
    );
}

#[test]
fn incremental_decodes_lossy_still_like_one_shot() {
    // An opaque lossy image encodes to a bare `VP8 ` (no `ALPH`), which the umbrella
    // row-streams through webpkit::lossy. Streaming it in tiny pushes must reproduce both
    // the one-shot `webpkit::decode` and libwebp's `WebPDecodeRGBA`.
    for &(w, h) in &[(32u32, 24u32), (17, 13), (5, 9)] {
        let mut rgba = synth_rgba(w, h, 2);
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 0xff; // fully opaque -> libwebp emits a bare VP8, no ALPH
        }
        let file = libwebp_encode_lossy_rgba(&rgba, w, h, 80.0, 0, 0);
        assert!(
            locate_image_with_alpha(&file).unwrap().alpha.is_none(),
            "expected an opaque bare VP8 for {w}x{h}"
        );

        let (_, _, golden) = libwebp_decode_rgba(&file);
        assert_eq!(
            webpkit::decode(&file).unwrap().as_bytes(),
            golden.as_slice(),
            "{w}x{h}: one-shot vs libwebp"
        );
        for chunk in [1usize, 5, file.len().max(1)] {
            let mut dec = webpkit::IncrementalDecoder::new();
            for slice in file.chunks(chunk) {
                dec.push(slice).unwrap();
            }
            assert_eq!(
                dec.into_image().unwrap().as_bytes(),
                golden.as_slice(),
                "{w}x{h} chunk={chunk}: streamed into_image differs"
            );
        }
    }

    // On a larger frame (whose payload is buffered well before EOF, so the still
    // stream is set up rather than short-circuited), the umbrella must drain rows
    // that reassemble to the libwebp golden.
    let mut rgba = synth_rgba(48, 40, 2);
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    let file = libwebp_encode_lossy_rgba(&rgba, 48, 40, 80.0, 0, 0);
    let (_, _, golden) = libwebp_decode_rgba(&file);
    let mut dec = webpkit::IncrementalDecoder::new();
    let mut drained = Vec::new();
    let mut next = 0u32;
    for slice in file.chunks(1) {
        dec.push(slice).unwrap();
        if let Some(rows) = dec.drain_rows() {
            assert_eq!(rows.first_row, next, "umbrella drain not contiguous");
            next += rows.rows;
            drained.extend_from_slice(rows.as_bytes());
        }
    }
    assert_eq!(
        drained,
        golden.as_slice(),
        "umbrella-drained lossy-still rows differ from the libwebp golden"
    );
}

/// [`AlphaFilter`] must map its discriminants to the byte offsets libwebp uses, so
/// the `seen_filter` indexing above is meaningful.
#[test]
fn alpha_filter_discriminants_are_stable() {
    assert_eq!(AlphaFilter::None as usize, 0);
    assert_eq!(AlphaFilter::Horizontal as usize, 1);
    assert_eq!(AlphaFilter::Vertical as usize, 2);
    assert_eq!(AlphaFilter::Gradient as usize, 3);
}

// ---------------------------------------------------------------------------
// Lossy animation differential
//
// Build a real animated-LOSSY WebP with libwebp's mux `WebPAnimEncoder`, then
// assert our `decode_frames().composited()` equals libwebp's demux
// `WebPAnimDecoder` frame-for-frame. The compositor is `webpkit::lossless`'s (already proven
// byte-exact vs `WebPAnimDecoder`); what is new here is routing each animated
// `VP8 `(+`ALPH`) frame through `webpkit::lossy` via the injected hook.
// ---------------------------------------------------------------------------

/// Encode `frames` (each `width*height*4` RGBA bytes) as an animated **lossy**
/// WebP via libwebp's mux `WebPAnimEncoder` (40 ms/frame, loop forever).
fn libwebp_encode_anim_lossy(
    frames: &[Vec<u8>],
    width: u32,
    height: u32,
    quality: f32,
    alpha_compression: i32,
    alpha_filtering: i32,
) -> Vec<u8> {
    let abi = libwebp_sys::WEBP_MUX_ABI_VERSION as i32;
    let mut opts: libwebp_sys::WebPAnimEncoderOptions = unsafe { core::mem::zeroed() };
    // SAFETY: `opts` is a live writable value; `InitInternal` fills it in.
    assert!(
        unsafe { libwebp_sys::WebPAnimEncoderOptionsInitInternal(&raw mut opts, abi) } != 0,
        "WebPAnimEncoderOptionsInitInternal failed"
    );
    opts.anim_params.loop_count = 0;

    // SAFETY: `opts` is initialized; the encoder is created for the canvas size.
    let enc = unsafe {
        libwebp_sys::WebPAnimEncoderNewInternal(width as i32, height as i32, &raw const opts, abi)
    };
    assert!(!enc.is_null(), "WebPAnimEncoderNewInternal failed");

    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = 4;
    config.alpha_compression = alpha_compression;
    config.alpha_filtering = alpha_filtering;
    config.alpha_quality = 100;
    // SAFETY: `config` is fully initialized.
    assert!(
        unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0,
        "invalid encoder config"
    );

    let mut timestamp = 0i32;
    for frame in frames {
        let mut picture = libwebp_sys::WebPPicture::new().unwrap();
        picture.use_argb = 1; // the anim encoder diffs frames in ARGB
        picture.width = width as i32;
        picture.height = height as i32;
        let stride = (width * 4) as i32;
        // SAFETY: `frame` holds `width*height*4` bytes at `stride`; dims match.
        assert!(
            unsafe { libwebp_sys::WebPPictureImportRGBA(&raw mut picture, frame.as_ptr(), stride) }
                != 0,
            "WebPPictureImportRGBA failed"
        );
        // SAFETY: `enc` is live; `picture` is a valid ARGB frame; `config` is valid.
        let ok = unsafe {
            libwebp_sys::WebPAnimEncoderAdd(enc, &raw mut picture, timestamp, &raw const config)
        };
        // SAFETY: `picture`'s argb buffer is freed exactly once here.
        unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
        assert!(ok != 0, "WebPAnimEncoderAdd failed");
        timestamp += 40;
    }
    // A null frame finalizes the stream.
    // SAFETY: `enc` is live; a null frame/config signals end of input.
    assert!(
        unsafe {
            libwebp_sys::WebPAnimEncoderAdd(
                enc,
                core::ptr::null_mut(),
                timestamp,
                core::ptr::null(),
            )
        } != 0,
        "WebPAnimEncoderAdd (flush) failed"
    );

    let mut data: libwebp_sys::WebPData = unsafe { core::mem::zeroed() };
    // SAFETY: `enc` is live; `data` receives an owned, freshly allocated buffer.
    assert!(
        unsafe { libwebp_sys::WebPAnimEncoderAssemble(enc, &raw mut data) } != 0,
        "WebPAnimEncoderAssemble failed"
    );
    // SAFETY: on success `data.bytes` points at `data.size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data.bytes, data.size) }.to_vec();
    // SAFETY: free the assembled data and the encoder exactly once.
    unsafe { libwebp_sys::WebPDataClear(&mut data) };
    unsafe { libwebp_sys::WebPAnimEncoderDelete(enc) };
    bytes
}

/// A live libwebp demux `WebPAnimDecoder`, deleted exactly once when dropped
/// (so an assertion that unwinds mid-decode cannot leak or double-free it).
struct AnimDecoder(*mut libwebp_sys::WebPAnimDecoder);

impl Drop for AnimDecoder {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null decoder from `WebPAnimDecoderNew`, deleted
        // exactly once here and never used afterwards.
        unsafe { libwebp_sys::WebPAnimDecoderDelete(self.0) };
    }
}

/// Composite an animated `webp` with libwebp's `WebPAnimDecoder` into
/// `(canvas_w, canvas_h, per-frame RGBA)` — each buffer `canvas_w*canvas_h*4`.
fn libwebp_anim_composite(webp: &[u8]) -> (u32, u32, Vec<Vec<u8>>) {
    let data = libwebp_sys::WebPData {
        bytes: webp.as_ptr(),
        size: webp.len(),
    };
    // SAFETY: `WebPAnimDecoderOptions` is plain-old-data; all-zero is a valid
    // instance for `WebPAnimDecoderOptionsInit` to fill in.
    let mut options: libwebp_sys::WebPAnimDecoderOptions = unsafe { core::mem::zeroed() };
    // SAFETY: `options` is a live writable value.
    assert!(
        unsafe { libwebp_sys::WebPAnimDecoderOptionsInit(&raw mut options) } != 0,
        "WebPAnimDecoderOptionsInit failed"
    );
    options.color_mode = libwebp_sys::WEBP_CSP_MODE::MODE_RGBA;

    // SAFETY: `data` describes the valid `webp` buffer (alive for the whole call);
    // `options` is initialized.
    let dec = unsafe { libwebp_sys::WebPAnimDecoderNew(&raw const data, &raw const options) };
    assert!(!dec.is_null(), "WebPAnimDecoderNew failed");
    let _guard = AnimDecoder(dec);

    let mut info = libwebp_sys::WebPAnimInfo::default();
    // SAFETY: `dec` is live; `info` is a writable out-parameter.
    assert!(
        unsafe { libwebp_sys::WebPAnimDecoderGetInfo(dec, &raw mut info) } != 0,
        "WebPAnimDecoderGetInfo failed"
    );
    let (canvas_w, canvas_h) = (info.canvas_width, info.canvas_height);
    let frame_len = usize::try_from(canvas_w).unwrap() * usize::try_from(canvas_h).unwrap() * 4;

    let mut frames = Vec::new();
    // SAFETY: `dec` is live; `HasMoreFrames` only reads decoder state.
    while unsafe { libwebp_sys::WebPAnimDecoderHasMoreFrames(dec) } > 0 {
        let mut buf: *mut u8 = core::ptr::null_mut();
        let mut timestamp: core::ffi::c_int = 0;
        // SAFETY: `dec` is live; libwebp writes its internal canvas-sized RGBA
        // buffer address into `buf` and the frame timestamp into `timestamp`.
        let got =
            unsafe { libwebp_sys::WebPAnimDecoderGetNext(dec, &raw mut buf, &raw mut timestamp) };
        assert!(got != 0 && !buf.is_null(), "WebPAnimDecoderGetNext failed");
        // SAFETY: `buf` points at `frame_len` valid, already-composited RGBA bytes
        // owned by the decoder (valid until the next call); copy them out now.
        let frame = unsafe { std::slice::from_raw_parts(buf, frame_len) }.to_vec();
        frames.push(frame);
    }
    (canvas_w, canvas_h, frames)
}

/// Composite an animated `webp` with our umbrella decoder into the same shape.
fn webp_anim_composite(webp: &[u8]) -> (u32, u32, Vec<Vec<u8>>) {
    let frames = webpkit::decode_frames(webp).unwrap();
    let canvas = frames.anim_info().canvas;
    let mut out = Vec::new();
    for frame in frames.composited() {
        out.push(frame.unwrap().image().as_bytes().to_vec());
    }
    (canvas.width(), canvas.height(), out)
}

/// One animation frame: synthetic RGBA, made visually distinct per `index` (so
/// the encoder never collapses identical frames into a still), optionally forced
/// fully opaque.
fn anim_frame(width: u32, height: u32, index: u32, opaque: bool) -> Vec<u8> {
    let mut frame = synth_rgba(width, height, index % 5);
    for px in frame.chunks_exact_mut(4) {
        px[0] = px[0].wrapping_add((index * 40) as u8);
        px[1] = px[1].wrapping_add((index * 17) as u8);
        px[2] = px[2].wrapping_add((index * 91) as u8);
        if opaque {
            px[3] = 0xff;
        }
    }
    frame
}

#[test]
fn decode_anim_matches_libwebp_on_lossy() {
    let mut checked = 0u32;
    for &(w, h) in &[(16u32, 16u32), (15, 9), (24, 20)] {
        for &opaque in &[true, false] {
            let frames: Vec<Vec<u8>> = (0..3).map(|i| anim_frame(w, h, i, opaque)).collect();
            let file = libwebp_encode_anim_lossy(&frames, w, h, 90.0, 1, 1);

            assert!(
                webpkit::container::reader::is_animated(&file).unwrap(),
                "encoder did not produce an animation for {w}x{h}"
            );
            let (lw, lh, golden) = libwebp_anim_composite(&file);
            let (ow, oh, ours) = webp_anim_composite(&file);
            assert_eq!((ow, oh), (lw, lh), "canvas {w}x{h}");
            assert_eq!(
                ours.len(),
                golden.len(),
                "frame count {w}x{h} opaque={opaque}"
            );
            for (i, (a, b)) in ours.iter().zip(&golden).enumerate() {
                assert_eq!(
                    a, b,
                    "composited frame {i} disagrees at {w}x{h} opaque={opaque}"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 6,
        "expected the full animation matrix, ran {checked}"
    );
}

#[test]
fn incremental_decodes_lossy_anim_like_one_shot() {
    let (w, h) = (24u32, 18u32);
    let frames: Vec<Vec<u8>> = (0..4).map(|i| anim_frame(w, h, i, true)).collect();
    let file = libwebp_encode_anim_lossy(&frames, w, h, 90.0, 1, 1);
    let (_, _, one_shot) = webp_anim_composite(&file);

    // Buffer the whole file, then drive one composited frame per empty push,
    // collecting the pre-disposal canvas after each `FrameComplete`.
    let mut decoder = webpkit::incremental_decoder();
    decoder.push(&file).unwrap();
    let mut streamed: Vec<Vec<u8>> = Vec::new();
    for _ in 0..frames.len() + 2 {
        match decoder.push(&[]).unwrap() {
            webpkit::Progress::FrameComplete(_) => {
                streamed.push(decoder.frame_image().unwrap().as_bytes().to_vec());
            },
            webpkit::Progress::Finished => break,
            _ => {},
        }
    }
    assert_eq!(
        streamed, one_shot,
        "streamed lossy-animation frames differ from the one-shot composite"
    );
}

#[test]
fn encode_lossy_anim_matches_libwebp_demux() {
    // The encoder direction: our AnimationEncoder's lossy output must be
    // libwebp-readable and composite bit-for-bit identically. Both sides consume
    // the identical VP8/ALPH bitstreams we wrote, and each half is independently
    // proven byte-exact (lossy RGBA vs WebPDecodeRGBA; our compositor vs
    // WebPAnimDecoder), so the two composites must agree exactly.
    let mut checked = 0u32;
    for &(w, h) in &[(16u32, 16u32), (15, 9), (24, 20)] {
        for &opaque in &[true, false] {
            let canvas = webpkit::Dimensions::new(w, h).unwrap();
            let frames: Vec<Vec<u8>> = (0..3).map(|i| anim_frame(w, h, i, opaque)).collect();
            let frame_meta = webpkit::FrameMeta::new(
                0,
                0,
                canvas,
                40,
                webpkit::BlendMode::Blend,
                webpkit::DisposalMode::Keep,
            );

            let img0 =
                webpkit::ImageRef::new(canvas, webpkit::PixelLayout::Rgba8, &frames[0]).unwrap();
            let mut enc = webpkit::AnimationEncoder::new(canvas)
                .codec(webpkit::AnimCodec::Lossy(90))
                .loop_count(0)
                .add_frame(img0, frame_meta)
                .unwrap();
            for f in &frames[1..] {
                let img = webpkit::ImageRef::new(canvas, webpkit::PixelLayout::Rgba8, f).unwrap();
                enc = enc.add_frame(img, frame_meta).unwrap();
            }
            let file = enc.finish();

            assert!(
                webpkit::is_animated(&file).unwrap(),
                "our encoder did not produce an animation for {w}x{h}"
            );
            let (lw, lh, golden) = libwebp_anim_composite(&file);
            let (ow, oh, ours) = webp_anim_composite(&file);
            assert_eq!((ow, oh), (lw, lh), "canvas {w}x{h}");
            assert_eq!(
                ours.len(),
                golden.len(),
                "frame count {w}x{h} opaque={opaque}"
            );
            for (i, (a, b)) in ours.iter().zip(&golden).enumerate() {
                assert_eq!(
                    a, b,
                    "our lossy-anim frame {i} diverges from libwebp demux at {w}x{h} opaque={opaque}"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 6,
        "expected the full encode-anim matrix, ran {checked}"
    );
}
