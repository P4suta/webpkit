//! Criterion micro-benchmark isolating the VP8L **optimal-parse LZ77 match
//! finder** — the `parse_optimal` -> `find_candidates` backward DP that dominates
//! the `MatchCompare` work counter (see `corpus/work.json`).
//!
//! `parse_optimal` is `pub(crate)`, so it cannot be timed directly; instead this
//! group drives it through the public [`webpkit::lossless::encode`] on the three
//! **repetitive** archetypes whose long overlapping runs are the pathological
//! input for the finder — [`Content::Solid`] (distance-1 RLE),
//! [`Content::Gradient`] (distance-width), and [`Content::Tiled`] (distance-tile).
//! `Balanced` and `Best` both run the DP, so both are timed (Best capped to
//! `edge <= 256`, matching `encode.rs`). The smooth archetypes (photo/palette/
//! noise), whose cost lives elsewhere, are intentionally excluded to keep the
//! signal sharp.
//!
//! Local-only developer tool (not a CI gate). Sample counts are small because the
//! pathological points take ~seconds each; use criterion's baseline workflow to
//! measure a change:
//!
//! ```text
//! cargo bench -p webpkit-bench --bench match_finding -- --save-baseline pre
//! # ... make a change ...
//! cargo bench -p webpkit-bench --bench match_finding -- --baseline pre
//! ```
#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items"
)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use webpkit::lossless::{Dimensions, Effort, EncoderConfig, ImageRef, PixelLayout};
use webpkit_samples::{Content, SIZES, render};

/// The DP-driven effort methods (both run `parse_optimal`); `fast` never scans.
const METHODS: [(&str, Effort); 2] = [("auto", Effort::AUTO), ("l9", Effort::level(9))];

/// The repetitive archetypes whose overlapping runs stress the match finder.
const REPETITIVE: [Content; 3] = [Content::Solid, Content::Gradient, Content::Tiled];

fn match_finding_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("match_finding");
    // The pathological points cost ~seconds/iter; keep the run bounded.
    group
        .sample_size(10)
        .warm_up_time(Duration::from_millis(500));
    for &content in &REPETITIVE {
        for &edge in &SIZES {
            // Render + borrow once per (content, edge), outside the timed loop.
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
                // The deepest search at 512 is too slow to iterate; cap it.
                if method == Effort::level(9) && edge > 256 {
                    continue;
                }
                let config = EncoderConfig::new().with_effort(method);
                let id = BenchmarkId::new(format!("{name}/{}", content.name()), edge);
                group.bench_with_input(id, &image, |b, &image| {
                    b.iter(|| webpkit::lossless::encode(black_box(image), &config));
                });
            }
        }
    }
    group.finish();
}

criterion_group!(benches, match_finding_benchmark);
criterion_main!(benches);
