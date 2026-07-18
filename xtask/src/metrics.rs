//! Compression-metrics ledgers (lossless `corpus/metrics.json` and lossy
//! `corpus/metrics-lossy.json`): per-(sample, method[, quality]) size / ratio /
//! reconstruction / peak-memory rows, byte-golden drift-gated, plus the print-only
//! `--vs-libwebp` and `--real` developer comparisons.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::bench::{RealImage, RealPrep, prepare_real_images};
use crate::common::{
    ALL_METHODS, LOSSY_METHODS, corpus_dir, fnv1a64, lossy_method_name, method_name, workspace_root,
};
use crate::fixtures::write_png;
use crate::ledger::{FieldDelta, load_ledger, print_field_deltas};
use crate::libwebp::{check_version, cwebp_bin, run_cwebp, run_cwebp_lossy};

/// One committed compression-metric row: the encoded size and integer
/// compression ratio for one synthetic sample image at one encoder method.
///
/// Every field is an integer (no float), so `serde_json::to_string_pretty`
/// renders identical bytes on every platform and the committed
/// `corpus/metrics.json` is a pure textual diff of a fresh run (exactly like
/// `corpus/baseline.json`). Field order is load-bearing: it fixes the JSON key
/// order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MetricCase {
    /// Stable sort key: `"{content}_{edge:03}/{method}"`.
    id: String,
    /// Content archetype name (e.g. `"photo"`, `"gradient"`).
    content: String,
    /// Square edge length in pixels.
    edge: u32,
    /// Encoder method label (`"l0"` | `"auto"` | `"l9"`).
    method: String,
    /// Raw RGBA byte length (`edge * edge * 4`), the ratio denominator.
    raw_len: u64,
    /// Encoded WebP byte length, the ratio numerator.
    encoded_len: u64,
    /// `encoded_len * 1000 / raw_len`, integer floor (smaller = better).
    ratio_permille: u64,
    /// FNV-1a-64 of the encoded bytes (byte-stability oracle for every method).
    encoded_hash: u64,
    /// Peak ADDITIONAL requested bytes during `webpkit::lossless::encode` — the encode
    /// working-set high-water mark. Integer and gate-safe, but toolchain-sensitive,
    /// so the ledger is blessed/gated on the pinned MSRV (see the `metrics` recipe).
    encode_peak_bytes: u64,
    /// Peak ADDITIONAL requested bytes while decoding this method's own output via
    /// `webpkit::lossless::decode_rgba` — the decode working-set high-water mark (input buffer
    /// excluded, since it predates the measurement window).
    decode_peak_bytes: u64,
}

/// The committed compression-metrics ledger, sorted by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MetricsLedger {
    /// Schema version (bump on any structural change).
    version: u32,
    /// Per-(sample, method) rows, sorted by `id`.
    cases: Vec<MetricCase>,
}

/// The methods measured at `edge`: the full [`ALL_METHODS`] set at every size.
///
/// Every method — including `l9` (the deepest forward-transform search) at
/// `edge = 512` — is gated at every size. `l9@512`'s encodes are affordable because
/// each candidate family folds to its minimum incrementally, so the peak never
/// holds every stream at once: the release ledger builds in a couple of minutes,
/// well within the metrics job's 20-minute cap. `edge` is a parameter so a
/// size-specific cap has a home without touching the call site.
const fn methods_for(_edge: u32) -> &'static [webpkit::lossless::Effort] {
    &ALL_METHODS
}

/// Encode one sample at one method and record its size, integer ratio, and the
/// peak additional requested bytes of the encode and of a decode of its output.
///
/// The `dims`/`image`/`config` inputs are built *before* the first
/// [`webpkit_alloc_count::reset_peak`], so only the encode's own allocations land
/// inside the encode measurement window; likewise the encoded `webp` predates the
/// decode window, so `decode_peak_bytes` excludes the input buffer.
fn measure_one(
    sample: &webpkit_samples::Sample,
    method: webpkit::lossless::Effort,
) -> Result<MetricCase> {
    let dims = webpkit::lossless::Dimensions::new(sample.edge, sample.edge)?;
    let image = webpkit::lossless::ImageRef::new(
        dims,
        webpkit::lossless::PixelLayout::Rgba8,
        &sample.rgba,
    )?;
    let config = webpkit::lossless::EncoderConfig::new().with_effort(method);

    webpkit_alloc_count::reset_peak();
    let webp = webpkit::lossless::encode(image, &config)?;
    let encode_peak_bytes = webpkit_alloc_count::peak_since_reset() as u64;

    let raw_len = u64::from(sample.edge) * u64::from(sample.edge) * 4;
    let encoded_len = webp.len() as u64;
    let encoded_hash = fnv1a64(&webp);

    webpkit_alloc_count::reset_peak();
    let _ = webpkit::lossless::decode_rgba(&webp)?;
    let decode_peak_bytes = webpkit_alloc_count::peak_since_reset() as u64;

    Ok(MetricCase {
        id: format!(
            "{}/{}",
            webpkit_samples::sample_id(sample.content, sample.edge),
            method_name(method)
        ),
        content: sample.content.name().to_owned(),
        edge: sample.edge,
        method: method_name(method).to_owned(),
        raw_len,
        encoded_len,
        ratio_permille: encoded_len.saturating_mul(1000) / raw_len,
        encoded_hash,
        encode_peak_bytes,
        decode_peak_bytes,
    })
}

