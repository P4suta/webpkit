//! Drift gate for the committed shell completions and man pages.
//!
//! `assets/` ships in the published tarball so packagers get completions and man
//! pages without building anything. That only works if the committed bytes match
//! what the binary would emit today, and nothing else checks: a flag added in
//! `brand.rs` changes every completion script, silently.
//!
//! So this regenerates each artifact and byte-compares. On failure, run
//! `just gen-assets`, then read the diff — it is the list of what your flag change
//! did to the shell scripts.
//!
//! The same shape as the repo's other committed-artifact gates (`corpus/*.json`,
//! the conformance ledgers): generate, compare, tell the author how to re-bless.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Every artifact `just gen-assets` writes: (relative path under `assets/`, the
/// `webp` arguments that produce it).
///
/// Adding a shell or a subcommand means adding a row here *and* to `gen-assets`.
/// The two are checked against each other by [`the_ledger_covers_every_asset`],
/// so they cannot drift apart unnoticed.
fn assets() -> Vec<(PathBuf, Vec<&'static str>)> {
    let mut rows: Vec<(PathBuf, Vec<&'static str>)> =
        ["bash", "zsh", "fish", "powershell", "elvish"]
            .into_iter()
            .map(|shell| {
                (
                    Path::new("completions").join(format!("webp.{shell}")),
                    vec!["completions", shell],
                )
            })
            .collect();
    rows.push((Path::new("man").join("webp.1"), vec!["man"]));
    for command in [
        "encode",
        "decode",
        "convert",
        "info",
        "config",
        "explain",
        "completions",
        "man",
    ] {
        rows.push((
            Path::new("man").join(format!("webp-{command}.1")),
            vec!["man", command],
        ));
    }
    rows
}

fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// What `webp <args>` prints on stdout today.
fn generate(args: &[&str]) -> String {
    let output = Command::cargo_bin("webp")
        .expect("webp builds")
        .args(args)
        .output()
        .expect("run webp");
    assert!(
        output.status.success(),
        "`webp {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("generated assets are UTF-8")
}

/// Compare ignoring the trailing newline and any CRLF a checkout introduced.
///
/// The bytes that matter are the script's; `.gitattributes` pins the working tree
/// to LF, but a mis-set `core.autocrlf` should fail on the real content, not on
/// line endings.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_owned()
}

#[test]
fn committed_assets_match_the_binary() {
    let mut stale = Vec::new();
    for (relative, args) in assets() {
        let path = assets_dir().join(&relative);
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        if normalize(&committed) != normalize(&generate(&args)) {
            stale.push(relative.display().to_string().replace('\\', "/"));
        }
    }
    assert!(
        stale.is_empty(),
        "these committed assets no longer match the binary: {stale:?}\n\
         Run `just gen-assets` and review the diff — it is what your flag change \
         did to the shell scripts and man pages."
    );
}

/// Nothing in `assets/` is orphaned, and nothing in the table is missing from
/// disk. Without this, deleting a subcommand would leave its man page shipping
/// forever, describing a command that no longer exists.
#[test]
fn the_ledger_covers_every_asset() {
    let expected: Vec<String> = assets()
        .into_iter()
        .map(|(relative, _)| relative.display().to_string().replace('\\', "/"))
        .collect();

    let mut found = Vec::new();
    for sub in ["completions", "man"] {
        let dir = assets_dir().join(sub);
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|err| panic!("read {sub}: {err}")) {
            let name = entry.expect("dir entry").file_name();
            found.push(format!("{sub}/{}", name.to_string_lossy()));
        }
    }

    let mut orphaned: Vec<&String> = found.iter().filter(|f| !expected.contains(f)).collect();
    orphaned.sort();
    assert!(
        orphaned.is_empty(),
        "assets on disk that no command produces (delete them, or add a row): {orphaned:?}"
    );

    let mut missing: Vec<&String> = expected.iter().filter(|e| !found.contains(e)).collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "assets in the table but not on disk (run `just gen-assets`): {missing:?}"
    );
}
