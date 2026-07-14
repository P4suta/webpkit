//! Criterion throughput benchmarks for the `lossless` animation path.
//!
//! A deliberately small matrix — every [`Content`] archetype at `edge` in
//! `{64, 256}`, `N = 4` frames — so a smoke-run stays quick. Frames come from
//! [`webpkit_samples::render_frame`] (`frame` 0..4 perturbs the PRNG archetypes so
//! successive frames differ). Throughput is reported in **Mpixels/s** via
//! `Throughput::Elements(N * edge * edge)` (the total pixels across all frames):
//!
//! - `animation/encode` — build an [`AnimationEncoder`] from the 4 frames and
//!   `finish` the whole file.
//! - `animation/decode` — [`webpkit::lossless::decode_frames`] then `composited().count()`
//!   over a pre-built animation.
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
use webpkit::lossless::{
    AnimationEncoder, BlendMode, Dimensions, DisposalMode, FrameMeta, ImageRef, PixelLayout,
};
use webpkit_samples::{Content, Sample, render_frame};

/// Frames per animation.
const FRAMES: u32 = 4;

/// The small edge set for the animation matrix (kept below [`webpkit_samples::SIZES`]'s
/// 512 so a smoke-run is quick).
const EDGES: [u32; 2] = [64, 256];

/// Total-pixels basis (Mpixels/s) for an `N`-frame `edge` x `edge` animation.
fn total_pixels(edge: u32) -> u64 {
    u64::from(FRAMES) * u64::from(edge) * u64::from(edge)
}

/// A full-canvas frame meta: no offset, blend, keep.
const fn full_frame_meta(canvas: Dimensions) -> FrameMeta {
    FrameMeta {
        x: 0,
        y: 0,
        dimensions: canvas,
        duration_ms: 100,
        blend: BlendMode::Blend,
        dispose: DisposalMode::Keep,
    }
}

/// Render the `N` frame samples for one `(content, edge)`. Each frame owns its
/// RGBA, so the [`ImageRef`]s the encoder borrows stay valid across the loop.
fn frame_samples(content: Content, edge: u32) -> Vec<Sample> {
    (0..FRAMES)
        .map(|n| render_frame(content, edge, n))
        .collect()
}

/// Assemble one animation from pre-rendered frame samples. Returns `None` on any
/// setup error so a point is skipped rather than `unwrap`ped (denied outside
/// `#[test]`).
fn build_animation(canvas: Dimensions, frames: &[Sample]) -> Option<Vec<u8>> {
    let meta = full_frame_meta(canvas);
    let (first, rest) = frames.split_first()?;
    let first_ref = ImageRef::new(canvas, PixelLayout::Rgba8, &first.rgba).ok()?;
    let mut enc = AnimationEncoder::new(canvas)
        .add_frame(first_ref, meta)
        .ok()?;
    for frame in rest {
        let frame_ref = ImageRef::new(canvas, PixelLayout::Rgba8, &frame.rgba).ok()?;
        enc = enc.add_frame(frame_ref, meta).ok()?;
    }
    Some(enc.finish())
}

/// `animation/encode`: build + `finish` a 4-frame animation each iteration.
fn animation_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("animation/encode");
    for &content in &Content::ALL {
        for &edge in &EDGES {
            let Ok(canvas) = Dimensions::new(edge, edge) else {
                continue;
            };
            // Render the frame samples once, outside the measured loop.
            let frames = frame_samples(content, edge);
            group.throughput(Throughput::Elements(total_pixels(edge)));
            group.bench_with_input(
                BenchmarkId::new(content.name(), edge),
                &frames,
                |b, frames| {
                    b.iter(|| build_animation(canvas, black_box(frames)));
                },
            );
        }
    }
    group.finish();
}

/// `animation/decode`: `decode_frames` + composite every frame over a pre-built
/// animation.
fn animation_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("animation/decode");
    for &content in &Content::ALL {
        for &edge in &EDGES {
            let Ok(canvas) = Dimensions::new(edge, edge) else {
                continue;
            };
            let frames = frame_samples(content, edge);
            // Pre-build one animation once, outside the measured loop; skip on error.
            let Some(webp) = build_animation(canvas, &frames) else {
                continue;
            };
            group.throughput(Throughput::Elements(total_pixels(edge)));
            group.bench_with_input(BenchmarkId::new(content.name(), edge), &webp, |b, webp| {
                b.iter(|| {
                    let Ok(frames) = webpkit::lossless::decode_frames(black_box(webp)) else {
                        return;
                    };
                    // `count()` drives the compositor over every frame; black-box
                    // it so the decode+composite work is not elided.
                    black_box(frames.composited().count());
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, animation_encode, animation_decode);
criterion_main!(benches);
