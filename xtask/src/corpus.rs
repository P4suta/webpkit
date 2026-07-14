//! Corpus sweep (L3): decode every committed image, hash it, re-encode it, and
//! self-round-trip it, gating the committed `corpus/baseline.json` byte-golden
//! against a fresh run (with a documented `self_roundtrip` allowlist).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::common::{corpus_dir, decode_fixtures_dir, fnv1a64, webpkit_encode, workspace_root};

/// One committed corpus image's decode + re-encode outcome.
///
/// Field order is load-bearing: it fixes the JSON key order so the committed
/// `corpus/baseline.json` is a pure textual diff of a fresh run (like the
/// conformance ledger). A malformed input that decodes to `Err` is a *normal*
/// recorded outcome (`decoded_ok = false`), not a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CorpusCase {
    /// Stable case id (e.g. `conformance/color_cache_scatter`).
    id: String,
    /// Whether `webpkit::lossless::decode_rgba` returned `Ok`.
    decoded_ok: bool,
    /// Decoded width in pixels (0 when `decoded_ok` is false).
    width: u32,
    /// Decoded height in pixels (0 when `decoded_ok` is false).
    height: u32,
    /// FNV-1a-64 of the decoded RGBA bytes (decoder stability oracle).
    decoded_hash: u64,
    /// Whether re-encoding the decoded pixels returned `Ok`.
    reencode_ok: bool,
    /// Byte length of the re-encoded WebP (0 when `reencode_ok` is false).
    reencode_len: u64,
    /// FNV-1a-64 of the re-encoded WebP bytes (encoder byte-stability oracle).
    reencode_hash: u64,
    /// Whether `decode(encode(decoded)) == decoded` (lossless self round-trip).
    self_roundtrip: bool,
}

/// The committed corpus baseline: the machine oracle later tracks compare against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CorpusBaseline {
    /// Schema version (bump on any structural change).
    version: u32,
    /// Per-case results, sorted by `id`.
    cases: Vec<CorpusCase>,
}

/// A documented exemption from the `self_roundtrip` hard invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoundtripExemption {
    /// The exempt case id.
    id: String,
    /// Why the self round-trip invariant legitimately does not apply.
    reason: String,
}

/// The committed allowlist of `self_roundtrip` exemptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoundtripAllowlist {
    /// Schema version (bump on any structural change).
    version: u32,
    /// Empirically-authored exemptions (never speculative).
    exempt: Vec<RoundtripExemption>,
}

/// Enumerate every committed corpus case as `(id, path)`, sorted by `id`.
///
/// Optional directories (`seeds/animation`, `corpus/extra`) are silently skipped
/// when absent, so the sweep degrades gracefully.
fn enumerate_corpus(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let seeds = root.join("crates/webpkit-lossless-fuzz/seeds");
    let mut cases = Vec::new();
    collect_flat_webp(&seeds.join("decode"), "fuzz/decode", &mut cases)?;
    collect_flat_webp(&seeds.join("roundtrip"), "fuzz/roundtrip", &mut cases)?;
    collect_flat_webp(&seeds.join("animation"), "fuzz/animation", &mut cases)?;
    collect_conformance_webp(&decode_fixtures_dir(root), &mut cases)?;
    collect_flat_webp(&corpus_dir(root).join("extra"), "extra", &mut cases)?;
    cases.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(cases)
}

/// Append every `*.webp` file directly under `dir` as `<prefix>/<stem>`.
/// A missing directory yields no cases (silently skipped).
fn collect_flat_webp(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("webp"))
        .collect();
    for path in paths {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("non-UTF8 file stem in {}", path.display()))?;
        out.push((format!("{prefix}/{stem}"), path));
    }
    Ok(())
}

/// Append each conformance case dir holding an `input.webp` as `conformance/<case>`.
/// A missing directory yields no cases (silently skipped).
fn collect_conformance_webp(dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    for case_dir in dirs {
        let input = case_dir.join("input.webp");
        if !input.exists() {
            continue;
        }
        let case = case_dir
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("non-UTF8 case dir in {}", dir.display()))?;
        out.push((format!("conformance/{case}"), input));
    }
    Ok(())
}

/// Decode, hash, re-encode and self-round-trip one corpus image.
///
/// `webpkit::lossless::decode_rgba` is called inside a `match` — a returned `Err` is a
/// recorded outcome, never a failure. Only a panic/abort fails the sweep.
fn sweep_one(id: String, bytes: &[u8]) -> CorpusCase {
    let Ok((dims, rgba)) = webpkit::lossless::decode_rgba(bytes) else {
        return CorpusCase {
            id,
            decoded_ok: false,
            width: 0,
            height: 0,
            decoded_hash: 0,
            reencode_ok: false,
            reencode_len: 0,
            reencode_hash: 0,
            self_roundtrip: false,
        };
    };
    let (width, height) = (dims.width(), dims.height());
    let decoded_hash = fnv1a64(&rgba);

    let Ok(reencoded) = webpkit_encode(&rgba, width, height) else {
        return CorpusCase {
            id,
            decoded_ok: true,
            width,
            height,
            decoded_hash,
            reencode_ok: false,
            reencode_len: 0,
            reencode_hash: 0,
            self_roundtrip: false,
        };
    };
    let reencode_hash = fnv1a64(&reencoded);
    let reencode_len = reencoded.len() as u64;
    let self_roundtrip = matches!(
        webpkit::lossless::decode_rgba(&reencoded),
        Ok((d, out)) if d.width() == width && d.height() == height && out == rgba
    );

    CorpusCase {
        id,
        decoded_ok: true,
        width,
        height,
        decoded_hash,
        reencode_ok: true,
        reencode_len,
        reencode_hash,
        self_roundtrip,
    }
}