/// Compute a fresh metrics ledger over the shared synthetic corpus.
///
/// Integer-only and tool-free, so the serialized ledger is byte-reproducible
/// for the drift gate. Cases are sorted by `id` for a deterministic diff.
fn compute_metrics() -> Result<MetricsLedger> {
    let mut cases = Vec::new();
    for sample in webpkit_samples::matrix() {
        for &method in methods_for(sample.edge) {
            cases.push(measure_one(&sample, method)?);
        }
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(MetricsLedger { version: 1, cases })
}

/// Describe the first case where a fresh metrics run diverges from the committed
/// ledger, for an actionable stale-ledger error (mirrors
/// `first_corpus_divergence`).
fn first_metrics_divergence(committed: &MetricsLedger, fresh: &MetricsLedger) -> String {
    if committed.version != fresh.version {
        return format!(
            "schema version {} (committed) != {} (fresh)",
            committed.version, fresh.version
        );
    }
    for (i, fc) in fresh.cases.iter().enumerate() {
        match committed.cases.get(i) {
            Some(cc) if cc == fc => {},
            Some(cc) if cc.id != fc.id => {
                return format!(
                    "case #{i}: committed id `{}` != fresh id `{}`",
                    cc.id, fc.id
                );
            },
            Some(_) => return format!("case `{}` values changed", fc.id),
            None => return format!("case `{}` is new (not in committed ledger)", fc.id),
        }
    }
    if let Some(extra) = committed.cases.get(fresh.cases.len()) {
        return format!("committed ledger has extra case `{}`", extra.id);
    }
    "textual formatting only (no structural difference)".to_owned()
}

/// Field-level diff of the compression-metrics ledger, with the byte-invariance
/// verdict the optimization loop turns on: a perf change may re-bless only when
/// `encoded_len`/`encoded_hash` are unchanged (output bytes fixed) and just the
/// peak-memory columns moved.
fn explain_metrics(committed: &MetricsLedger, fresh: &MetricsLedger) {
    println!("metrics diff (committed -> fresh):");
    let by_id: std::collections::BTreeMap<&str, &MetricCase> =
        committed.cases.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut deltas: Vec<FieldDelta> = Vec::new();
    for fc in &fresh.cases {
        let Some(cc) = by_id.get(fc.id.as_str()) else {
            println!("  case `{}` is new (not in committed ledger)", fc.id);
            continue;
        };
        for (field, old, new) in [
            ("encoded_len", cc.encoded_len, fc.encoded_len),
            ("ratio_permille", cc.ratio_permille, fc.ratio_permille),
            ("encoded_hash", cc.encoded_hash, fc.encoded_hash),
            (
                "encode_peak_bytes",
                cc.encode_peak_bytes,
                fc.encode_peak_bytes,
            ),
            (
                "decode_peak_bytes",
                cc.decode_peak_bytes,
                fc.decode_peak_bytes,
            ),
        ] {
            if old != new {
                deltas.push(FieldDelta {
                    case: fc.id.clone(),
                    field: field.to_owned(),
                    old,
                    new,
                });
            }
        }
    }
    print_field_deltas(&deltas);
    let bytes_moved = deltas
        .iter()
        .any(|d| d.field == "encoded_len" || d.field == "encoded_hash");
    if bytes_moved {
        println!(
            "  VERDICT: OUTPUT BYTES CHANGED — encoded_len/encoded_hash moved (NOT byte-invariant)"
        );
    } else {
        println!(
            "  VERDICT: byte-invariant — encoded_len & encoded_hash identical; only peak/ratio moved"
        );
    }
}

/// The mutually-exclusive mode of the `metrics` subcommand (aside from `--real`,
/// which is handled separately as it needs the directory argument). Modeled as an
/// enum rather than a fistful of bools.
#[derive(Clone, Copy)]
pub(crate) enum MetricsAction {
    /// Gate the committed ledger against a fresh run (default).
    Gate,
    /// Rewrite the committed ledger from a fresh run.
    Bless,
    /// Print a field-level committed-vs-fresh diff (no gate/bless).
    Explain,
}

pub(crate) fn metrics(
    action: MetricsAction,
    vs_libwebp: bool,
    real: Option<PathBuf>,
    max_edge: u32,
) -> Result<()> {
    // `--real <dir>` runs ONLY the real-image comparison: it never touches the
    // gated ledger (no gate, no bless), writes no repo file, and soft-skips when
    // libwebp is absent. Returning here also keeps it off the expensive synthetic
    // sweep below.
    if let Some(real_dir) = real {
        return compare_vs_libwebp_real(&real_dir, max_edge);
    }

    let root = workspace_root()?;
    let dir = corpus_dir(&root);
    let metrics_path = dir.join("metrics.json");

    // `--explain` prints a field-level committed-vs-fresh diff and returns without
    // gating or blessing — the loop's byte-invariance inspection.
    if matches!(action, MetricsAction::Explain) {
        let committed = load_ledger::<MetricsLedger>(&metrics_path)?;
        let fresh = compute_metrics()?;
        explain_metrics(&committed, &fresh);
        return Ok(());
    }

    let fresh = compute_metrics()?;
    let fresh_json = format!("{}\n", serde_json::to_string_pretty(&fresh)?);

    if matches!(action, MetricsAction::Bless) {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&metrics_path, &fresh_json)
            .with_context(|| format!("writing {}", metrics_path.display()))?;
        println!(
            "metrics: blessed {} case(s) -> {}",
            fresh.cases.len(),
            metrics_path.display()
        );
    } else {
        // Gate mode: committed == fresh (byte-golden drift), like `corpus_sweep`.
        let committed = match std::fs::read_to_string(&metrics_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "no metrics ledger at {} — run `cargo xtask metrics --bless` and commit it first",
                metrics_path.display()
            ),
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("reading {}", metrics_path.display()))
                );
            },
        };

        if committed != fresh_json {
            let detail = serde_json::from_str::<MetricsLedger>(&committed).map_or_else(
                |_| "committed ledger is not valid JSON".to_owned(),
                |committed_ledger| first_metrics_divergence(&committed_ledger, &fresh),
            );
            bail!(
                "metrics: ledger at {} is stale (first divergence: {}). \
                 Run `cargo xtask metrics --bless` and commit the updated file.",
                metrics_path.display(),
                detail
            );
        }

        println!("metrics: {} case(s) stable", fresh.cases.len());
    }

    // Printed-only libwebp size comparison. Runs AFTER the gate/bless above and
    // is fully independent of the committed ledger: it writes no file and never
    // fails the command (it soft-skips when libwebp is unavailable).
    if vs_libwebp {
        compare_vs_libwebp()?;
    }
    Ok(())
}

