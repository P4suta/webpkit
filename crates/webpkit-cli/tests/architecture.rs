//! Mechanical enforcement of the CLI's architecture.
//!
//! Two properties, neither of which the compiler can state:
//!
//! 1. **Facade-only.** The CLI must name codec types through `webpkit`'s curated
//!    re-exports, never through the `#[doc(hidden)]` modules they live in. Those
//!    modules are public only so this workspace's own tooling can reach them; they
//!    are explicitly not a stable API. The CLI is `webpkit`'s flagship consumer, so
//!    if it bypasses the facade, nothing verifies that the facade is sufficient to
//!    build a real application against.
//! 2. **Layering.** Every `crate::<module>` edge must point at an equal-or-lower
//!    layer, and every module must be registered in the table so the layering
//!    cannot silently rot as modules are added.
//!
//! The scanning lives in `webpkit-archtest`, shared with `webpkit`'s gate and
//! tested there — one home, so a fix cannot land on one gate and miss the other.
//! It stays a heuristic: comments and doc links never count, and it catches the
//! accident rather than every conceivable obfuscation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "architecture test scaffold: reads of the crate's own src/ should panic \
              loudly if the repo layout is broken"
)]

use std::collections::BTreeSet;
use std::path::Path;

use webpkit_archtest::{all_src, names_module, slashed, use_paths};

/// Module layers. An edge may point at its own layer or lower, never higher.
///
/// - L0 vocabulary with no dependencies   L1 the outside world (bytes, pixels, terminal)
/// - L2 resolved intent                   L3 what to do    L4 doing it    L5 argv
const CLI_LAYERS: &[(&str, u8)] = &[
    ("diag", 0),
    ("error", 0),
    ("personality", 0),
    ("codec", 1),
    ("format", 1),
    ("io", 1),
    ("metadata", 1),
    ("report", 1),
    ("term", 1),
    ("config", 2),
    ("effort", 2),
    ("inspect", 2),
    ("bulk", 4),
    ("cli", 5),
];

/// `webpkit`'s internal modules. Public only for this workspace's own tooling and
/// explicitly not a stable API, so the CLI must reach the curated facade instead
/// (`webpkit::Image`, not `webpkit::image::Image`).
const HIDDEN_MODULES: &[&str] = &[
    "alpha",
    "anim",
    "container",
    "effort",
    "error",
    "image",
    "lossless",
    "lossy",
    "stream",
    "work_count",
];

/// Files permitted to name a hidden module, and the module each may name.
///
/// Every entry is a standing signal that `webpkit`'s facade should grow: the CLI
/// needs something the public API does not offer. Keep this list empty if you can.
const FACADE_GAPS: &[(&str, &str)] = &[
    // `webp info -v` prints the RIFF chunk table (the `webpinfo` half), and reads
    // an animation's per-frame codecs, which `probe` cannot answer for the file as
    // a whole. Neither has a facade equivalent: the container walk is not exposed.
    // The facade should grow a chunk iterator; until it does, this line is the
    // record of why.
    ("inspect.rs", "container"),
];

/// Modules permitted to touch the filesystem and the standard streams.
///
/// `io` owns the image bytes, which is what lets errors carry a real path and a
/// real message and what keeps a `-o -` pipe byte-clean. `report` and `term` own
/// the human channel: one writes stderr, the other asks whether it is a terminal.
/// Nothing else opens a file or a stream.
const IO_OWNERS: &[&str] = &["io", "report", "term"];

/// Every way to reach a file or a standard stream.
///
/// `eprintln!`/`println!` are here because `anstream` shadows the std macros:
/// clippy's `print_stderr` cannot see through the shadow, so this rule is the
/// only thing keeping output in `report`.
const IO_CALLS: &[&str] = &[
    "std::fs::",
    "fs::read",
    "fs::write",
    "io::stdin",
    "io::stdout",
    "io::stderr",
    "println!",
    "eprintln!",
];

