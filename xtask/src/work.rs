//! Work-cost ledger (`corpus/work.json`): committed per-(codec, sample, method)
//! deterministic algorithmic-work counters — the toolchain-independent third
//! measurement plane. Available only with the `work-count` feature; a stub bails
//! otherwise so the ordinary `metrics` job stays free of counter code.

#[cfg(feature = "work-count")]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(feature = "work-count")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "work-count")]
use crate::common::{
    ALL_METHODS, LOSSY_METHODS, corpus_dir, lossy_method_name, method_name, workspace_root,
};
#[cfg(feature = "work-count")]
use crate::ledger::{FieldDelta, load_ledger, print_field_deltas};

/// One committed work-cost row: the integer algorithmic-work counters recorded
/// while encoding one synthetic sample with one codec+method.
///
/// The `counts` map is keyed by [`webpkit::work_count::field_names`], so it renders
/// as a deterministic (BTreeMap-sorted) JSON object and auto-tracks the counter
/// vocabulary — appending a `Counter` variant flows into the ledger with no edit
/// here. Every value is an integer and, unlike `metrics.json`'s peak-memory
/// fields, the counts are toolchain- AND profile-independent, so this ledger
/// needs no MSRV pin.
#[cfg(feature = "work-count")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkCase {
    /// Stable sort key: `"{codec}/{content}_{edge:03}/{method}"`.
    id: String,
    /// Codec label (`"lossless"` = webpkit-lossless | `"lossy"` = webpkit-lossy).
    codec: String,
    /// Content archetype name (e.g. `"photo"`, `"gradient"`).
    content: String,
    /// Square edge length in pixels.
    edge: u32,
    /// Encoder method label (`"fast"` | `"balanced"` | `"best"`).
    method: String,
    /// Per-hot-path work counts, keyed by [`webpkit::work_count::field_names`].
    counts: std::collections::BTreeMap<String, u64>,
}

/// The committed work-cost ledger, sorted by `id`.
#[cfg(feature = "work-count")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkLedger {
    /// Schema version (bump on any structural change, e.g. adding decode counts).
    version: u32,
    /// Per-(codec, sample, method) rows, sorted by `id`.
    cases: Vec<WorkCase>,
}

/// Describe the first case where a fresh work run diverges from the committed
/// ledger (mirrors `first_metrics_divergence`).
#[cfg(feature = "work-count")]
fn first_work_divergence(committed: &WorkLedger, fresh: &WorkLedger) -> String {
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
            Some(_) => return format!("case `{}` counts changed", fc.id),
            None => return format!("case `{}` is new (not in committed ledger)", fc.id),
        }
    }
    if let Some(extra) = committed.cases.get(fresh.cases.len()) {
        return format!("committed ledger has extra case `{}`", extra.id);
    }
    "textual formatting only (no structural difference)".to_owned()
}

/// Counter-level diff of the work-cost ledger: which hot-loop counters moved, in
/// which cases, and by how much — the counter-plane analog of `explain_metrics`,
/// confirming an intended reduction touches ONLY its own counter.
#[cfg(feature = "work-count")]
fn explain_work(committed: &WorkLedger, fresh: &WorkLedger) {
    println!("work diff (committed -> fresh):");
    let by_id: std::collections::BTreeMap<&str, &WorkCase> =
        committed.cases.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut deltas: Vec<FieldDelta> = Vec::new();
    for fc in &fresh.cases {
        let Some(cc) = by_id.get(fc.id.as_str()) else {
            println!("  case `{}` is new (not in committed ledger)", fc.id);
            continue;
        };
        for (name, &new) in &fc.counts {
            let old = cc.counts.get(name).copied().unwrap_or(0);
            if old != new {
                deltas.push(FieldDelta {
                    case: fc.id.clone(),
                    field: name.clone(),
                    old,
                    new,
                });
            }
        }
    }
    print_field_deltas(&deltas);
}

/// Turn a raw counter snapshot into a name-keyed map for the ledger.
#[cfg(feature = "work-count")]
fn counts_map(snapshot: [u64; webpkit::work_count::N]) -> std::collections::BTreeMap<String, u64> {
    webpkit::work_count::field_names()
        .iter()
        .zip(snapshot.iter())
        .map(|(&name, &n)| (name.to_owned(), n))
        .collect()
}