/// Print (never gate) a size comparison of our deepest-effort (`l9`) encoder against
/// libwebp `cwebp -m 6 -q 100` over the shared synthetic corpus.
///
/// This is a developer aid, not a committed artifact: it writes NO file to the
/// repo and never fails the command. It *needs* libwebp (`cwebp`) — which the
/// deterministic ledger deliberately does not — so it can never be part of the
/// gated `corpus/metrics.json`. When `cwebp` is absent or the wrong version the
/// probe soft-skips (prints a notice, returns `Ok`), exactly like the not-found
/// handling elsewhere in this tool.
///
/// `Best` is capped to `edge <= 256` here — this is a local iteration cap for the
/// per-image `cwebp` shell-out (the gated ledger itself now includes `Best@512`;
/// see [`methods_for`]). Percentages are printed via integer permille math (no
/// lossy float casts), even though this output is not gated.
fn compare_vs_libwebp() -> Result<()> {
    let cwebp = cwebp_bin();
    // Probe cwebp; soft-skip (never fail) when it is absent or the wrong version.
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!("metrics --vs-libwebp: skipped ({e})");
        return Ok(());
    }

    println!();
    println!("ours(l9) vs cwebp -m 6 -q 100 (printed only, NOT gated):");
    println!(
        "  {:<16} {:>12} {:>12} {:>12}",
        "id", "ours bytes", "cwebp bytes", "ours/cwebp"
    );

    let tmp = tempfile::tempdir().context("creating tempdir for --vs-libwebp comparison")?;
    let src_png = tmp.path().join("src.png");
    let out_webp = tmp.path().join("out.webp");
    let config =
        webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::level(9));

    for sample in webpkit_samples::matrix() {
        // Cap Best to edge <= 256 for time, mirroring the ledger's Best@512 cap.
        if sample.edge > 256 {
            continue;
        }
        let id = webpkit_samples::sample_id(sample.content, sample.edge);

        let dims = webpkit::lossless::Dimensions::new(sample.edge, sample.edge)?;
        let image = webpkit::lossless::ImageRef::new(
            dims,
            webpkit::lossless::PixelLayout::Rgba8,
            &sample.rgba,
        )?;
        let ours = webpkit::lossless::encode(image, &config)?.len() as u64;

        write_png(&src_png, sample.edge, sample.edge, &sample.rgba)?;
        run_cwebp(&cwebp, &src_png, &out_webp, None)?;
        let theirs = std::fs::metadata(&out_webp)
            .with_context(|| format!("statting {}", out_webp.display()))?
            .len();

        // (ours / theirs) as a percentage in tenths, integer-only: 1234 => 123.4%.
        // `checked_div` guards the (impossible for cwebp) zero-size output.
        let pct_tenths = ours.saturating_mul(1000).checked_div(theirs).unwrap_or(0);
        let pct = format!("{}.{}%", pct_tenths / 10, pct_tenths % 10);
        println!("  {id:<16} {ours:>12} {theirs:>12} {pct:>12}");
    }
    Ok(())
}

/// Print (never gate) a size comparison of our deepest-effort (`l9`) encoder against
/// libwebp `cwebp -m 6 -q 100` over the *real* images in a caller-supplied
/// directory `real`.
///
/// PRIVACY: `real` is a pure runtime argument — no image path or filename is
/// baked into this tool. The comparison is strictly print-only: every temporary
/// file lives inside a `tempfile::tempdir()` that is dropped on return, so
/// nothing is written into the repository (the committed `corpus/metrics.json`
/// is never touched by this path). It needs libwebp (`cwebp`); when that is
/// absent or the wrong version it soft-skips (prints a notice, returns `Ok`),
/// exactly like the synthetic `--vs-libwebp` path.
///
/// Per file (subdirectories are skipped; files are visited in sorted name order
/// for stable output): cwebp encodes it losslessly — first capping the width via
/// `-resize <max_edge> 0` (aspect preserved) when `max_edge > 0`, else at native
/// resolution — then our decoder reads the exact resized pixels back so both
/// encoders see byte-identical input. A file cwebp cannot read is reported as a
/// `skipped` row and never aborts the run. The ratio column is integer permille
/// (`ours * 1000 / cwebp`, mirroring the ledger); below 1000 means our stream is
/// the smaller of the two.
fn compare_vs_libwebp_real(real: &Path, max_edge: u32) -> Result<()> {
    let cwebp = cwebp_bin();
    // Probe cwebp; soft-skip (never fail) when it is absent or the wrong version.
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!("metrics --real: skipped ({e})");
        return Ok(());
    }

    println!();
    println!("ours(l9) vs cwebp -m 6 -q 100 on real images (printed only, NOT gated):");
    match (max_edge > 0).then_some(max_edge) {
        Some(width) => println!("  input width capped to {width}px by cwebp (aspect preserved)"),
        None => println!("  native resolution (no resize)"),
    }
    println!("  ratio permille = ours * 1000 / cwebp; below 1000 means our stream is smaller");
    println!(
        "  {:<32} {:>11} {:>12} {:>12} {:>14}",
        "file", "dims", "ours bytes", "cwebp bytes", "ratio permille"
    );

    // Shared ARGB-prep: cwebp encodes each source (optionally width-capped), our
    // decoder reads the exact resized pixels back, so both encoders see identical
    // input. Ordering/skips are preserved for stable rows.
    let prepared = prepare_real_images(&cwebp, real, max_edge)?;
    let config =
        webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::level(9));

    let mut total_ours = 0u64;
    let mut total_cwebp = 0u64;
    let mut compared = 0usize;

    for item in &prepared {
        let RealImage {
            name,
            dims,
            rgba,
            cwebp_size,
        } = match item {
            // A per-file cwebp failure (e.g. a non-image file) is a `skipped` row,
            // never an abort of the whole run.
            RealPrep::Skipped { name } => {
                println!(
                    "  {name:<32} {:>11} {:>12} {:>12} {:>14}",
                    "-", "-", "-", "skipped"
                );
                continue;
            },
            RealPrep::Ready(image) => image,
        };

        let image =
            webpkit::lossless::ImageRef::new(*dims, webpkit::lossless::PixelLayout::Rgba8, rgba)?;
        let our_size = webpkit::lossless::encode(image, &config)?.len() as u64;

        let dims_label = format!("{}x{}", dims.width(), dims.height());
        let permille = our_size
            .saturating_mul(1000)
            .checked_div(*cwebp_size)
            .unwrap_or(0);
        println!("  {name:<32} {dims_label:>11} {our_size:>12} {cwebp_size:>12} {permille:>14}");

        total_ours += our_size;
        total_cwebp += cwebp_size;
        compared += 1;
    }

    if compared == 0 {
        println!("  (no comparable images in {})", real.display());
        return Ok(());
    }

    let total_permille = total_ours
        .saturating_mul(1000)
        .checked_div(total_cwebp)
        .unwrap_or(0);
    let total_label = format!("{compared} img");
    println!(
        "  {:<32} {total_label:>11} {total_ours:>12} {total_cwebp:>12} {total_permille:>14}",
        "TOTAL"
    );
    Ok(())
}

