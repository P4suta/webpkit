//! Generate a self-contained HTML dashboard comparing `webpkit`'s lossy encoder
//! against the libwebp C reference (linked in-process via `libwebp-sys`) on size,
//! lossy reconstruction quality, and wall-clock speed.
//!
//! This is a developer measurement tool, not a gate — wall-clock is machine- and
//! thermal-dependent, so the report is written to `target/` (local, never
//! committed) alongside the deterministic ledgers that CI does gate. Run it with:
//!
//! ```text
//! just report-vs-libwebp                     # -> target/vs-libwebp.html
//! cargo run -p webpkit --example vs_libwebp --features oracle --release -- out.html
//! ```
//!
//! Fairness: encode is effort-matched (our Fast/Balanced/Best vs libwebp method
//! 1/4/6) at quality 75 on identical pixels; decode is timed on each codec's own
//! output (VP8 decode is bit-exact per RFC 6386, so cross-decoding is redundant);
//! libwebp is called in-process, so there is no CLI startup or file-IO skew.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "developer measurement example (not shipped library code): it prints \
              its progress and the report path, unwraps on local file I/O, and casts \
              image dimensions to the libwebp FFI's i32 — all fine for a demo binary"
)]

use std::hint::black_box;
use std::time::Instant;

use webpkit::lossy::{Dimensions, Effort, ImageRef, LossyConfig, PixelLayout};
use webpkit_samples::{Content, render};

const QUALITY: u8 = 75;
const EDGE: u32 = 256;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/vs-libwebp.html".to_string());

    let mut rows = Vec::new();
    for content in Content::ALL {
        eprintln!("measuring {}…", content.name());
        rows.push(measure(content));
    }

    let html = render_html(&rows);
    std::fs::write(&out, html).expect("write HTML report");
    println!(
        "wrote {out} ({} content archetypes at {EDGE}px)",
        rows.len()
    );
}

/// One content archetype's full comparison record.
struct Row {
    name: &'static str,
    // lossy VP8 @ quality 75 (ours Best vs libwebp method 6)
    lossy_ours_bytes: usize,
    lossy_lw_bytes: usize,
    lossy_ours_psnr: f64,
    lossy_lw_psnr: f64,
    // lossless VP8L (ours Best vs libwebp lossless)
    ll_ours_bytes: usize,
    ll_lw_bytes: usize,
    // encode wall-clock (ns), effort-matched tiers, and decode
    enc_ours: [f64; 3], // fast, balanced, best
    enc_lw: [f64; 3],   // method 1, 4, 6
    dec_ours: f64,
    dec_lw: f64,
}

