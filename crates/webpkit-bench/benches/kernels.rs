//! Isolated per-kernel microbenchmarks for the autovectorization loop.
//!
//! Unlike the pipeline benches, this target times individual numeric kernels
//! against their pre-optimization `*_reference` twins **in the same criterion
//! run**. Because the optimized kernel and its reference are measured back-to-back
//! in one invocation — same thermal window, same profile — their opt-vs-ref delta
//! is read directly from the two `new/estimates.json` point estimates, sidestepping
//! the cold-`--save-baseline` bias that makes cross-run deltas on this machine
//! unreliable (see `docs/benchmarking.md`).
//!
//! Needs `--features bench` (which exposes the kernels via each codec's
//! `crate::bench` shims); without it this target compiles to an empty `main`, so a
//! plain `cargo bench` skips it.
//!
//! ```text
//! cargo bench -p webpkit-bench --features bench --bench kernels
//! # then, per size, compare the two point estimates (ns/iter, lower = faster):
//! #   jq .mean.point_estimate target/criterion/sse_block/opt/16/new/estimates.json
//! #   jq .mean.point_estimate target/criterion/sse_block/ref/16/new/estimates.json
//! ```
#![allow(
    missing_docs,
    reason = "criterion_group!/criterion_main! expand to undocumented public items"
)]

#[cfg(feature = "bench")]
use std::hint::black_box;
#[cfg(feature = "bench")]
use std::time::Duration;

#[cfg(feature = "bench")]
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
#[cfg(feature = "bench")]
use webpkit::lossless::bench::{
    cross_color_inverse_row, cross_color_inverse_row_reference, sweep_blue, sweep_blue_reference,
};
#[cfg(feature = "bench")]
use webpkit::lossy::bench::{
    residual_block, residual_block_reference, sse_block, sse_block_reference, true_motion,
    true_motion_reference,
};

/// Deterministic `SplitMix64` byte fill so every run benches byte-identical inputs
/// (matching the equivalence proptest's generator).
#[cfg(feature = "bench")]
fn fill(len: usize, mut state: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            // Low byte of the mixed word — no truncating cast.
            (z ^ (z >> 31)).to_le_bytes()[0]
        })
        .collect()
}

/// The block sizes `sse_block` is called with in the RD search: 16 (luma 16x16),
/// 8 (chroma 8x8), 4 (luma 4x4 subblock).
#[cfg(feature = "bench")]
const SIZES: [usize; 3] = [16, 8, 4];

#[cfg(feature = "bench")]
fn sse_block_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("sse_block");
    // The kernel is nanosecond-scale, so a larger sample and longer measurement
    // shrink the CI enough to resolve a single-digit-% opt-vs-ref gap. Local-only;
    // never a CI gate.
    group
        .sample_size(200)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));
    for size in SIZES {
        // A row-contiguous source plane and a padded reconstruction plane, exactly
        // the two-pitch shape the RD callers pass.
        let src_stride = size;
        let pred_stride = size + 8;
        let src = fill(size * src_stride, 0xA5A5_5A5A_1234_5678);
        let pred = fill(size * pred_stride, 0x1357_9BDF_2468_ACE0);

        // Register the optimized kernel and its reference adjacently so both are
        // timed in this one run (the back-to-back A/B).
        group.bench_with_input(BenchmarkId::new("opt", size), &size, |b, &size| {
            b.iter(|| {
                sse_block(
                    black_box(&src),
                    0,
                    src_stride,
                    black_box(&pred),
                    0,
                    pred_stride,
                    size,
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("ref", size), &size, |b, &size| {
            b.iter(|| {
                sse_block_reference(
                    black_box(&src),
                    0,
                    src_stride,
                    black_box(&pred),
                    0,
                    pred_stride,
                    size,
                )
            });
        });
    }
    group.finish();
}

#[cfg(feature = "bench")]
fn residual_block_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("residual_block");
    group
        .sample_size(200)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));
    // A row-contiguous source plane read at an interior `(src_x, src_y)` and a
    // padded 4-row reconstruction plane, the two-pitch shape the RD callers pass.
    let (src_stride, src_x, src_y) = (64usize, 8usize, 8usize);
    let pred_stride = 4 + 8;
    let src = fill((src_y + 4) * src_stride, 0xA5A5_5A5A_1234_5678);
    let pred = fill(4 * pred_stride, 0x1357_9BDF_2468_ACE0);

    group.bench_function(BenchmarkId::new("opt", "4x4"), |b| {
        b.iter(|| {
            residual_block(
                black_box(&src),
                src_stride,
                src_x,
                src_y,
                black_box(&pred),
                0,
                pred_stride,
            )
        });
    });
    group.bench_function(BenchmarkId::new("ref", "4x4"), |b| {
        b.iter(|| {
            residual_block_reference(
                black_box(&src),
                src_stride,
                src_x,
                src_y,
                black_box(&pred),
                0,
                pred_stride,
            )
        });
    });
    group.finish();
}