/// The qualities each sample is encoded at for the lossy table.
const LOSSY_QUALITIES: [u8; 3] = [50, 75, 90];

/// `Best`'s trellis + i4x4 search at edge 512 dominates the ledger's runtime, so
/// it is capped to `edge <= 256` — mirroring the work ledger's `WORK_BEST_MAX_EDGE`
/// and the `--vs-libwebp` cap. `Fast`/`Balanced` are measured at every size.
const LOSSY_METRICS_BEST_MAX_EDGE: u32 = 256;

/// One committed lossy compression-metric row: encoded size, integer ratio and a
/// deterministic reconstruction-quality field for one sample at one `(method,
/// quality)`.
///
/// Every field is an integer (the quality is `sse`, not float dB), so
/// `serde_json::to_string_pretty` renders identical bytes on every platform and
/// `corpus/metrics-lossy.json` is a pure textual diff of a fresh run — exactly
/// like `metrics.json`. Field order is load-bearing (it fixes the JSON key order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LossyMetricCase {
    /// Stable sort key: `"{content}_{edge:03}/{method}/q{quality:03}"`.
    id: String,
    /// Content archetype name (e.g. `"photo"`, `"gradient"`).
    content: String,
    /// Square edge length in pixels.
    edge: u32,
    /// Encoder method label (`"l0"` | `"auto"` | `"l9"`).
    method: String,
    /// Encoder quality in `0..=100`.
    quality: u8,
    /// Raw RGB byte length (`edge * edge * 3`), the ratio denominator (the lossy
    /// codec drops alpha, so RGB is the natural raw basis).
    raw_len: u64,
    /// Encoded `VP8 ` payload byte length, the ratio numerator.
    encoded_len: u64,
    /// `encoded_len * 1000 / raw_len`, integer floor (smaller = better).
    ratio_permille: u64,
    /// FNV-1a-64 of the encoded payload — the encoder byte-stability oracle: any
    /// change to the emitted stream shows here even if the size is unchanged.
    encoded_hash: u64,
    /// Integer sum of squared error (RGB) of OUR decode of this payload vs the
    /// source — the deterministic reconstruction-quality field. For a pure speed
    /// refactor both `encoded_hash` and `sse` stay fixed.
    sse: u64,
    /// Peak ADDITIONAL requested bytes during `webpkit::lossy::encode_vp8` — the encode
    /// working-set high-water mark (toolchain-sensitive, so the ledger is
    /// blessed/gated on the pinned MSRV, like `metrics.json`).
    encode_peak_bytes: u64,
    /// Peak ADDITIONAL requested bytes while decoding this payload via
    /// `webpkit::lossy::decode` (input buffer excluded — it predates the window).
    decode_peak_bytes: u64,
}

/// The committed lossy compression-metrics ledger, sorted by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LossyMetricsLedger {
    /// Schema version (bump on any structural change).
    version: u32,
    /// Per-(sample, method, quality) rows, sorted by `id`.
    cases: Vec<LossyMetricCase>,
}

/// Encode one sample at one `(method, quality)` and record its size, integer
/// ratio, reconstruction `sse`, and the encode/decode peak-allocation marks —
/// the lossy analog of [`measure_one`].
fn measure_one_lossy(
    sample: &webpkit_samples::Sample,
    method: webpkit::lossy::Effort,
    quality: u8,
) -> Result<LossyMetricCase> {
    let dims = webpkit::lossy::Dimensions::new(sample.edge, sample.edge)?;
    let image =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &sample.rgba)?;
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(quality)
        .with_effort(method);

    webpkit_alloc_count::reset_peak();
    let (_dims, payload) = webpkit::lossy::encode_vp8(image, &cfg)?;
    let encode_peak_bytes = webpkit_alloc_count::peak_since_reset() as u64;

    let raw_len = u64::from(sample.edge) * u64::from(sample.edge) * 3;
    let encoded_len = payload.len() as u64;
    let encoded_hash = fnv1a64(&payload);

    webpkit_alloc_count::reset_peak();
    let decoded = webpkit::lossy::decode(&payload)?;
    let decode_peak_bytes = webpkit_alloc_count::peak_since_reset() as u64;
    let sse = sse_rgb(&sample.rgba, decoded.as_bytes());

    Ok(LossyMetricCase {
        id: format!(
            "{}/{}/q{quality:03}",
            webpkit_samples::sample_id(sample.content, sample.edge),
            lossy_method_name(method),
        ),
        content: sample.content.name().to_owned(),
        edge: sample.edge,
        method: lossy_method_name(method).to_owned(),
        quality,
        raw_len,
        encoded_len,
        ratio_permille: encoded_len.saturating_mul(1000) / raw_len,
        encoded_hash,
        sse,
        encode_peak_bytes,
        decode_peak_bytes,
    })
}

