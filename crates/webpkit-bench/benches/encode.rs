//! Criterion throughput benchmarks for the `lossless` VP8L encoder.
//!
//! One `encode` group sweeps the shared [`webpkit_samples`] matrix (every
//! [`Content`] archetype x [`SIZES`]) crossed with the effort [`Effort`]s. `Fast`
//! exercises the literal + subtract-green path; `Balanced` runs the full Tier2
//! LZ77 + color-cache cost model; `Best` additionally searches the Tier3
//! forward-transform families (predictor / cross-color / palette). Throughput is
//! reported in **input MB/s** via
//! `Throughput::Bytes(edge * edge * 4)` (a method-independent basis: the raw RGBA
//! byte count the encoder consumes), and each [`ImageRef`] is built **once** per
//! `(content, edge)` outside the measured loop.
//!
//! `Best` is capped to `edge <= 256`: the Tier3 search over a 512x512 image,
//! iterated by criterion, runs into minutes per point. `Fast`/`Balanced` are
//! timed at every size.
//!
//! Building with `--features rayon` forwards `webpkit/rayon`, so the parallel `Best`
//! evaluator is timed. Compare serial vs parallel with criterion's two-baseline
//! workflow (below): record a `serial` baseline without the feature, then
//! `--features rayon -- --baseline serial`.
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
use webpkit::lossless::{Dimensions, Effort, EncoderConfig, ImageRef, PixelLayout};
use webpkit_samples::{Content, SIZES, render};

/// The effort methods timed, with their stable id fragment. `Best` is filtered by
/// size below; `Fast`/`Balanced` run at every edge.
const METHODS: [(&str, Effort); 3] = [
    ("fast", Effort::Fast),
    ("balanced", Effort::Balanced),
    ("best", Effort::Best),
];

fn encode_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");
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
                // Best's Tier3 search at 512 is too slow to iterate; cap it.
                if method == Effort::Best && edge > 256 {
                    continue;
                }
                let config = EncoderConfig::new().with_effort(method);
                let id = BenchmarkId::new(format!("{name}/{}", content.name()), edge);
                group.bench_with_input(id, &image, |b, &image| {
                    // Return the `Result` so criterion black-boxes it; no `unwrap`.
                    b.iter(|| webpkit::lossless::encode(black_box(image), &config));
                });
            }
        }
    }
    group.finish();
}

criterion_group!(benches, encode_benchmark);
criterion_main!(benches);
