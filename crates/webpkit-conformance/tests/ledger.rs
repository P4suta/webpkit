//! Drift gates for the committed `conformance-results-alpha.json` (still) and
//! `conformance-results-anim.json` (animated) ledgers, plus the `#[ignore]` +
//! `oracle`-gated generators that (re)produce the fixtures and the ledgers from
//! libwebp.
//!
//! The default build recomputes a [`CaseResult`] for every
//! `fixtures/alpha/<case>/` and an [`AnimCaseResult`] for every
//! `fixtures/anim/<case>/` (decode-only, tool-free — it never links libwebp),
//! serializes each with the crate's [`results_to_json`] / [`anim_results_to_json`],
//! and asserts the bytes equal the committed ledger at the crate root. This pins
//! the machine-readable conformance records so they cannot silently drift from
//! what the decoder does.
//!
//! The fixtures and ledgers are supplied by the integrator: run the generators
//! in the `oracle`-gated `generate` module (`--features oracle -- --ignored`) to
//! encode synthetic RGBA through the libwebp ADVANCED / mux `WebPAnimEncoder`,
//! read back its RGBA golden, and commit the results. Until a ledger exists its
//! gate skips with a note rather than failing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use webpkit_conformance::{
    AnimCaseResult, CaseResult, anim_results_to_json, load_anim_meta, load_meta, results_to_json,
};

/// Recompute the alpha decode ledger from the committed fixtures, visiting cases
/// in sorted order so the serialized bytes are deterministic.
///
/// A case is any `fixtures/alpha/<case>/` holding `input.webp`, `expected.rgba`,
/// and `meta.toml`; `passed` records whether [`webpkit::decode`] reproduced the
/// golden byte-for-byte. This is intentionally tool-free (no libwebp link), so
/// the gate stays reproducible on CI.
fn compute_results(alpha_dir: &Path) -> Result<Vec<CaseResult>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(alpha_dir)
        .with_context(|| format!("reading {}", alpha_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut results = Vec::with_capacity(dirs.len());
    for case_dir in dirs {
        let input = case_dir.join("input.webp");
        let golden = case_dir.join("expected.rgba");
        let meta_path = case_dir.join("meta.toml");
        if !input.exists() || !golden.exists() || !meta_path.exists() {
            continue;
        }
        let meta = load_meta(&meta_path)?;
        let case = case_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_owned();
        let payload =
            std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
        let expected =
            std::fs::read(&golden).with_context(|| format!("reading {}", golden.display()))?;
        let passed =
            webpkit::decode(&payload).is_ok_and(|image| image.as_bytes() == expected.as_slice());
        results.push(CaseResult {
            case,
            alpha_compression: meta.alpha_compression,
            alpha_filtering: meta.alpha_filtering,
            passed,
        });
    }
    Ok(results)
}

/// Decode an animated-lossy `payload` and concatenate every composited frame's
/// canvas-sized RGBA in frame order — the exact shape of a case's `frames.rgba`.
/// Returns `None` if the file fails to decode or a frame fails to composite, so
/// the caller records a `passed = false` rather than aborting the whole ledger.
fn decode_anim_concat(payload: &[u8]) -> Option<Vec<u8>> {
    let frames = webpkit::decode_frames(payload).ok()?;
    let mut out = Vec::new();
    for frame in frames.composited() {
        out.extend_from_slice(frame.ok()?.image().as_bytes());
    }
    Some(out)
}

/// Recompute the animated-lossy decode ledger from the committed fixtures,
/// visiting cases in sorted order so the serialized bytes are deterministic.
///
/// A case is any `fixtures/anim/<case>/` holding `input.webp`, `frames.rgba`, and
/// `meta.toml`; `passed` records whether `webpkit::decode_frames(...).composited()`
/// reproduced the concatenated golden byte-for-byte. Tool-free (no libwebp link),
/// so the gate stays reproducible on CI.
fn compute_anim_results(anim_dir: &Path) -> Result<Vec<AnimCaseResult>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(anim_dir)
        .with_context(|| format!("reading {}", anim_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut results = Vec::with_capacity(dirs.len());
    for case_dir in dirs {
        let input = case_dir.join("input.webp");
        let golden = case_dir.join("frames.rgba");
        let meta_path = case_dir.join("meta.toml");
        if !input.exists() || !golden.exists() || !meta_path.exists() {
            continue;
        }
        let meta = load_anim_meta(&meta_path)?;
        let case = case_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_owned();
        let payload =
            std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
        let expected =
            std::fs::read(&golden).with_context(|| format!("reading {}", golden.display()))?;
        let passed = decode_anim_concat(&payload).is_some_and(|actual| actual == expected);
        results.push(AnimCaseResult {
            case,
            frame_count: meta.frame_count,
            passed,
        });
    }
    Ok(results)
}

#[test]
fn committed_ledger_is_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger_path = root.join("conformance-results-alpha.json");

    let committed = match std::fs::read_to_string(&ledger_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping: no ledger at {} (the integrator commits it from the oracle generators)",
                ledger_path.display()
            );
            return;
        },
        Err(e) => panic!("reading {}: {e}", ledger_path.display()),
    };

    let alpha_dir = root.join("fixtures/alpha");
    let results = compute_results(&alpha_dir).expect("recompute conformance ledger");
    // Honesty gate: a ledger must never certify a FAILING decode.
    for r in &results {
        assert!(
            r.passed,
            "conformance case `{}` failed to decode; a ledger cannot certify it",
            r.case
        );
    }
    let fresh = results_to_json(&results).expect("serialize conformance ledger");

    assert_eq!(
        fresh,
        committed,
        "conformance-results-alpha.json at {} has drifted from a fresh decode run. \
         Regenerate it from the oracle generators and commit the updated file.",
        ledger_path.display()
    );
}