fn measure(content: Content) -> Row {
    let sample = render(content, EDGE);
    let rgba = &sample.rgba;
    let rgb = rgba_to_rgb(rgba);
    let dims = Dimensions::new(EDGE, EDGE).unwrap();
    let image = ImageRef::new(dims, PixelLayout::Rgba8, rgba).unwrap();

    // ---- lossy size + quality (ours Best vs libwebp method 6) ----
    let best = LossyConfig::new()
        .with_quality(QUALITY)
        .with_effort(Effort::Best);
    let (_d, ours_payload) = webpkit::lossy::encode_vp8(image, &best).unwrap();
    let ours_rgba = webpkit::lossy::decode(&ours_payload).unwrap().into_pixels();
    let lossy_ours_psnr = psnr_rgb(rgba, &ours_rgba);

    let lw_container = libwebp_encode(&rgb, EDGE, EDGE, f32::from(QUALITY), 6);
    let (lw_rgba, _lww, _lwh) = libwebp_decode(&lw_container);
    let lossy_lw_psnr = psnr_rgb(rgba, &lw_rgba);

    // ---- lossless size (ours Best vs libwebp lossless) ----
    let ll_cfg =
        webpkit::lossless::EncoderConfig::default().with_effort(webpkit::lossless::Effort::Best);
    let ll_ours = webpkit::lossless::encode(image, &ll_cfg).unwrap();
    let ll_lw = libwebp_encode_lossless(rgba, EDGE, EDGE);

    // ---- speed: encode tiers (ours vs effort-matched libwebp method) ----
    let tiers = [
        (Effort::Fast, 1i32),
        (Effort::Balanced, 4),
        (Effort::Best, 6),
    ];
    let mut enc_ours = [0.0; 3];
    let mut enc_lw = [0.0; 3];
    for (i, (method, lw_method)) in tiers.into_iter().enumerate() {
        let cfg = LossyConfig::new().with_quality(QUALITY).with_effort(method);
        enc_ours[i] = median_ns(5, 400, || webpkit::lossy::encode_vp8(image, &cfg));
        enc_lw[i] = median_ns(5, 400, || {
            libwebp_encode(&rgb, EDGE, EDGE, f32::from(QUALITY), lw_method)
        });
    }

    // ---- speed: decode (each codec on its own output) ----
    let balanced = LossyConfig::new()
        .with_quality(QUALITY)
        .with_effort(Effort::Balanced);
    let (_d, dec_payload) = webpkit::lossy::encode_vp8(image, &balanced).unwrap();
    let dec_container = libwebp_encode(&rgb, EDGE, EDGE, f32::from(QUALITY), 4);
    let dec_ours = median_ns(10, 400, || webpkit::lossy::decode(&dec_payload));
    let dec_lw = median_ns(10, 400, || libwebp_decode(&dec_container));

    Row {
        name: content.name(),
        lossy_ours_bytes: ours_payload.len(),
        lossy_lw_bytes: lw_container.len(),
        lossy_ours_psnr,
        lossy_lw_psnr,
        ll_ours_bytes: ll_ours.len(),
        ll_lw_bytes: ll_lw.len(),
        enc_ours,
        enc_lw,
        dec_ours,
        dec_lw,
    }
}

// ---------------------------------------------------------------------------
// measurement helpers

/// Drop the alpha lane: `RGBA8` -> packed `RGB8` (lossy discards alpha anyway).
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    rgb
}

/// RGB peak-signal-to-noise ratio (dB) of `got` against `src` (both `RGBA8`,
/// alpha ignored). Capped at 99 dB for a perfect match.
fn psnr_rgb(src: &[u8], got: &[u8]) -> f64 {
    let n = src.len().min(got.len());
    let mut sse = 0u64;
    let mut count = 0u64;
    let mut i = 0;
    while i < n {
        for c in 0..3 {
            let d = i32::from(src[i + c]) - i32::from(got[i + c]);
            sse += (d * d) as u64;
        }
        count += 3;
        i += 4;
    }
    if sse == 0 {
        return 99.0;
    }
    let mse = sse as f64 / count as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

/// Median nanoseconds per call of `f`, warmed up then measured for at least
/// `min_iters` iterations and `min_millis` of wall-clock (whichever is longer).
fn median_ns<T>(min_iters: u32, min_millis: u128, mut f: impl FnMut() -> T) -> f64 {
    for _ in 0..3 {
        black_box(f());
    }
    let mut samples = Vec::new();
    let start = Instant::now();
    let mut iters = 0u32;
    while iters < min_iters || start.elapsed().as_millis() < min_millis {
        let t = Instant::now();
        black_box(f());
        samples.push(t.elapsed().as_nanos());
        iters += 1;
    }
    samples.sort_unstable();
    samples[samples.len() / 2] as f64
}

// ---------------------------------------------------------------------------
// libwebp (in-process, via libwebp-sys)

/// libwebp advanced lossy encode at `quality`/`method`, returning the container.
fn libwebp_encode(rgb: &[u8], width: u32, height: u32, quality: f32, method: i32) -> Vec<u8> {
    let mut config = libwebp_sys::WebPConfig::new().unwrap();
    config.lossless = 0;
    config.quality = quality;
    config.method = method;
    // SAFETY: `config` is a fully-initialized WebPConfig.
    assert!(unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } != 0);

    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 0;
    picture.width = width as i32;
    picture.height = height as i32;
    let stride = (width * 3) as i32;
    // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGB(&raw mut picture, rgb.as_ptr(), stride) } != 0
    );

    let mut writer = std::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
    // SAFETY: initializes the whole struct in place.
    unsafe { libwebp_sys::WebPMemoryWriterInit(writer.as_mut_ptr()) };
    let mut writer = unsafe { writer.assume_init() };
    picture.writer = Some(libwebp_sys::WebPMemoryWrite);
    picture.custom_ptr = (&raw mut writer).cast();

    // SAFETY: `config`/`picture` are set up; the writer appends the stream.
    let ok = unsafe { libwebp_sys::WebPEncode(&raw const config, &raw mut picture) };
    assert!(ok != 0, "libwebp encode failed: {:?}", picture.error_code);
    // SAFETY: on success `writer.mem` points at `writer.size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(writer.mem, writer.size) }.to_vec();
    // SAFETY: free the writer buffer and picture planes exactly once.
    unsafe { libwebp_sys::WebPMemoryWriterClear(&raw mut writer) };
    unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
    bytes
}

