//! Criterion throughput benchmarks for the `lossless` VP8L decoder.
//!
//! Both groups sweep the shared [`webpkit_samples`] matrix (every [`Content`]
//! archetype x [`SIZES`]), so the same synthetic images the metrics ledger sizes
//! are the ones timed here. Each sample is pre-encoded **once** (Balanced),
//! outside the measured loop, and throughput is reported in **Mpixels/s** via
//! `Throughput::Elements(edge * edge)`:
//!
//! - `decode/oneshot` — a whole-buffer [`webpkit::lossless::decode`].
//! - `decode/streaming` — the push-based [`IncrementalDecoder`], fed in fixed
//!   4 KiB chunks, finalized with `into_image`.
//!
//! These are **local-only** developer tools (run via `just bench`); they are
//! intentionally NOT a CI timing gate — wall-clock time is noisy and
//! hardware-dependent, so byte/correctness regressions are guarded elsewhere
//! (`corpus/baseline.json`, `corpus/metrics.json`), not here. To compare two
//! runs, use criterion's baseline workflow:
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
use webpkit::lossless::{
    Dimensions, Effort, EncoderConfig, ImageRef, IncrementalDecoder, PixelLayout,
};
use webpkit_samples::{Content, SIZES, render};

/// Encode one matrix sample to a complete WebP file (Balanced) for the decode
/// benches. Returns `None` on any setup error so a bad sample is skipped rather
/// than `unwrap`ped (the workspace denies `unwrap`/`expect` outside `#[test]`).
fn encode_sample(content: Content, edge: u32) -> Option<Vec<u8>> {
    let sample = render(content, edge);
    let dims = Dimensions::new(edge, edge).ok()?;
    let image = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba).ok()?;
    let config = EncoderConfig::new().with_effort(Effort::AUTO);
    webpkit::lossless::encode(image, &config).ok()
}

/// Mpixels/s basis for an `edge` x `edge` image (method-independent).
fn pixels(edge: u32) -> u64 {
    u64::from(edge) * u64::from(edge)
}

/// `decode/oneshot`: whole-buffer [`webpkit::lossless::decode`] over the matrix.
fn decode_oneshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode/oneshot");
    for &content in &Content::ALL {
        for &edge in &SIZES {
            // Pre-encode once, outside the measured loop; skip on setup error.
            let Some(webp) = encode_sample(content, edge) else {
                continue;
            };
            group.throughput(Throughput::Elements(pixels(edge)));
            group.bench_with_input(BenchmarkId::new(content.name(), edge), &webp, |b, webp| {
                // Return the `Result` so criterion black-boxes it; no `unwrap`.
                b.iter(|| webpkit::lossless::decode(black_box(webp)));
            });
        }
    }
    group.finish();
}

/// `decode/streaming`: drive [`IncrementalDecoder`] over 4 KiB chunks, then
/// assemble with `into_image`. `drain_rows` is available as a non-consuming early
/// view of finalized rows (a pure-streaming consumer would read it instead of
/// `into_image`), but is kept out of the timed loop: draining-and-freeing rows
/// forces `into_image` to re-decode from the buffer, whereas leaving them retained
/// lets it assemble with no second decode — so the timing reflects one streamed
/// decode, not a stream plus a re-decode.
fn decode_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode/streaming");
    for &content in &Content::ALL {
        for &edge in &SIZES {
            let Some(webp) = encode_sample(content, edge) else {
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