#[cfg(feature = "bench")]
fn true_motion_benchmark(c: &mut Criterion) {
    // One top border row + one left border column precede the block origin; the
    // plane is tall enough for a 16-row block (STRIDE=24, 20 rows). `true_motion`
    // never writes those borders, so repeated calls are idempotent — no per-iter
    // reset needed.
    const STRIDE: usize = 24;
    const OFF: usize = STRIDE + 1;
    let mut group = c.benchmark_group("true_motion");
    group
        .sample_size(200)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));
    for size in [16usize, 8, 4] {
        let plane = fill(STRIDE * 20, 0xC0FF_EE00_D15E_A5ED);
        let mut plane_opt = plane.clone();
        let mut plane_ref = plane;
        group.bench_with_input(BenchmarkId::new("opt", size), &size, |b, &size| {
            b.iter(|| true_motion(black_box(&mut plane_opt), OFF, STRIDE, size));
        });
        group.bench_with_input(BenchmarkId::new("ref", size), &size, |b, &size| {
            b.iter(|| true_motion_reference(black_box(&mut plane_ref), OFF, STRIDE, size));
        });
    }
    group.finish();
}

#[cfg(feature = "bench")]
fn sweep_blue_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("sweep_blue");
    group
        .sample_size(200)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));
    // A representative cross-color tile (16×16 = 256 pixels); `sweep_blue` runs a
    // 256-value multiplier sweep over it.
    let n = 256usize;
    let bytes = fill(n * 2, 0x9E37_79B9_7F4A_7C15);
    let green: Vec<i8> = bytes[..n].iter().map(|&b| i8::from_le_bytes([b])).collect();
    let red: Vec<u8> = bytes[n..].to_vec();
    // `blue_base` in the caller's realistic `i32::from(px.b) - held_delta` range.
    let blue_base: Vec<i32> = green.iter().map(|&g| i32::from(g) - 128).collect();
    let mut channel: Vec<i8> = Vec::new();

    group.bench_function(BenchmarkId::new("opt", n), |b| {
        b.iter(|| {
            sweep_blue(
                black_box(&green),
                black_box(&red),
                black_box(&blue_base),
                false,
                &mut channel,
            )
        });
    });
    group.bench_function(BenchmarkId::new("ref", n), |b| {
        b.iter(|| {
            sweep_blue_reference(
                black_box(&green),
                black_box(&red),
                black_box(&blue_base),
                false,
            )
        });
    });
    group.finish();
}

#[cfg(feature = "bench")]
fn cross_color_inverse_row_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("cross_color_inverse_row");
    group
        .sample_size(200)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));
    // A 512-px decode row over an 8-px tile grid (bits=3): 64 runs of 8 pixels, the
    // shape where hoisting the per-pixel code unpack to per-run should show.
    let bits = 3u32;
    let width = 512usize;
    let bytes = fill(width * 4, 0x9E37_79B9_7F4A_7C15);
    let row0: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // One tile code per tile column; codes vary so the unpack is not const-folded.
    let td_bytes = fill(width.div_ceil(1 << bits) * 4, 0x1357_9BDF_2468_ACE0);
    let tile_data: Vec<u32> = td_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    group.bench_function(BenchmarkId::new("opt", width), |b| {
        b.iter_batched_ref(
            || row0.clone(),
            |row| cross_color_inverse_row(black_box(row), 5, bits, black_box(&tile_data)),
            criterion::BatchSize::SmallInput,
        );
    });
    group.bench_function(BenchmarkId::new("ref", width), |b| {
        b.iter_batched_ref(
            || row0.clone(),
            |row| cross_color_inverse_row_reference(black_box(row), 5, bits, black_box(&tile_data)),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

#[cfg(feature = "bench")]
criterion_group!(
    benches,
    sse_block_benchmark,
    residual_block_benchmark,
    true_motion_benchmark,
    sweep_blue_benchmark,
    cross_color_inverse_row_benchmark
);
#[cfg(feature = "bench")]
criterion_main!(benches);

/// Without `--features bench` the kernels are not exposed; the target still needs a
/// `main`, so build an empty one and let a plain `cargo bench` skip it.
#[cfg(not(feature = "bench"))]
fn main() {}