/// Encode one sample losslessly with the counters reset, and snapshot the work.
///
/// The counters are process-global, so the reset/encode/snapshot bracket must be
/// serial (the caller loops one case at a time) and the snapshot is read only
/// after the encode — including any in-encode rayon region — has fully returned.
#[cfg(feature = "work-count")]
fn measure_work_lossless(
    sample: &webpkit_samples::Sample,
    method: webpkit::lossless::Effort,
) -> Result<WorkCase> {
    let dims = webpkit::lossless::Dimensions::new(sample.edge, sample.edge)?;
    let image = webpkit::lossless::ImageRef::new(
        dims,
        webpkit::lossless::PixelLayout::Rgba8,
        &sample.rgba,
    )?;
    let config = webpkit::lossless::EncoderConfig::new().with_effort(method);

    webpkit::work_count::reset();
    let _webp = webpkit::lossless::encode(image, &config)?;
    let counts = counts_map(webpkit::work_count::snapshot());

    Ok(WorkCase {
        id: format!(
            "lossless/{}/{}",
            webpkit_samples::sample_id(sample.content, sample.edge),
            method_name(method)
        ),
        codec: "lossless".to_owned(),
        content: sample.content.name().to_owned(),
        edge: sample.edge,
        method: method_name(method).to_owned(),
        counts,
    })
}

/// Encode one sample lossily (default quality) with the counters reset, and
/// snapshot the work. Same serial reset/encode/snapshot discipline as the
/// lossless path.
#[cfg(feature = "work-count")]
fn measure_work_lossy(
    sample: &webpkit_samples::Sample,
    method: webpkit::lossy::Effort,
) -> Result<WorkCase> {
    let dims = webpkit::lossy::Dimensions::new(sample.edge, sample.edge)?;
    let image =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &sample.rgba)?;
    let cfg = webpkit::lossy::LossyConfig::new().with_effort(method);

    webpkit::work_count::reset();
    let _ = webpkit::lossy::encode_vp8(image, &cfg)?;
    let counts = counts_map(webpkit::work_count::snapshot());

    Ok(WorkCase {
        id: format!(
            "lossy/{}/{}",
            webpkit_samples::sample_id(sample.content, sample.edge),
            lossy_method_name(method)
        ),
        codec: "lossy".to_owned(),
        content: sample.content.name().to_owned(),
        edge: sample.edge,
        method: lossy_method_name(method).to_owned(),
        counts,
    })
}

/// The fixed (method, quality) at which a decode-work sample is encoded. Decode
/// work is method-INDEPENDENT (the bitstream, not the search that produced it,
/// drives the decoder), but it scales with the coefficient density / filter
/// level the encoder chose, so a mid-quality `Balanced` encode is a stable,
/// representative decode workload across the archetype matrix.
#[cfg(feature = "work-count")]
const WORK_DECODE_QUALITY: u8 = 75;

/// Decode one sample's lossy payload with the counters reset, and snapshot the
/// work — the decode-plane analog of [`measure_work_lossy`]. The payload is
/// produced once at [`WORK_DECODE_QUALITY`] / `Balanced`; the reset/decode/
/// snapshot bracket then isolates the DECODE hot paths (`bool_read`,
/// `coeff_token`, `idct_call`, `loop_filter_edge`, `upsample_row`, plus the
/// shared `predict_*`). The encode-only counters stay zero in this row, and the
/// decode-only counters stay zero in the encode rows, so the two planes never
/// contaminate each other. Same serial discipline as the encode paths.
#[cfg(feature = "work-count")]
fn measure_work_lossy_decode(sample: &webpkit_samples::Sample) -> Result<WorkCase> {
    let dims = webpkit::lossy::Dimensions::new(sample.edge, sample.edge)?;
    let image =
        webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &sample.rgba)?;
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(WORK_DECODE_QUALITY)
        .with_effort(webpkit::lossy::Effort::Balanced);
    let (_dims, payload) = webpkit::lossy::encode_vp8(image, &cfg)?;

    webpkit::work_count::reset();
    let _ = webpkit::lossy::decode(&payload)?;
    let counts = counts_map(webpkit::work_count::snapshot());

    Ok(WorkCase {
        id: format!(
            "lossy-decode/{}/decode",
            webpkit_samples::sample_id(sample.content, sample.edge),
        ),
        codec: "lossy-decode".to_owned(),
        content: sample.content.name().to_owned(),
        edge: sample.edge,
        method: "decode".to_owned(),
        counts,
    })
}

