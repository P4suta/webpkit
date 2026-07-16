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
//! Like the sibling gate in `webpkit`, this is a source-scanning heuristic: each
//! line is truncated at its first `//`, so comments and intra-doc links do not
//! count as edges. It catches the accident, not every conceivable obfuscation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "architecture test scaffold: reads of the crate's own src/ should panic \
              loudly if the repo layout is broken"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Module layers. An edge may point at its own layer or lower, never higher.
///
/// - L0 vocabulary with no dependencies   L1 the outside world (bytes, pixels, terminal)
/// - L2 resolved intent                   L3 what to do    L4 doing it    L5 argv
const CLI_LAYERS: &[(&str, u8)] = &[
    ("error", 0),
    ("personality", 0),
    ("codec", 1),
    ("format", 1),
    ("io", 1),
    ("metadata", 1),
    ("report", 1),
    ("term", 1),
    ("effort", 2),
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
const FACADE_GAPS: &[(&str, &str)] = &[];

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

/// Drop each line's first `//`-comment so intra-doc links and comments don't count
/// as dependency edges.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// Rule 1: name codec types through the facade, not through `webpkit`'s hidden
/// modules.
#[test]
fn codec_types_come_from_the_facade() {
    let mut violations = Vec::new();
    for (rel, code) in all_src() {
        let file = rel.to_string_lossy().replace('\\', "/");
        for hidden in HIDDEN_MODULES {
            if !code.contains(&format!("webpkit::{hidden}::")) {
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
    for (rel, code) in all_src() {
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
    for (rel, _) in all_src() {
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
    for (rel, code) in all_src() {
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
                    rel.to_string_lossy().replace('\\', "/")
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