/// Compute a fresh lossy metrics ledger over the shared synthetic corpus crossed
/// with [`LOSSY_METHODS`] x [`LOSSY_QUALITIES`] (`l9` capped to
/// [`LOSSY_METRICS_BEST_MAX_EDGE`]). Integer-only, so it is byte-reproducible for
/// the drift gate. Cases are sorted by `id` for a deterministic diff.
fn compute_lossy_metrics() -> Result<LossyMetricsLedger> {
    let mut cases = Vec::new();
    for sample in webpkit_samples::matrix() {
        for &method in &LOSSY_METHODS {
            if method == webpkit::lossy::Effort::level(9)
                && sample.edge > LOSSY_METRICS_BEST_MAX_EDGE
            {
                continue;
            }
            for &quality in &LOSSY_QUALITIES {
                cases.push(measure_one_lossy(&sample, method, quality)?);
            }
        }
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(LossyMetricsLedger { version: 1, cases })
}

/// Describe the first case where a fresh lossy metrics run diverges from the
/// committed ledger (mirrors [`first_metrics_divergence`]).
fn first_lossy_metrics_divergence(
    committed: &LossyMetricsLedger,
    fresh: &LossyMetricsLedger,
) -> String {
    if committed.version != fresh.version {
        return format!(
            "schema version {} (committed) != {} (fresh)",
            committed.version, fresh.version
        );
    }
    for (i, fc) in fresh.cases.iter().enumerate() {
        match committed.cases.get(i) {
            Some(cc) if cc == fc => {},
            Some(cc) if cc.id != fc.id => {
                return format!(
                    "case #{i}: committed id `{}` != fresh id `{}`",
                    cc.id, fc.id
                );
            },
            Some(cc) => {
                let what = if cc.encoded_hash != fc.encoded_hash {
                    "encoded bytes"
                } else if cc.sse != fc.sse {
                    "reconstruction sse"
                } else {
                    "size/ratio/peak"
                };
                return format!("case `{}` changed ({what})", fc.id);
            },
            None => return format!("case `{}` is new (not in committed ledger)", fc.id),
        }
    }
    if let Some(extra) = committed.cases.get(fresh.cases.len()) {
        return format!("committed ledger has extra case `{}`", extra.id);
    }
    "textual formatting only (no structural difference)".to_owned()
}

/// Field-level diff of the lossy metrics ledger (mirrors [`explain_metrics`]),
/// ending with a byte-invariance verdict: for a pure speed refactor no
/// `encoded_hash`/`encoded_len`/`sse` may move (only peak memory may).
fn explain_lossy_metrics(committed: &LossyMetricsLedger, fresh: &LossyMetricsLedger) {
    println!("lossy metrics diff (committed -> fresh):");
    let by_id: std::collections::BTreeMap<&str, &LossyMetricCase> =
        committed.cases.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut deltas: Vec<FieldDelta> = Vec::new();
    let mut bytes_moved = false;
    for fc in &fresh.cases {
        let Some(cc) = by_id.get(fc.id.as_str()) else {
            println!("  case `{}` is new (not in committed ledger)", fc.id);
            continue;
        };
        let fields = [
            ("encoded_len", cc.encoded_len, fc.encoded_len),
            ("ratio_permille", cc.ratio_permille, fc.ratio_permille),
            ("encoded_hash", cc.encoded_hash, fc.encoded_hash),
            ("sse", cc.sse, fc.sse),
            (
                "encode_peak_bytes",
                cc.encode_peak_bytes,
                fc.encode_peak_bytes,
            ),
            (
                "decode_peak_bytes",
                cc.decode_peak_bytes,
                fc.decode_peak_bytes,
            ),
        ];
        for (field, old, new) in fields {
            if old != new {
                if matches!(field, "encoded_len" | "encoded_hash" | "sse") {
                    bytes_moved = true;
                }
                deltas.push(FieldDelta {
                    case: fc.id.clone(),
                    field: field.to_owned(),
                    old,
                    new,
                });
            }
        }
    }
    print_field_deltas(&deltas);
    if deltas.is_empty() {
        println!("  VERDICT: identical ledgers");
    } else if bytes_moved {
        println!("  VERDICT: OUTPUT changed — encoded bytes and/or reconstruction sse moved");
    } else {
        println!("  VERDICT: byte-invariant — only peak memory moved (pure speed refactor)");
    }
}

/// Peak signal-to-noise ratio (dB) over the RGB channels of two RGBA buffers.
///
/// Byte-for-byte the formula in `crates/webpkit/tests/encode_lossy.rs`: the codec
/// crate forbids floating point (bit-determinism), but xtask is a CLI boundary,
/// not the codec, so the `f64` PSNR math is allowed here (as in that test crate).
/// `99.0` denotes an identical pair (the squared-error sum is integer-valued).
fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            se = d.mul_add(d, se);
            n += 1.0;
        }
    }
    if se < 1.0 {
        return 99.0;
    }
    let mse = se / n;
    10.0 * (255.0 * 255.0 / mse).log10()
}

/// Integer sum of squared error over the RGB channels of two RGBA buffers — the
/// deterministic (float-free) quality field of the committed lossy ledger.
///
/// Unlike [`psnr_rgb`]'s `f64` dB (a CLI convenience that renders differently
/// across platforms and so cannot be committed), this is a pure integer: every
/// per-channel difference is in `[-255, 255]`, its square in `[0, 65025]`, and
/// the sum over any committed sample stays far inside `u64`. The human dB is
/// derivable from `sse` and the pixel count, so it stays in the print-only
/// `--vs-libwebp` aid. `alpha` is ignored (the lossy codec drops it).
fn sse_rgb(a: &[u8], b: &[u8]) -> u64 {
    let mut se = 0u64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = (i64::from(pa[c]) - i64::from(pb[c])).unsigned_abs();
            se += d * d;
        }
    }
    se
}

/// Extract the payload of the first `VP8 ` chunk from a RIFF/WEBP container, so a
/// (container-wrapped) `cwebp` lossy file can be decoded by `webpkit::lossy::decode`,
/// which consumes a bare VP8 key-frame payload. Chunks are padded to even size.
fn extract_vp8_chunk(webp: &[u8]) -> Result<Vec<u8>> {
    if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        bail!("cwebp output is not a RIFF/WEBP container");
    }
    let mut off = 12;
    while off + 8 <= webp.len() {
        let size = u32::from_le_bytes([webp[off + 4], webp[off + 5], webp[off + 6], webp[off + 7]])
            as usize;
        let start = off + 8;
        let end = start.checked_add(size).context("VP8 chunk size overflow")?;
        if end > webp.len() {
            bail!("truncated WEBP chunk");
        }
        if &webp[off..off + 4] == b"VP8 " {
            return Ok(webp[start..end].to_vec());
        }
        off = end + (size & 1); // pad to the next even offset
    }
    bail!("no `VP8 ` chunk in cwebp output (unexpected extended container)")
}