/// The counter increments make the `Best` search several times slower than in a
/// normal build, and its `Best@512` cases (lossless meta-Huffman clustering,
/// lossy trellis + i4x4) dominate the run. `Best` is therefore capped to
/// `edge <= 256`, mirroring the `--vs-libwebp` cap: the 64/256 `Best` rows fully
/// capture its algorithmic signature (the counts scale predictably with size),
/// so the ledger stays a fast, comprehensive proxy without the 512 tail. `Fast`
/// and `Balanced` are measured at every size.
#[cfg(feature = "work-count")]
const WORK_BEST_MAX_EDGE: u32 = 256;

/// Compute a fresh work-cost ledger over the shared synthetic corpus for BOTH
/// codecs. Serial by construction (the counters are process-global statics).
#[cfg(feature = "work-count")]
fn compute_work() -> Result<WorkLedger> {
    let mut cases = Vec::new();
    for sample in webpkit_samples::matrix() {
        for &method in &ALL_METHODS {
            if method == webpkit::lossless::Effort::Best && sample.edge > WORK_BEST_MAX_EDGE {
                continue;
            }
            cases.push(measure_work_lossless(&sample, method)?);
        }
        for &method in &LOSSY_METHODS {
            if method == webpkit::lossy::Effort::Best && sample.edge > WORK_BEST_MAX_EDGE {
                continue;
            }
            cases.push(measure_work_lossy(&sample, method)?);
        }
        // One decode row per sample (decode is method-independent).
        cases.push(measure_work_lossy_decode(&sample)?);
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WorkLedger { version: 2, cases })
}

/// Gate (or `--bless`) the committed `corpus/work.json` deterministic work-cost
/// ledger. Only available with the `work-count` feature (the counters are a
/// dependency then); without it the command bails with a rebuild hint so the
/// ordinary `metrics` job stays free of counter code.
#[cfg(feature = "work-count")]
pub(crate) fn work(bless: bool, explain: bool) -> Result<()> {
    let root = workspace_root()?;
    let dir = corpus_dir(&root);
    let work_path = dir.join("work.json");

    // `--explain` prints a counter-level committed-vs-fresh diff and returns
    // without gating or blessing — confirms a hot-loop reduction touched only its
    // own counter.
    if explain {
        let committed = load_ledger::<WorkLedger>(&work_path)?;
        let fresh = compute_work()?;
        explain_work(&committed, &fresh);
        return Ok(());
    }

    let fresh = compute_work()?;
    let fresh_json = format!("{}\n", serde_json::to_string_pretty(&fresh)?);

    if bless {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&work_path, &fresh_json)
            .with_context(|| format!("writing {}", work_path.display()))?;
        println!(
            "work: blessed {} case(s) -> {}",
            fresh.cases.len(),
            work_path.display()
        );
    } else {
        let committed = match std::fs::read_to_string(&work_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "no work ledger at {} — run `just work-bless` and commit it first",
                work_path.display()
            ),
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("reading {}", work_path.display()))
                );
            },
        };

        if committed != fresh_json {
            let detail = serde_json::from_str::<WorkLedger>(&committed).map_or_else(
                |_| "committed ledger is not valid JSON".to_owned(),
                |committed_ledger| first_work_divergence(&committed_ledger, &fresh),
            );
            bail!(
                "work: ledger at {} is stale (first divergence: {}). \
                 Run `just work-bless` and commit the updated file.",
                work_path.display(),
                detail
            );
        }

        println!("work: {} case(s) stable", fresh.cases.len());
    }
    Ok(())
}

/// Stub when the `work-count` feature is off: the counters are not linked, so the
/// ledger cannot be produced. Bails with a rebuild hint (keeps the `metrics` job
/// counter-free so its toolchain-sensitive peak-memory numbers are undisturbed).
#[cfg(not(feature = "work-count"))]
pub(crate) fn work(_bless: bool, _explain: bool) -> Result<()> {
    bail!(
        "the `work` command needs the algorithmic-work counters: rebuild with \
         `--features work-count` (or run `just work` / `just work-bless`)"
    )
}
