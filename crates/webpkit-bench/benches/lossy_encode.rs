//! Criterion throughput benchmarks for the `lossy` VP8 (lossy) encoder.
//!
//! One `lossy_encode` group sweeps the shared [`webpkit_samples`] matrix (every
//! [`Content`] archetype x [`SIZES`]) crossed with the effort [`Effort`]s at a
//! fixed mid quality. `Fast` is the single-mode round-to-nearest path;
//! `Balanced` runs the full per-mode RD search; `Best` additionally runs the
//! trellis quantizer and the i4x4 luma search. Throughput is reported in
//! **input MB/s** via `Throughput::Bytes(edge * edge * 4)` (a method-independent
//! basis: the raw RGBA byte count the encoder consumes), and each [`ImageRef`] is
//! built **once** per `(content, edge)` outside the measured loop.
//!
//! `Best` is capped to `edge <= 256`: the trellis + i4x4 search over a 512x512
//! image, iterated by criterion, runs into minutes per point. `Fast`/`Balanced`
//! are timed at every size.
//!
//! These are **local-only** developer tools (run via `just bench`); they are
//! intentionally NOT a CI timing gate. To compare two runs, use criterion's
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
use webpkit::lossy::{Dimensions, Effort, ImageRef, LossyConfig, PixelLayout};
use webpkit_samples::{Content, SIZES, render};

/// The quality all encode points use — a mid quality, so the RD search is doing
/// representative work (not the near-lossless or near-empty extremes).
const ENCODE_QUALITY: u8 = 75;

/// The effort methods timed, with their stable id fragment. `Best` is filtered by
/// size below; `Fast`/`Balanced` run at every edge.
const METHODS: [(&str, Effort); 3] = [
    ("fast", Effort::Fast),
    ("balanced", Effort::Balanced),
    ("best", Effort::Best),
];

fn encode_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("lossy_encode");
    for &content in &Content::ALL {
        for &edge in &SIZES {
            // Render + borrow once per (content, edge); `sample` owns the RGBA the
            // `ImageRef` borrows and outlives every method's bench below. Skip on
            // any setup error rather than `unwrap` (denied outside `#[test]`).
            let sample = render(content, edge);
            let Ok(dims) = Dimensions::new(edge, edge) else {
                continue;
            };
            let Ok(image) = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba) else {
                continue;
            };
            // Input MB/s: the raw RGBA byte count, independent of the method.
            group.throughput(Throughput::Bytes(u64::from(edge) * u64::from(edge) * 4));
            for (name, method) in METHODS {
                // Best's trellis + i4x4 search at 512 is too slow to iterate; cap it.
                if method == Effort::Best && edge > 256 {
                    continue;
                }
                let cfg = LossyConfig::new()
                    .with_quality(ENCODE_QUALITY)
                    .with_effort(method);
                let id = BenchmarkId::new(format!("{name}/{}", content.name()), edge);
                group.bench_with_input(id, &image, |b, &image| {
                    // Return the `Result` so criterion black-boxes it; no `unwrap`.
                    b.iter(|| webpkit::lossy::encode_vp8(black_box(image), &cfg));
                });
            }
        }
    }
    group.finish();
}

criterion_group!(benches, encode_benchmark);
criterion_main!(benches);