/// Compute a fresh corpus baseline by sweeping every committed case in order.
fn compute_corpus_baseline(root: &Path) -> Result<CorpusBaseline> {
    let cases = enumerate_corpus(root)?;
    let mut swept = Vec::with_capacity(cases.len());
    for (id, path) in cases {
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        swept.push(sweep_one(id, &bytes));
    }
    Ok(CorpusBaseline {
        version: 1,
        cases: swept,
    })
}

/// Load the committed allowlist, treating an absent file as "no exemptions".
fn load_roundtrip_allowlist(path: &Path) -> Result<RoundtripAllowlist> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RoundtripAllowlist {
            version: 1,
            exempt: Vec::new(),
        }),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
}

/// Describe the first case where a fresh sweep diverges from the committed
/// baseline, for an actionable stale-baseline error.
fn first_corpus_divergence(committed: &CorpusBaseline, fresh: &CorpusBaseline) -> String {
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
            None => return format!("case `{}` is new (not in committed baseline)", fc.id),
        }
    }
    if let Some(extra) = committed.cases.get(fresh.cases.len()) {
        return format!("committed baseline has extra case `{}`", extra.id);
    }
    "textual formatting only (no structural difference)".to_owned()
}

pub(crate) fn corpus_sweep(bless: bool) -> Result<()> {
    let root = workspace_root()?;
    let dir = corpus_dir(&root);
    let baseline_path = dir.join("baseline.json");
    let allowlist_path = dir.join("roundtrip-allowlist.json");

    let fresh = compute_corpus_baseline(&root)?;
    let fresh_json = format!("{}\n", serde_json::to_string_pretty(&fresh)?);

    if bless {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(&baseline_path, &fresh_json)
            .with_context(|| format!("writing {}", baseline_path.display()))?;
        // Seed an empty allowlist only if absent — never clobber an authored one.
        if !allowlist_path.exists() {
            let empty = RoundtripAllowlist {
                version: 1,
                exempt: Vec::new(),
            };
            let json = format!("{}\n", serde_json::to_string_pretty(&empty)?);
            std::fs::write(&allowlist_path, json)
                .with_context(|| format!("writing {}", allowlist_path.display()))?;
        }
        println!(
            "corpus-sweep: blessed {} case(s) -> {}",
            fresh.cases.len(),
            baseline_path.display()
        );
        return Ok(());
    }

    // Gate mode. First enforce the hard invariants on non-exempt cases.
    let allowlist = load_roundtrip_allowlist(&allowlist_path)?;
    let is_exempt = |id: &str| allowlist.exempt.iter().any(|e| e.id == id);
    for c in &fresh.cases {
        if !c.decoded_ok || is_exempt(&c.id) {
            continue;
        }
        if !c.reencode_ok {
            bail!(
                "corpus-sweep: case `{}` decoded but failed to re-encode (not in \
                 roundtrip-allowlist.json)",
                c.id
            );
        }
        if !c.self_roundtrip {
            bail!(
                "corpus-sweep: case `{}` violates the self_roundtrip invariant \
                 (decode(encode(decoded)) != decoded). If this is legitimate, add it to \
                 corpus/roundtrip-allowlist.json with a documented reason.",
                c.id
            );
        }
    }

    // Then gate committed == fresh (byte-golden drift), like `drift_gate`.
    let committed = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "no corpus baseline at {} — run `cargo xtask corpus-sweep --bless` and commit it first",
            baseline_path.display()
        ),
        Err(e) => {
            return Err(
                anyhow::Error::new(e).context(format!("reading {}", baseline_path.display()))
            );
        },
    };

    if committed != fresh_json {
        let detail = serde_json::from_str::<CorpusBaseline>(&committed).map_or_else(
            |_| "committed baseline is not valid JSON".to_owned(),
            |committed_baseline| first_corpus_divergence(&committed_baseline, &fresh),
        );
        bail!(
            "corpus-sweep: baseline at {} is stale (first divergence: {}). \
             Run `cargo xtask corpus-sweep --bless` and commit the updated file.",
            baseline_path.display(),
            detail
        );
    }

    println!("corpus-sweep: {} case(s) stable", fresh.cases.len());
    Ok(())
}