#[test]
fn committed_anim_ledger_is_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger_path = root.join("conformance-results-anim.json");

    let committed = match std::fs::read_to_string(&ledger_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping: no ledger at {} (the integrator commits it from the oracle generators)",
                ledger_path.display()
            );
            return;
        },
        Err(e) => panic!("reading {}: {e}", ledger_path.display()),
    };

    let anim_dir = root.join("fixtures/anim");
    let results = compute_anim_results(&anim_dir).expect("recompute anim conformance ledger");
    // Honesty gate: a ledger must never certify a FAILING decode.
    for r in &results {
        assert!(
            r.passed,
            "anim conformance case `{}` failed to decode; a ledger cannot certify it",
            r.case
        );
    }
    let fresh = anim_results_to_json(&results).expect("serialize anim conformance ledger");

    assert_eq!(
        fresh,
        committed,
        "conformance-results-anim.json at {} has drifted from a fresh decode run. \
         Regenerate it from the oracle generators and commit the updated file.",
        ledger_path.display()
    );
}

/// Fixture and ledger generators. Enabled only with `--features oracle` (which
/// links libwebp via `libwebp-sys`); never part of a normal build or the default
/// CI gate.
///
/// The FFI helpers (`libwebp_encode_lossy_rgba`, `libwebp_decode_rgba`,
/// `synth_rgba` for the still path; `libwebp_encode_anim_lossy`, the `AnimDecoder`
/// guard, `libwebp_anim_composite`, and `anim_frame` for the animated path) are
/// copied verbatim from `crates/webpkit/tests/oracle.rs`, so the fixtures this crate
/// replays are produced through the exact same encode/decode reference path the
/// differential oracle validates.
#[cfg(feature = "oracle")]
#[expect(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "fixture generator: unwrap is the accepted style for provably-infallible \
              conversions and reference-library successes, and the synthetic-pixel generator \
              truncates to u8 on purpose"
)]
mod generate {
    use std::path::Path;

    use webpkit_conformance::{AlphaCompression, AnimMeta, Meta};

    /// Encode `rgba` (`width * height * 4` bytes) as a lossy VP8 WebP that carries
    /// its alpha in an `ALPH` chunk, via the libwebp ADVANCED encoder so the alpha
    /// knobs are under test: `alpha_compression` (0 raw / 1 lossless),
    /// `alpha_filtering` (0 none / 1 fast / 2 best). `alpha_quality` is pinned to
    /// 100 so the alpha is stored exactly (no lossy quantization / dithering),
    /// isolating the decode path.
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
            unsafe { libwebp_sys::WebPPictureImportRGBA(&raw mut picture, rgba.as_ptr(), stride) }
                != 0,
            "WebPPictureImportRGBA failed"
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