/// The top-level module a `src`-relative file belongs to. `None` for the crate
/// root and the binary shims, which are composition points and may name anything.
fn module_of(rel: &Path) -> Option<String> {
    let comps: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match comps.as_slice() {
        ["lib.rs"] | ["bin", ..] | [] => None,
        [first, ..] => Some(first.trim_end_matches(".rs").to_owned()),
    }
}

fn layer_of(module: &str) -> Option<u8> {
    CLI_LAYERS
        .iter()
        .find(|(m, _)| *m == module)
        .map(|(_, l)| *l)
}

/// Every registered-module head in `crate::<ident>` references.
fn crate_edges(code: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for path in use_paths(code, "crate") {
        if let Some(head) = path.split("::").next()
            && layer_of(head).is_some()
        {
            refs.insert(head.to_owned());
        }
    }
    // Inline `crate::io::Sink::from_arg(..)`, which no `use` records.
    let mut rest = code;
    while let Some(pos) = rest.find("crate::") {
        rest = &rest[pos + "crate::".len()..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if layer_of(&ident).is_some() {
            refs.insert(ident);
        }
    }
    refs
}

/// Rule 1: name codec types through the facade, not through `webpkit`'s hidden
/// modules.
#[test]
fn codec_types_come_from_the_facade() {
    let mut violations = Vec::new();
    for (rel, code) in all_src(env!("CARGO_MANIFEST_DIR")) {
        let file = slashed(&rel);
        for hidden in HIDDEN_MODULES {
            if !names_module(&code, "webpkit", hidden) {
                continue;
            }
            let allowed = FACADE_GAPS
                .iter()
                .any(|(path, module)| *path == file && module == hidden);
            if !allowed {
                violations.push(format!(
                    "{file} names `webpkit::{hidden}::` — use the facade re-export \
                     (`webpkit::Image`, not `webpkit::image::Image`). If the facade \
                     genuinely lacks it, add the file to FACADE_GAPS and open an issue \
                     to widen `webpkit`'s public API."
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "facade violations:\n  {}",
        violations.join("\n  ")
    );
}

/// Rule 2: `crate::<module>` edges point equal-or-lower.
#[test]
fn cli_layers_point_downward() {
    let mut violations = Vec::new();
    for (rel, code) in all_src(env!("CARGO_MANIFEST_DIR")) {
        // `lib.rs` and the bin shims are composition roots.
        let Some(module) = module_of(&rel) else {
            continue;
        };
        let Some(here) = layer_of(&module) else {
            continue;
        };
        for dep in crate_edges(&code) {
            let there = layer_of(&dep).unwrap();
            if there > here {
                violations.push(format!("{module} (L{here}) -> {dep} (L{there})"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "upward-dependency violations:\n  {}",
        violations.join("\n  ")
    );
}

/// Rule 3: every module is registered, so the layering can't rot when one is added.
#[test]
fn every_module_is_registered() {
    let mut unregistered = BTreeSet::new();
    for (rel, _) in all_src(env!("CARGO_MANIFEST_DIR")) {
        if let Some(module) = module_of(&rel)
            && layer_of(&module).is_none()
        {
            unregistered.insert(module);
        }
    }
    assert!(
        unregistered.is_empty(),
        "unregistered modules (add to CLI_LAYERS): {unregistered:?}"
    );
}

/// Rule 4: the filesystem and the standard streams belong to one module.
///
/// This is what makes `permission denied: /path/foo.webp` renderable — the codec's
/// own `Error::Io` carries only an `ErrorKind`, with no path and no message, so any
/// I/O routed through the library loses both.
#[test]
fn io_stays_in_its_module() {
    let mut violations = Vec::new();
    for (rel, code) in all_src(env!("CARGO_MANIFEST_DIR")) {
        let Some(module) = module_of(&rel) else {
            continue;
        };
        if IO_OWNERS.contains(&module.as_str()) {
            continue;
        }
        for call in IO_CALLS {
            if code.contains(call) {
                violations.push(format!(
                    "{} calls `{call}` — route it through `crate::io`",
                    slashed(&rel)
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "I/O ownership violations:\n  {}",
        violations.join("\n  ")
    );
}
