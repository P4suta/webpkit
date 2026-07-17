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
//!    image's `ALPH` plane is a VP8L stream), and the facade (`lib.rs`,
//!    `encoder.rs`, `mux.rs`) may use anything. `mux.rs` is facade, not shell:
//!    `AnimationMux` is the authoring counterpart to `AnimationEncoder` and, like
//!    it, composes container framing with codec knowledge (peeking a passthrough
//!    frame's VP8L header for its alpha bit) rather than staying bitstream-agnostic.
//! 2. **Intra-codec layering.** Inside `lossless/` and `lossy/`, every
//!    `crate::<zone>::<module>` edge must point at an equal-or-lower layer.
//!
//! The scanning itself lives in `webpkit-archtest`, shared with the CLI's gate and
//! tested there: both gates once carried their own copy, and the copies drifted
//! until this one — the more load-bearing — could not see a violation written the
//! way rustfmt writes it. It stays a heuristic: comments and doc links never count,
//! and it catches the accident rather than every conceivable obfuscation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "architecture test scaffold: reads of the crate's own src/ should panic \
              loudly if the repo layout is broken"
)]

use std::collections::BTreeSet;
use std::path::Path;

use webpkit_archtest::{all_src, module_edges, names_module};

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
    ("tuning", 0),
    ("idct", 1),
    ("predict", 1),
    ("loop_filter", 1),
    ("yuv", 1),
    ("fdct", 1),
    ("quant", 1),
    ("rgb_to_yuv", 1),
    ("perceptual", 1),
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
    ("sharp_yuv", 2),
    ("decode_incr", 3),
    ("frame", 3),
    ("alpha", 3),
    ("decoder", 4),
    ("encoder", 4),
    ("rate", 4),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Shell,
    Lossless,
    Lossy,
    Facade,
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
        ["lib.rs" | "encoder.rs" | "mux.rs"] => (Zone::Facade, None),
        ["lossless.rs"] => (Zone::Lossless, None),
        ["lossy.rs"] => (Zone::Lossy, None),
        ["lossless", rest @ ..] => (Zone::Lossless, rest.first().map(|s| strip(s))),
        ["lossy", rest @ ..] => (Zone::Lossy, rest.first().map(|s| strip(s))),
        [first, ..] => (Zone::Shell, Some(strip(first))),
        [] => (Zone::Shell, None),
    }
}

/// Every registered-module head in this zone that `code` depends on.
fn zone_edges(code: &str, prefix: &str, layers: &[(&str, u8)]) -> BTreeSet<String> {
    module_edges(code, "crate", prefix, &|ident| {
        layers.iter().any(|(m, _)| *m == ident)
    })
    .into_iter()
    .collect()
}

fn layer_of(layers: &[(&str, u8)], module: &str) -> Option<u8> {
    layers.iter().find(|(m, _)| *m == module).map(|(_, l)| *l)
}

/// Rule 1: no zone depends on a zone it must not see.
#[test]
fn zones_stay_isolated() {
    let mut violations = Vec::new();
    for (rel, code) in all_src(env!("CARGO_MANIFEST_DIR")) {
        let (zone, _) = classify(&rel);
        // `names_module`, not `contains`: a grouped `use crate::{lossless::decode}`
        // is the form rustfmt writes and a substring scan never sees it.
        let refs_lossless = names_module(&code, "crate", "lossless");
        let refs_lossy = names_module(&code, "crate", "lossy");
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
    for (rel, code) in all_src(env!("CARGO_MANIFEST_DIR")) {
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
    for (rel, _) in all_src(env!("CARGO_MANIFEST_DIR")) {
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
