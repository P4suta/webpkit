//! webpkit build automation (xtask pattern): golden-fixture generation and the
//! committed measurement ledgers (corpus / metrics / work).
//!
//! The conformance drift-gates are *not* here — each codec's `*-conformance`
//! crate owns its own in-crate `tests/ledger.rs` gate (run by `cargo test`), so
//! all three codecs pin their ledger the same way.
//!
//! CLI boundary: printing to stdout/stderr and exiting non-zero on
//! misconfiguration are intentional (hence the crate-level lint relaxations).
//!
//! The per-subcommand implementations live in the sibling modules
//! ([`fixtures`], [`corpus`], [`metrics`], [`work`], [`mod@bench`]),
//! sharing the [`common`], [`ledger`], and [`libwebp`] helper modules; this file
//! is just the CLI surface and the [`Task`] dispatch.
#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "xtask is a CLI tool; printing is its job"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "xtask is a boundary; panicking on misconfiguration is acceptable"
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) is our honest internal visibility; this nursery lint conflicts \
              with the rustc unreachable_pub lint that we also enable"
)]

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod bench;
mod common;
mod corpus;
mod fixtures;
mod ledger;
mod libwebp;
mod metrics;
mod work;

use metrics::MetricsAction;

/// Process-wide counting allocator, so `metrics` can measure the per-op peak
/// additional requested bytes. Registering it here needs no `unsafe`: the
/// `unsafe impl GlobalAlloc` lives in the `webpkit-alloc-count` crate, keeping
/// xtask itself `#![forbid(unsafe_code)]`.
#[global_allocator]
static GLOBAL: webpkit_alloc_count::Counting = webpkit_alloc_count::Counting::new();