/// Gate / bless / explain the committed lossy size+quality ledger
/// `corpus/metrics-lossy.json`, then — when `vs_libwebp` — print the (never-gated)
/// `cwebp -q Q` size/PSNR comparison.
///
/// This is the lossy analog of [`metrics`]: the ledger is integer-only (its
/// quality field is `sse`, not float dB) and byte-golden drift-gated exactly like
/// `corpus/metrics.json`, blessed on the pinned MSRV (`just metrics-lossy-bless`).
/// `--vs-libwebp` stays a print-only developer aid ([`compare_lossy_vs_libwebp`]);
/// `--real <dir>` runs the print-only AUTO-vs-cwebp real-image sweep
/// ([`compare_lossy_vs_libwebp_real`]) and returns without touching the ledger.
pub(crate) fn metrics_lossy(
    action: MetricsAction,
    vs_libwebp: bool,
    real: Option<PathBuf>,
    max_edge: u32,
) -> Result<()> {
    // `--real <dir>` runs ONLY the real-image AUTO-vs-cwebp comparison: it never
    // touches the gated ledger, writes no repo file, and soft-skips when libwebp is
    // absent (mirrors the lossless `metrics` path).
    if let Some(real_dir) = real {
        return compare_lossy_vs_libwebp_real(&real_dir, max_edge);
    }
    let root = workspace_root()?;
    let dir = corpus_dir(&root);
    let metrics_path = dir.join("metrics-lossy.json");

    match action {
        MetricsAction::Explain => {
            let committed = load_ledger::<LossyMetricsLedger>(&metrics_path)?;
            let fresh = compute_lossy_metrics()?;
            explain_lossy_metrics(&committed, &fresh);
            return Ok(());
        },
        MetricsAction::Bless => {
            let fresh = compute_lossy_metrics()?;
            let fresh_json = format!("{}\n", serde_json::to_string_pretty(&fresh)?);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            std::fs::write(&metrics_path, &fresh_json)
                .with_context(|| format!("writing {}", metrics_path.display()))?;
            println!(
                "metrics-lossy: blessed {} case(s) -> {}",
                fresh.cases.len(),
                metrics_path.display()
            );
        },
        MetricsAction::Gate => {
            let fresh = compute_lossy_metrics()?;
            let fresh_json = format!("{}\n", serde_json::to_string_pretty(&fresh)?);
            let committed = match std::fs::read_to_string(&metrics_path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                    "no lossy metrics ledger at {} — run `just metrics-lossy-bless` and commit it first",
                    metrics_path.display()
                ),
                Err(e) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("reading {}", metrics_path.display())));
                },
            };
            if committed != fresh_json {
                let detail = serde_json::from_str::<LossyMetricsLedger>(&committed).map_or_else(
                    |_| "committed ledger is not valid JSON".to_owned(),
                    |committed_ledger| first_lossy_metrics_divergence(&committed_ledger, &fresh),
                );
                bail!(
                    "metrics-lossy: ledger at {} is stale (first divergence: {}). \
                     Run `just metrics-lossy-bless` and commit the updated file.",
                    metrics_path.display(),
                    detail
                );
            }
            println!("metrics-lossy: {} case(s) stable", fresh.cases.len());
        },
    }

    if vs_libwebp {
        compare_lossy_vs_libwebp()?;
    }
    Ok(())
}

/// Print (never gate) a lossy size + PSNR comparison of our deepest-effort (`l9`)
/// encoder against libwebp `cwebp -q Q` over the shared sample matrix.
///
/// Both encoders' raw `VP8 ` payloads are decoded by *our* decoder, so the PSNR
/// is apples-to-apples (identical YUV→RGB conversion on both sides) and the size
/// column compares the VP8 payloads directly (container overhead excluded).
/// `cwebp` is invoked with `-noalpha` so it emits a bare (non-extended) container,
/// matching our alpha-dropping encoder. Needs libwebp; soft-skips (prints a
/// notice, returns `Ok`) when `cwebp` is absent or the wrong version.
fn compare_lossy_vs_libwebp() -> Result<()> {
    let cwebp = cwebp_bin();
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!();
        println!("metrics --lossy --vs-libwebp: skipped ({e})");
        return Ok(());
    }

    println!();
    println!("ours(l9) vs cwebp -q Q (lossy, printed only, NOT gated):");
    println!("  bytes = VP8 payload; both decoded by our decoder for an apples-to-apples PSNR");
    println!(
        "  {:<14} {:>3} {:>10} {:>8} {:>11} {:>9}",
        "sample", "q", "ours bytes", "ours dB", "cwebp bytes", "cwebp dB"
    );

    let tmp = tempfile::tempdir().context("creating tempdir for lossy --vs-libwebp comparison")?;
    let src_png = tmp.path().join("src.png");
    let out_webp = tmp.path().join("out.webp");

    for sample in webpkit_samples::matrix() {
        let id = webpkit_samples::sample_id(sample.content, sample.edge);
        let dims = webpkit::lossy::Dimensions::new(sample.edge, sample.edge)?;
        write_png(&src_png, sample.edge, sample.edge, &sample.rgba)?;

        for &q in &LOSSY_QUALITIES {
            let image = webpkit::lossy::ImageRef::new(
                dims,
                webpkit::lossy::PixelLayout::Rgba8,
                &sample.rgba,
            )?;
            let cfg = webpkit::lossy::LossyConfig::new()
                .with_quality(q)
                .with_effort(webpkit::lossy::Effort::level(9));
            let (_dims, payload) = webpkit::lossy::encode_vp8(image, &cfg)?;
            let ours_bytes = payload.len() as u64;
            let ours_psnr = psnr_rgb(&sample.rgba, webpkit::lossy::decode(&payload)?.as_bytes());

            run_cwebp_lossy(&cwebp, &src_png, &out_webp, q)?;
            let cwebp_file = std::fs::read(&out_webp)
                .with_context(|| format!("reading {}", out_webp.display()))?;
            let cwebp_vp8 = extract_vp8_chunk(&cwebp_file)?;
            let cwebp_bytes = cwebp_vp8.len() as u64;
            let cwebp_psnr = psnr_rgb(&sample.rgba, webpkit::lossy::decode(&cwebp_vp8)?.as_bytes());

            println!(
                "  {id:<14} {q:>3} {ours_bytes:>10} {ours_psnr:>8.2} {cwebp_bytes:>11} {cwebp_psnr:>9.2}"
            );
        }
    }
    Ok(())
}