    /// Decode `webp` with libwebp into `(width, height, rgba)`.
    fn libwebp_decode_rgba(webp: &[u8]) -> (u32, u32, Vec<u8>) {
        let (mut w, mut h) = (0i32, 0i32);
        // SAFETY: `webp`/`len` describe a valid buffer; libwebp allocates the
        // output and writes the dimensions through the out-pointers.
        let ptr = unsafe {
            libwebp_sys::WebPDecodeRGBA(webp.as_ptr(), webp.len(), &raw mut w, &raw mut h)
        };
        assert!(!ptr.is_null(), "libwebp RGBA decode failed");
        let len = usize::try_from(w).unwrap() * usize::try_from(h).unwrap() * 4;
        // SAFETY: libwebp returned `w * h * 4` valid RGBA bytes at `ptr`.
        let rgba = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        // SAFETY: `ptr` was allocated by libwebp and is freed exactly once here.
        unsafe { libwebp_sys::WebPFree(ptr.cast()) };
        (u32::try_from(w).unwrap(), u32::try_from(h).unwrap(), rgba)
    }

    /// Deterministic synthetic RGBA (`width * height * 4`) whose alpha channel is
    /// an independent function of position (never derivable from the RGB), so a
    /// wrong alpha/green extraction cannot pass. `pattern` selects an alpha shape
    /// that steers the encoder toward a different spatial filter.
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

    /// Regenerate the committed `ALPH` fixtures. For each case in a small matrix
    /// — sizes {16x16, 15x17, 5x9} × `alpha_compression` {none, lossless} ×
    /// `alpha_filtering` {0, 1} — it encodes synthetic RGBA through the libwebp
    /// ADVANCED encoder (the `input.webp`), reads its RGBA back with libwebp (the
    /// `expected.rgba` golden), and writes a `meta.toml`. Run explicitly:
    /// `cargo test -p webpkit-conformance --features oracle -- --ignored gen_fixtures`.
    #[test]
    #[ignore = "regenerates committed fixtures via libwebp; run explicitly"]
    fn gen_fixtures() {
        let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/alpha");
        let sizes = [(16u32, 16u32), (15, 17), (5, 9)];
        let compressions = [
            (0i32, AlphaCompression::None),
            (1i32, AlphaCompression::Lossless),
        ];
        let filterings = [0i32, 1];
        let quality = 90.0f32;

        for (i, &(w, h)) in sizes.iter().enumerate() {
            for &(comp_knob, comp) in &compressions {
                for &filt in &filterings {
                    let pattern = (i as u32 + filt as u32) % 5;
                    let rgba = synth_rgba(w, h, pattern);
                    let file = libwebp_encode_lossy_rgba(&rgba, w, h, quality, comp_knob, filt);
                    let (gw, gh, golden) = libwebp_decode_rgba(&file);
                    assert_eq!(
                        (gw, gh),
                        (w, h),
                        "libwebp round-trip changed dims for {w}x{h}"
                    );

                    let comp_label = match comp {
                        AlphaCompression::None => "none",
                        AlphaCompression::Lossless => "lossless",
                    };
                    let case = format!("alpha_{w}x{h}_{comp_label}_f{filt}");
                    let dir = out.join(&case);
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("input.webp"), &file).unwrap();
                    std::fs::write(dir.join("expected.rgba"), &golden).unwrap();

                    let meta = Meta {
                        width: w,
                        height: h,
                        alpha_compression: comp,
                        alpha_filtering: u8::try_from(filt).unwrap(),
                        quality,
                        note: "libwebp ADVANCED encoder (method 4, alpha_quality=100) -> \
                             WebPDecodeRGBA golden"
                            .to_owned(),
                    };
                    let toml_text = toml::to_string(&meta).unwrap();
                    std::fs::write(dir.join("meta.toml"), toml_text).unwrap();
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Animated-lossy generators. The FFI helpers below are copied verbatim from
    // `crates/webpkit/tests/oracle.rs` (the animation differential), so the
    // fixtures replay the exact same mux `WebPAnimEncoder` -> `WebPAnimDecoder`
    // reference path the oracle validates.
    // -----------------------------------------------------------------------

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
            libwebp_sys::WebPAnimEncoderNewInternal(
                width as i32,
                height as i32,
                &raw const opts,
                abi,
            )
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
                unsafe {
                    libwebp_sys::WebPPictureImportRGBA(&raw mut picture, frame.as_ptr(), stride)
                } != 0,
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
            let got = unsafe {
                libwebp_sys::WebPAnimDecoderGetNext(dec, &raw mut buf, &raw mut timestamp)
            };
            assert!(got != 0 && !buf.is_null(), "WebPAnimDecoderGetNext failed");
            // SAFETY: `buf` points at `frame_len` valid, already-composited RGBA bytes
            // owned by the decoder (valid until the next call); copy them out now.
            let frame = unsafe { std::slice::from_raw_parts(buf, frame_len) }.to_vec();
            frames.push(frame);
        }
        (canvas_w, canvas_h, frames)
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

    /// Regenerate the committed animated-lossy fixtures. For each case in a small
    /// matrix — sizes {16x16, 15x9} × frame-count {3, 4} × {opaque, with-alpha} —
    /// it builds per-index-distinct frames, encodes them through the libwebp mux
    /// `WebPAnimEncoder` (the `input.webp`), composites them back with
    /// `WebPAnimDecoder` (the per-frame `frames.rgba` golden, concatenated in frame
    /// order), and writes a `meta.toml`. Run explicitly:
    /// `cargo test -p webpkit-conformance --features oracle -- --ignored gen_anim_fixtures`.
    #[test]
    #[ignore = "regenerates committed animated fixtures via libwebp; run explicitly"]
    fn gen_anim_fixtures() {
        let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/anim");
        let sizes = [(16u32, 16u32), (15, 9)];
        let frame_counts = [3u32, 4];
        let alphas = [(true, "opaque"), (false, "alpha")];
        let quality = 90.0f32;

        for &(w, h) in &sizes {
            for &nframes in &frame_counts {
                for &(opaque, kind) in &alphas {
                    let frames: Vec<Vec<u8>> =
                        (0..nframes).map(|i| anim_frame(w, h, i, opaque)).collect();
                    // alpha_compression=lossless, alpha_filtering=fast (matches the oracle).
                    let file = libwebp_encode_anim_lossy(&frames, w, h, quality, 1, 1);
                    let (gw, gh, golden_frames) = libwebp_anim_composite(&file);
                    assert_eq!((gw, gh), (w, h), "libwebp changed canvas dims for {w}x{h}");

                    let mut golden = Vec::new();
                    for f in &golden_frames {
                        golden.extend_from_slice(f);
                    }
                    let frame_count = u32::try_from(golden_frames.len()).unwrap();

                    let case = format!("anim_{w}x{h}_{nframes}f_{kind}");
                    let dir = out.join(&case);
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("input.webp"), &file).unwrap();
                    std::fs::write(dir.join("frames.rgba"), &golden).unwrap();

                    let meta = AnimMeta {
                        width: w,
                        height: h,
                        frame_count,
                        quality,
                        note: "libwebp mux WebPAnimEncoder (method 4, alpha_quality=100) -> \
                               WebPAnimDecoder composited golden"
                            .to_owned(),
                    };
                    let toml_text = toml::to_string(&meta).unwrap();
                    std::fs::write(dir.join("meta.toml"), toml_text).unwrap();
                }
            }
        }
    }
}

/// Ledger (re)generation, tool-free so `just gen-ledgers` regenerates every
/// committed ledger — not only the two that need no `oracle` build to *read*.
///
/// These were previously inside the `oracle`-gated `generate` module, "run in the
/// same pass as the fixtures". But `just gen-ledgers` builds without `oracle`, so
/// the tests did not exist in that build, `cargo test -- --ignored` matched zero,
/// and the recipe reported success while touching neither ledger. Ledger writing
/// only recomputes from committed fixtures; it needs no reference library.
#[cfg(test)]
mod regen {
    use std::path::Path;