#[derive(Debug, Parser)]
#[command(
    name = "xtask",
    about = "webpkit build automation",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Invoke libwebp `cwebp`/`dwebp` to (re)generate golden fixtures.
    GenFixtures,
    /// Sweep the committed image corpus for decoder/encoder regressions.
    ///
    /// Default (gate) mode enforces the hard invariants and fails if the
    /// committed `corpus/baseline.json` drifts from a fresh run. `--bless`
    /// (re)writes the baseline instead of comparing.
    CorpusSweep {
        /// Rewrite `corpus/baseline.json` (and seed the allowlist) instead of
        /// gating against the committed baseline.
        #[arg(long)]
        bless: bool,
    },
    /// Measure the shared synthetic corpus and gate `corpus/metrics.json`.
    ///
    /// Default (gate) mode fails if the committed compression-metrics ledger
    /// drifts from a fresh run; `--bless` (re)writes it instead of comparing.
    Metrics {
        /// Rewrite `corpus/metrics.json` instead of gating against it.
        #[arg(long)]
        bless: bool,
        /// After the gate/bless, print a size comparison of our deepest-effort (`l9`)
        /// encoder against libwebp `cwebp -m 6`. Printed only — it writes no
        /// file and never gates (it needs libwebp; it soft-skips when absent).
        #[arg(long)]
        vs_libwebp: bool,
        /// Instead of the synthetic ledger, print a size comparison of our deepest-effort
        /// (`l9`) encoder vs libwebp `cwebp -m 6` over the real images
        /// in this directory. With `--lossy`, instead compares our zero-knob AUTO
        /// encoder vs `cwebp -q Q` default shaping (size + PSNR + SSIM, rolled up by
        /// pixel-derived content category, no filename recorded). Print-only: it writes
        /// no file and never gates, and the path is a pure runtime argument (no image
        /// path is baked into the tool). Soft-skips when libwebp is unavailable.
        #[arg(long)]
        real: Option<PathBuf>,
        /// With `--real`, cap each image's width via cwebp `-resize <max_edge> 0`
        /// (aspect preserved); `0` disables resizing (native resolution).
        #[arg(long, default_value_t = 512)]
        max_edge: u32,
        /// Operate on the committed LOSSY ledger `corpus/metrics-lossy.json`
        /// (webpkit-lossy size / ratio / reconstruction sse / peak memory across the
        /// sample matrix x methods x qualities) instead of the lossless one. Gates
        /// by default; composes with `--bless` and `--explain`. With `--vs-libwebp`
        /// it also prints the (never-gated) `cwebp -q Q` size/PSNR comparison.
        #[arg(long)]
        lossy: bool,
        /// Print (never gate/bless) a field-level diff of the committed ledger vs a
        /// fresh run: every changed `(case, field): old -> new`, a per-field rollup,
        /// and a byte-invariance verdict (did `encoded_len`/`encoded_hash` move, or
        /// only peak memory?). The optimization loop's "did ONLY my intended field
        /// change" check, in Rust instead of `git diff | grep`.
        #[arg(long)]
        explain: bool,
    },
    /// Measure deterministic algorithmic-work counters and gate `corpus/work.json`.
    ///
    /// The third measurement plane (see `docs/benchmarking.md`): integer
    /// event-tally counters instrumented in each codec's encode hot paths, a
    /// toolchain- and profile-independent proxy for time. Default (gate) mode
    /// fails if the committed ledger drifts from a fresh run; `--bless` rewrites
    /// it. Requires the `work-count` feature (`just work` builds it).
    Work {
        /// Rewrite `corpus/work.json` instead of gating against it.
        #[arg(long)]
        bless: bool,
        /// Print (never gate/bless) a field-level diff of the committed work ledger
        /// vs a fresh run: every changed `(case, counter): old -> new` and a
        /// per-counter rollup, so an intended hot-loop reduction can be confirmed to
        /// touch ONLY its counter. The counter-plane analog of `metrics --explain`.
        #[arg(long)]
        explain: bool,
    },
    /// Print (never gate) encode/decode throughput over a directory of real
    /// images — the timing counterpart of `metrics --real`.
    ///
    /// The criterion measurement plane (see `docs/benchmarking.md`), applied to
    /// real images instead of the synthetic matrix. Print-only: it writes no repo
    /// file, the `dir` is a pure runtime argument (no image path is baked into the
    /// tool), and it soft-skips when libwebp (`cwebp`, the source-image reader) is
    /// unavailable.
    BenchReal {
        /// Directory of real images to benchmark (a pure runtime path; nothing is
        /// written into it and no path is recorded in the repo).
        dir: PathBuf,
        /// Cap each image's width via cwebp `-resize <max_edge> 0` (aspect
        /// preserved); `0` disables resizing (native resolution).
        #[arg(long, default_value_t = 512)]
        max_edge: u32,
        /// Time each op this many passes and report the fastest (min = least
        /// scheduler/cache noise).
        #[arg(long, default_value_t = 5)]
        iters: u32,
        /// Benchmark only the first N images (in sorted name order) for a fast
        /// smoke; `0` means all. The heavy deep-effort (`l9`) timing makes a small
        /// `--limit` the quick inner-loop signal.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Task::GenFixtures => fixtures::gen_fixtures(),
        Task::CorpusSweep { bless } => corpus::corpus_sweep(bless),
        Task::Metrics {
            bless,
            vs_libwebp,
            real,
            max_edge,
            lossy,
            explain,
        } => {
            // `--lossy` selects the lossy ledger; it is orthogonal to the action
            // (explain > bless > gate), so `--lossy --bless`, `--lossy --explain`
            // etc. compose. `--real` is dispatched separately inside each of
            // `metrics` (lossless) / `metrics_lossy` (lossy AUTO vs cwebp default).
            let action = if explain {
                MetricsAction::Explain
            } else if bless {
                MetricsAction::Bless
            } else {
                MetricsAction::Gate
            };
            if lossy {
                metrics::metrics_lossy(action, vs_libwebp, real, max_edge)
            } else {
                metrics::metrics(action, vs_libwebp, real, max_edge)
            }
        },
        Task::Work { bless, explain } => work::work(bless, explain),
        Task::BenchReal {
            dir,
            max_edge,
            iters,
            limit,
        } => bench::bench_real(&dir, max_edge, iters, limit),
    }
}