/// A real image's content category, derived from its PIXELS (never its filename),
/// so the real-image sweep can roll results up without recording any file name —
/// the private corpus stays private by construction (see [`classify_real`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RealCategory {
    /// Opaque, high distinct-color content — photographs and scans.
    Photo,
    /// Opaque, low distinct-color content — line art, charts, flat graphics.
    Graphic,
    /// Any pixel with alpha `< 255` — icons and transparent exports.
    Transparent,
}

impl RealCategory {
    /// The categories in printed/aggregation order.
    const ALL: [Self; 3] = [Self::Photo, Self::Graphic, Self::Transparent];

    /// The row label used in the printed rollup.
    const fn label(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Graphic => "graphic",
            Self::Transparent => "transparent",
        }
    }

    /// Dense index into the per-category accumulator array.
    const fn idx(self) -> usize {
        match self {
            Self::Photo => 0,
            Self::Graphic => 1,
            Self::Transparent => 2,
        }
    }
}

/// Distinct-color threshold separating flat graphics/line-art from photographic
/// content (opaque images only). A 512-edge photograph carries tens of thousands
/// of distinct RGB triples; clean vector-style exports stay in the low thousands.
const GRAPHIC_MAX_COLORS: usize = 8192;

/// Classify a decoded RGBA buffer into a [`RealCategory`] from its pixels alone —
/// transparency first, then distinct-color count. No filename is ever inspected,
/// which is what lets the sweep aggregate a private corpus without naming it.
fn classify_real(rgba: &[u8]) -> RealCategory {
    if rgba.chunks_exact(4).any(|px| px[3] < 255) {
        return RealCategory::Transparent;
    }
    let mut colors = std::collections::BTreeSet::new();
    for px in rgba.chunks_exact(4) {
        colors.insert([px[0], px[1], px[2]]);
        if colors.len() > GRAPHIC_MAX_COLORS {
            return RealCategory::Photo;
        }
    }
    RealCategory::Graphic
}

/// Mean structural similarity (MSSIM) over non-overlapping 8×8 luma windows of two
/// RGBA buffers; `1.0` for an identical pair, lower as structure degrades.
///
/// Like [`psnr_rgb`] this is an `f64` print-only aid — xtask is a CLI boundary, not
/// the float-free codec, and nothing here is committed to a gated ledger. Luma is
/// BT.601; alpha is ignored (the lossy codec drops it); pixels beyond the last full
/// 8×8 block (a partial right/bottom strip) are not windowed. An image smaller than
/// one window scores `1.0` (no measurable structural loss).
fn ssim_rgb(a: &[u8], b: &[u8], width: usize, height: usize) -> f64 {
    // Stabilizers from the standard SSIM definition at 8-bit dynamic range L = 255:
    // C1 = (0.01 L)^2, C2 = (0.03 L)^2.
    const C1: f64 = 6.5025;
    const C2: f64 = 58.5225;
    let luma = |px: &[u8]| {
        0.299f64.mul_add(
            f64::from(px[0]),
            0.587f64.mul_add(f64::from(px[1]), 0.114 * f64::from(px[2])),
        )
    };
    let ya: Vec<f64> = a.chunks_exact(4).map(&luma).collect();
    let yb: Vec<f64> = b.chunks_exact(4).map(&luma).collect();
    if width < 8 || height < 8 || ya.len() < width * height || yb.len() < width * height {
        return 1.0;
    }
    let (mut acc, mut windows) = (0.0f64, 0u32);
    let mut wy = 0;
    while wy + 8 <= height {
        let mut wx = 0;
        while wx + 8 <= width {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for dy in 0..8 {
                let row = (wy + dy) * width + wx;
                for dx in 0..8 {
                    let (va, vb) = (ya[row + dx], yb[row + dx]);
                    sa += va;
                    sb += vb;
                    saa = va.mul_add(va, saa);
                    sbb = vb.mul_add(vb, sbb);
                    sab = va.mul_add(vb, sab);
                }
            }
            let n = 64.0;
            let (ma, mb) = (sa / n, sb / n);
            // variance/covariance as E[x^2] - E[x]^2, expressed via mul_add.
            let va = ma.mul_add(-ma, saa / n);
            let vb = mb.mul_add(-mb, sbb / n);
            let cov = ma.mul_add(-mb, sab / n);
            let num = (2.0 * ma).mul_add(mb, C1) * 2.0f64.mul_add(cov, C2);
            let den = mb.mul_add(mb, ma.mul_add(ma, C1)) * (va + vb + C2);
            acc += num / den;
            windows += 1;
            wx += 8;
        }
        wy += 8;
    }
    if windows == 0 {
        1.0
    } else {
        acc / f64::from(windows)
    }
}

/// Running per-(quality, category) totals for the real-image AUTO-vs-cwebp sweep.
/// Sizes are exact byte sums; the quality fields are dB / SSIM sums averaged by `n`
/// at print time.
#[derive(Default, Clone, Copy)]
struct CategoryAcc {
    /// Images folded into this bucket.
    n: u32,
    /// Summed webpkit-AUTO `VP8 ` payload bytes.
    ours_bytes: u64,
    /// Summed `cwebp -q Q` `VP8 ` payload bytes.
    cwebp_bytes: u64,
    /// Summed webpkit-AUTO reconstruction PSNR (dB).
    ours_psnr: f64,
    /// Summed `cwebp` reconstruction PSNR (dB).
    cwebp_psnr: f64,
    /// Summed webpkit-AUTO reconstruction MSSIM.
    ours_ssim: f64,
    /// Summed `cwebp` reconstruction MSSIM.
    cwebp_ssim: f64,
}

impl CategoryAcc {
    /// Fold this accumulator into `other` (the per-quality `ALL` rollup).
    fn add_into(self, other: &mut Self) {
        other.n += self.n;
        other.ours_bytes += self.ours_bytes;
        other.cwebp_bytes += self.cwebp_bytes;
        other.ours_psnr += self.ours_psnr;
        other.cwebp_psnr += self.cwebp_psnr;
        other.ours_ssim += self.ours_ssim;
        other.cwebp_ssim += self.cwebp_ssim;
    }

