//! Mechanical enforcement of the merged crate's module architecture.
//!
//! `webpkit` fuses three former crates (`webpkit-core`, `-lossless`, `-lossy`)
//! into one. This gate keeps those zones from bleeding into each other and
//! preserves each codec's internal layering that the separate crates used to get
//! for free from the crate boundary:
//!
//! 1. **Zone isolation.** The bitstream-agnostic *shell* (the crate-root modules
//!    `error`, `image`, `container`, `stream`, `anim`, `alpha`, `effort`,
//!    `prelude`) must not depend on either codec, and the `lossless` engine must
//!    not depend on `lossy`. The `lossy` engine *may* use `lossless` (a lossy
//!    image's `ALPH` plane is a VP8L stream), and the facade (`lib.rs` /
//!    `encoder.rs`) may use anything.
//! 2. **Intra-codec layering.** Inside `lossless/` and `lossy/`, every
//!    `crate::<zone>::<module>` edge must point at an equal-or-lower layer.
//!
//! Like the per-crate gates it replaces, this is a source-scanning heuristic:
//! each line is truncated at its first `//` (dropping comments and doc links), and
//! only lowercase module heads that name a registered module count as edges. It
//! catches accidental upward/cross dependencies, not every conceivable obfuscation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "architecture test scaffold: reads of the crate's own src/ should panic \
              loudly if the repo layout is broken"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `lossless/` internal layers. Lower layers must not depend on higher ones.
///
/// - L0 foundational vocabulary   L1 algorithm primitives
/// - L2 VP8L bitstream            L4 public composition
const LOSSLESS_LAYERS: &[(&str, u8)] = &[
    ("constants", 0),
    ("bit_io", 0),
    ("prelude", 0),
    ("work", 0),
    ("huffman", 1),
    ("color_cache", 1),
    ("lz77", 1),
    ("transform", 1),
    ("histogram", 1),
    ("vp8l", 2),
    ("decoder", 4),
    ("encoder", 4),
    ("animation", 4),
];

/// `lossy/` internal layers. Lower layers must not depend on higher ones.
///
/// - L0 foundational vocabulary   L1 pixel-operation primitives
/// - L2 VP8 bitstream             L3 suspend/resume + frame planning
/// - L4 public composition
const LOSSY_LAYERS: &[(&str, u8)] = &[
    ("bool_dec", 0),
    ("bool_enc", 0),
    ("constants", 0),
    ("prelude", 0),
    ("work", 0),
    ("idct", 1),
    ("predict", 1),
    ("loop_filter", 1),
    ("yuv", 1),
    ("fdct", 1),
    ("quant", 1),
    ("rgb_to_yuv", 1),
    ("frame_header", 2),
    ("header", 2),
    ("mb", 2),
    ("token", 2),
    ("reconstruct", 2),
    ("decode", 2),
    ("tokens", 2),
    ("enc_header", 2),
    ("prob_opt", 2),
    ("trellis", 2),
    ("decode_incr", 3),
    ("frame", 3),
    ("alpha", 3),
    ("decoder", 4),
    ("encoder", 4),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Shell,
    Lossless,
    Lossy,
    Facade,
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Recursively collect every `.rs` file under `dir`.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Drop each line's first `//`-comment so intra-doc links / comments don't count
/// as dependency edges.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Classify a `src`-relative file into its zone and, for a codec module, the
/// top-level module name within that zone (`None` for a zone/crate root).
fn classify(rel: &Path) -> (Zone, Option<String>) {
    let comps: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let strip = |s: &str| s.trim_end_matches(".rs").to_owned();
    match comps.as_slice() {
        ["lib.rs" | "encoder.rs"] => (Zone::Facade, None),
        ["lossless.rs"] => (Zone::Lossless, None),
        ["lossy.rs"] => (Zone::Lossy, None),
        ["lossless", rest @ ..] => (Zone::Lossless, rest.first().map(|s| strip(s))),
        ["lossy", rest @ ..] => (Zone::Lossy, rest.first().map(|s| strip(s))),
        [first, ..] => (Zone::Shell, Some(strip(first))),
        [] => (Zone::Shell, None),
    }
}

/// Every registered-module head in `crate::<prefix>::<ident>` references.
fn zone_edges(code: &str, prefix: &str, layers: &[(&str, u8)]) -> BTreeSet<String> {
    let needle = format!("crate::{prefix}::");
    let mut refs = BTreeSet::new();
    let mut rest = code;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if layers.iter().any(|(m, _)| *m == ident) {
            refs.insert(ident);
        }
    }
    refs
}

