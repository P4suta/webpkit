//! Criterion throughput benchmarks for the `lossy` VP8 (lossy) decoder.
//!
//! Both groups sweep the shared [`webpkit_samples`] matrix (every [`Content`]
//! archetype x [`SIZES`]), so the same synthetic images the metrics ledgers size
//! are the ones timed here. Each sample is pre-encoded **once** (Balanced, a
//! mid quality), outside the measured loop, and throughput is reported in
//! **Mpixels/s** via `Throughput::Elements(edge * edge)`:
//!
//! - `lossy_decode/oneshot` — a whole-buffer [`webpkit::lossy::decode`] over the
//!   raw `VP8 ` payload from [`webpkit::lossy::encode_vp8`].
//! - `lossy_decode/streaming` — the push-based [`IncrementalDecoder`], fed the
//!   full WebP container from [`webpkit::lossy::encode`] in fixed 4 KiB chunks and
//!   finalized with `into_image`.
//!
//! These are **local-only** developer tools (run via `just bench`); they are
//! intentionally NOT a CI timing gate — wall-clock time is noisy and
//! hardware-dependent, so byte/correctness regressions are guarded elsewhere
//! (`corpus/work.json`'s deterministic decode counters, the golden conformance
//! tests, the libwebp oracle), not here. To compare two runs, use criterion's
//! baseline workflow:
//!
//! ```text
//! cargo bench -p webpkit-bench -- --save-baseline main   # record a baseline
//! # ... make a change ...
//! cargo bench -p webpkit-bench -- --baseline main         # compare against it
//! ```
#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items"
)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use webpkit::lossy::{Dimensions, Effort, ImageRef, IncrementalDecoder, LossyConfig, PixelLayout};
use webpkit_samples::{Content, SIZES, render};

/// The quality the decode benches' inputs are encoded at — a mid quality, so the
/// decoded coefficient density / filter activity is representative rather than
/// trivially sparse (very low q) or saturated (very high q).
const DECODE_QUALITY: u8 = 75;

/// Encode one matrix sample to a raw `VP8 ` payload (Balanced) for the oneshot
/// decode bench. Returns `None` on any setup error so a bad sample is skipped
/// rather than `unwrap`ped (the workspace denies `unwrap`/`expect` outside tests).
fn encode_vp8_sample(content: Content, edge: u32) -> Option<Vec<u8>> {
    let sample = render(content, edge);
    let dims = Dimensions::new(edge, edge).ok()?;
    let image = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba).ok()?;
    let cfg = LossyConfig::new()
        .with_quality(DECODE_QUALITY)
        .with_effort(Effort::AUTO);
    let (_dims, payload) = webpkit::lossy::encode_vp8(image, &cfg).ok()?;
    Some(payload)
}

/// Encode one matrix sample to a complete WebP container (Balanced) for the
/// streaming decode bench, which feeds the [`IncrementalDecoder`] a container.
fn encode_container_sample(content: Content, edge: u32) -> Option<Vec<u8>> {
    let sample = render(content, edge);
    let dims = Dimensions::new(edge, edge).ok()?;
    let image = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba).ok()?;
    let cfg = LossyConfig::new()
        .with_quality(DECODE_QUALITY)
        .with_effort(Effort::AUTO);
    webpkit::lossy::encode(image, &cfg).ok()
}

/// Mpixels/s basis for an `edge` x `edge` image (method-independent).
fn pixels(edge: u32) -> u64 {
    u64::from(edge) * u64::from(edge)
}

/// `lossy_decode/oneshot`: whole-buffer [`webpkit::lossy::decode`] over the matrix.
fn decode_oneshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("lossy_decode/oneshot");
    for &content in &Content::ALL {
        for &edge in &SIZES {
            // Pre-encode once, outside the measured loop; skip on setup error.
            let Some(payload) = encode_vp8_sample(content, edge) else {
                continue;
            };
            group.throughput(Throughput::Elements(pixels(edge)));
            group.bench_with_input(
                BenchmarkId::new(content.name(), edge),
                &payload,
                |b, payload| {
                    // Return the `Result` so criterion black-boxes it; no `unwrap`.
                    b.iter(|| webpkit::lossy::decode(black_box(payload)));
                },
            );
        }
    }
    group.finish();
}

/// `lossy_decode/streaming`: drive [`IncrementalDecoder`] over 4 KiB chunks of the
/// WebP container, then assemble with `into_image`.
fn decode_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("lossy_decode/streaming");
    for &content in &Content::ALL {
        for &edge in &SIZES {
            let Some(webp) = encode_container_sample(content, edge) else {
                continue;
            };
            group.throughput(Throughput::Elements(pixels(edge)));
            group.bench_with_input(BenchmarkId::new(content.name(), edge), &webp, |b, webp| {
                b.iter(|| {
                    let mut dec = IncrementalDecoder::new();
                    for chunk in webp.chunks(4096) {
                        let _ = dec.push(black_box(chunk));
                    }
                    black_box(dec.into_image())
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, decode_oneshot, decode_streaming);
criterion_main!(benches);