    /// Print one rollup row (`label` names the bucket). No-op for an empty bucket.
    fn print_row(self, label: &str) {
        if self.n == 0 {
            return;
        }
        let n = f64::from(self.n);
        let (o_db, c_db) = (self.ours_psnr / n, self.cwebp_psnr / n);
        let (o_ss, c_ss) = (self.ours_ssim / n, self.cwebp_ssim / n);
        let size_tenths = self
            .ours_bytes
            .saturating_mul(1000)
            .checked_div(self.cwebp_bytes)
            .unwrap_or(0);
        let size_pct = format!("{}.{}", size_tenths / 10, size_tenths % 10);
        println!(
            "  {label:<12} {:>3} {:>12} {:>12} {size_pct:>7} {o_db:>7.2} {c_db:>6.2} {:>+6.2} {o_ss:>8.4} {c_ss:>7.4} {:>+8.4}",
            self.n,
            self.ours_bytes,
            self.cwebp_bytes,
            o_db - c_db,
            o_ss - c_ss,
        );
    }
}

/// Print (never gate) a lossy size + PSNR + SSIM comparison of our zero-knob AUTO
/// encoder against libwebp `cwebp -q Q` (default shaping) over the real images in
/// `real`, rolled up by pixel-derived content category.
///
/// PRIVACY: `real` is a pure runtime argument; results are aggregated by
/// [`RealCategory`] (classified from pixels, never the filename), and no file name
/// is printed or recorded. Every temp file lives in a dropped `tempfile::tempdir()`,
/// so nothing is written into the repo. Needs libwebp (`cwebp`); soft-skips when it
/// is absent or the wrong version.
///
/// This is the AUTO analog of [`compare_lossy_vs_libwebp`] (which runs `l9` over the
/// synthetic matrix): it exercises the exact zero-knob default a user ships, so it is
/// the measurement basis for the AUTO default-shaping tune (issue #32). Both payloads
/// are decoded by OUR decoder for an apples-to-apples PSNR/SSIM; `cwebp -noalpha` and
/// our alpha-dropping lossy encoder compare on RGB.
fn compare_lossy_vs_libwebp_real(real: &Path, max_edge: u32) -> Result<()> {
    let cwebp = cwebp_bin();
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!("metrics --lossy --real: skipped ({e})");
        return Ok(());
    }

    println!();
    println!(
        "webpkit AUTO vs cwebp -q Q default shaping on real images (printed only, NOT gated):"
    );
    match (max_edge > 0).then_some(max_edge) {
        Some(w) => println!("  input width capped to {w}px by cwebp (aspect preserved)"),
        None => println!("  native resolution (no resize)"),
    }
    println!(
        "  bytes = summed VP8 payload; both decoded by our decoder; category derived from pixels"
    );
    println!(
        "  size% = ours*100/cwebp (<100 = ours smaller); +dB / +SSIM = ours - cwebp (>0 = ours better)"
    );

    let prepared = prepare_real_images(&cwebp, real, max_edge)?;

    let tmp = tempfile::tempdir().context("creating tempdir for lossy --real comparison")?;
    let src_png = tmp.path().join("src.png");
    let out_webp = tmp.path().join("out.webp");

    // [quality][category] byte/quality accumulators.
    let mut acc = [[CategoryAcc::default(); 3]; LOSSY_QUALITIES.len()];
    let mut skipped = 0usize;

    for item in &prepared {
        let RealPrep::Ready(img) = item else {
            skipped += 1;
            continue;
        };
        let (w, h) = (img.dims.width() as usize, img.dims.height() as usize);
        let cat = classify_real(&img.rgba);
        // cwebp reads a file, so materialize the exact resized pixels once per image.
        write_png(&src_png, img.dims.width(), img.dims.height(), &img.rgba)?;
        let dims = webpkit::lossy::Dimensions::new(img.dims.width(), img.dims.height())?;

        for (qi, &q) in LOSSY_QUALITIES.iter().enumerate() {
            // Ours: the zero-knob AUTO default a user actually ships.
            let image =
                webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &img.rgba)?;
            let cfg = webpkit::lossy::LossyConfig::new()
                .with_quality(q)
                .with_effort(webpkit::lossy::Effort::AUTO);
            let (_dims, ours) = webpkit::lossy::encode_vp8(image, &cfg)?;
            let ours_dec = webpkit::lossy::decode(&ours)?;

            // cwebp default shaping at the same quality.
            run_cwebp_lossy(&cwebp, &src_png, &out_webp, q)?;
            let cwebp_file = std::fs::read(&out_webp)
                .with_context(|| format!("reading {}", out_webp.display()))?;
            let cwebp_vp8 = extract_vp8_chunk(&cwebp_file)?;
            let cwebp_dec = webpkit::lossy::decode(&cwebp_vp8)?;

            let a = &mut acc[qi][cat.idx()];
            a.n += 1;
            a.ours_bytes += ours.len() as u64;
            a.cwebp_bytes += cwebp_vp8.len() as u64;
            a.ours_psnr += psnr_rgb(&img.rgba, ours_dec.as_bytes());
            a.cwebp_psnr += psnr_rgb(&img.rgba, cwebp_dec.as_bytes());
            a.ours_ssim += ssim_rgb(&img.rgba, ours_dec.as_bytes(), w, h);
            a.cwebp_ssim += ssim_rgb(&img.rgba, cwebp_dec.as_bytes(), w, h);
        }
    }

    for (qi, &q) in LOSSY_QUALITIES.iter().enumerate() {
        println!();
        println!("q{q}:");
        println!(
            "  {:<12} {:>3} {:>12} {:>12} {:>7} {:>7} {:>6} {:>6} {:>8} {:>7} {:>8}",
            "category",
            "n",
            "ours bytes",
            "cwebp bytes",
            "size%",
            "o.dB",
            "c.dB",
            "+dB",
            "o.SSIM",
            "c.SSIM",
            "+SSIM"
        );
        let mut all = CategoryAcc::default();
        for cat in RealCategory::ALL {
            let a = acc[qi][cat.idx()];
            a.print_row(cat.label());
            a.add_into(&mut all);
        }
        all.print_row("ALL");
    }
    if skipped > 0 {
        println!();
        println!("  ({skipped} non-image file(s) skipped)");
    }
    Ok(())
}