    use super::{anim_results_to_json, compute_anim_results, compute_results, results_to_json};

    #[test]
    #[ignore = "regenerates the committed alpha ledger; run explicitly"]
    fn gen_ledger() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let results = compute_results(&root.join("fixtures/alpha")).expect("recompute ledger");
        // Honesty gate: never regenerate a ledger that certifies a FAILING decode.
        for r in &results {
            assert!(
                r.passed,
                "conformance case `{}` failed to decode; refusing to regenerate",
                r.case
            );
        }
        let json = results_to_json(&results).expect("serialize ledger");
        std::fs::write(root.join("conformance-results-alpha.json"), json).expect("write ledger");
    }

    #[test]
    #[ignore = "regenerates the committed animated ledger; run explicitly"]
    fn gen_anim_ledger() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let results =
            compute_anim_results(&root.join("fixtures/anim")).expect("recompute anim ledger");
        // Honesty gate: never regenerate a ledger that certifies a FAILING decode.
        for r in &results {
            assert!(
                r.passed,
                "anim conformance case `{}` failed to decode; refusing to regenerate",
                r.case
            );
        }
        let json = anim_results_to_json(&results).expect("serialize anim ledger");
        std::fs::write(root.join("conformance-results-anim.json"), json)
            .expect("write anim ledger");
    }
}