/// libwebp lossless encode of `rgba`, returning the container size in bytes.
fn libwebp_encode_lossless(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out: *mut u8 = core::ptr::null_mut();
    let stride = (width * 4) as i32;
    // SAFETY: `rgba` holds `width*height*4` bytes at `stride`; libwebp writes the
    // freshly allocated stream pointer into `out`.
    let size = unsafe {
        libwebp_sys::WebPEncodeLosslessRGBA(
            rgba.as_ptr(),
            width as i32,
            height as i32,
            stride,
            &raw mut out,
        )
    };
    assert!(!out.is_null() && size > 0, "libwebp lossless encode failed");
    // SAFETY: on success `out` points at `size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out, size) }.to_vec();
    // SAFETY: `out` was allocated by libwebp and is freed exactly once here.
    unsafe { libwebp_sys::WebPFree(out.cast()) };
    bytes
}

/// Decode a WebP container with libwebp to RGBA `(pixels, width, height)`.
fn libwebp_decode(webp: &[u8]) -> (Vec<u8>, u32, u32) {
    let (mut w, mut h) = (0i32, 0i32);
    // SAFETY: `webp` is a valid buffer; libwebp allocates the output and writes
    // the dimensions through the out-pointers.
    let ptr =
        unsafe { libwebp_sys::WebPDecodeRGBA(webp.as_ptr(), webp.len(), &raw mut w, &raw mut h) };
    assert!(!ptr.is_null(), "libwebp decode failed");
    let len = w as usize * h as usize * 4;
    // SAFETY: libwebp returned `w*h*4` valid RGBA bytes at `ptr`.
    let rgba = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    // SAFETY: `ptr` was allocated by libwebp and is freed exactly once here.
    unsafe { libwebp_sys::WebPFree(ptr.cast()) };
    (rgba, w as u32, h as u32)
}

// ---------------------------------------------------------------------------
// HTML report

/// Serialize the measured rows into the `window.__DATA__` JSON the page renders.
fn data_json(rows: &[Row]) -> String {
    let mut items = Vec::new();
    for r in rows {
        items.push(format!(
            "{{\"name\":\"{}\",\"lossy\":{{\"ob\":{},\"lb\":{},\"op\":{:.2},\"lp\":{:.2}}},\
             \"ll\":{{\"ob\":{},\"lb\":{}}},\
             \"enc\":{{\"ours\":[{:.0},{:.0},{:.0}],\"lw\":[{:.0},{:.0},{:.0}]}},\
             \"dec\":{{\"ours\":{:.0},\"lw\":{:.0}}}}}",
            r.name,
            r.lossy_ours_bytes,
            r.lossy_lw_bytes,
            r.lossy_ours_psnr,
            r.lossy_lw_psnr,
            r.ll_ours_bytes,
            r.ll_lw_bytes,
            r.enc_ours[0],
            r.enc_ours[1],
            r.enc_ours[2],
            r.enc_lw[0],
            r.enc_lw[1],
            r.enc_lw[2],
            r.dec_ours,
            r.dec_lw,
        ));
    }
    format!("[{}]", items.join(","))
}

fn render_html(rows: &[Row]) -> String {
    TEMPLATE.replace("__DATA_JSON__", &data_json(rows))
}

/// The self-contained dashboard. `__DATA_JSON__` is replaced with the measured
/// rows; all rendering (ratio bars, tables, theming) happens in the inline JS so
/// the same page works from any generated data set.
const TEMPLATE: &str = include_str!("vs_libwebp_template.html");
