//! Snapshot tests pinning the `--help` output of each binary.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;

/// The binary's help text, with the platform `.exe` suffix stripped so the
/// snapshot is identical on Windows and Unix.
fn help(bin: &str, flag: &str) -> String {
    let output = Command::cargo_bin(bin)
        .expect("binary builds")
        .arg(flag)
        .output()
        .expect("run help");
    String::from_utf8_lossy(&output.stdout).replace(".exe", "")
}

#[test]
fn webp_help() {
    insta::assert_snapshot!("webp", help("webp", "--help"));
}

#[test]
fn cwebp_help() {
    insta::assert_snapshot!("cwebp", help("cwebp", "-h"));
}

#[test]
fn dwebp_help() {
    insta::assert_snapshot!("dwebp", help("dwebp", "-h"));
}