fn layer_of(layers: &[(&str, u8)], module: &str) -> Option<u8> {
    layers.iter().find(|(m, _)| *m == module).map(|(_, l)| *l)
}

fn all_src() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    collect_rs(&src_dir(), &mut files).expect("read src/");
    files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(src_dir()).unwrap().to_path_buf();
            let code = strip_comments(&std::fs::read_to_string(&p).expect("read file"));
            (rel, code)
        })
        .collect()
}

/// Rule 1: no zone depends on a zone it must not see.
#[test]
fn zones_stay_isolated() {
    let mut violations = Vec::new();
    for (rel, code) in all_src() {
        let (zone, _) = classify(&rel);
        let refs_lossless = code.contains("crate::lossless");
        let refs_lossy = code.contains("crate::lossy");
        match zone {
            // The shell is the shared base: it must name neither codec.
            Zone::Shell if refs_lossless || refs_lossy => {
                violations.push(format!("shell `{}` depends on a codec", rel.display()));
            },
            // The lossless engine must not reach sideways into lossy.
            Zone::Lossless if refs_lossy => {
                violations.push(format!("lossless `{}` depends on lossy", rel.display()));
            },
            // Lossy may use lossless (ALPH); the facade may use anything.
            _ => {},
        }
    }
    assert!(
        violations.is_empty(),
        "zone-isolation violations:\n  {}",
        violations.join("\n  ")
    );
}

/// Rule 2: intra-codec `crate::<zone>::<module>` edges point equal-or-lower.
fn assert_intra_layering(zone: Zone, prefix: &str, layers: &[(&str, u8)]) {
    let mut violations = Vec::new();
    for (rel, code) in all_src() {
        let (file_zone, module) = classify(&rel);
        if file_zone != zone {
            continue;
        }
        // The zone root (`lossless.rs` / `lossy.rs`) is the composition point.
        let Some(module) = module else { continue };
        let Some(here) = layer_of(layers, &module) else {
            continue;
        };
        for dep in zone_edges(&code, prefix, layers) {
            let there = layer_of(layers, &dep).unwrap();
            if there > here {
                violations.push(format!(
                    "{prefix}::{module} (L{here}) -> {prefix}::{dep} (L{there})"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{prefix} upward-dependency violations:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn lossless_layers_point_downward() {
    assert_intra_layering(Zone::Lossless, "lossless", LOSSLESS_LAYERS);
}

#[test]
fn lossy_layers_point_downward() {
    assert_intra_layering(Zone::Lossy, "lossy", LOSSY_LAYERS);
}

/// Rule 3: every top-level codec module is registered, so the layering can't
/// silently rot when a new module is added.
fn assert_all_registered(zone: Zone, prefix: &str, layers: &[(&str, u8)]) {
    let mut unregistered = BTreeSet::new();
    for (rel, _) in all_src() {
        let (file_zone, module) = classify(&rel);
        if file_zone != zone {
            continue;
        }
        if let Some(module) = module
            && layer_of(layers, &module).is_none()
        {
            unregistered.insert(module);
        }
    }
    assert!(
        unregistered.is_empty(),
        "unregistered `{prefix}` modules (add to the layer table): {unregistered:?}"
    );
}

#[test]
fn every_lossless_module_is_registered() {
    assert_all_registered(Zone::Lossless, "lossless", LOSSLESS_LAYERS);
}

#[test]
fn every_lossy_module_is_registered() {
    assert_all_registered(Zone::Lossy, "lossy", LOSSY_LAYERS);
}
