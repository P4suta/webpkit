//! Integration tests for `webp config`, observed on real processes.
//!
//! The resolution fold is a pure function unit-tested in `src/config.rs`; these
//! tests check that each layer reaches the wire through the real binary — that a
//! `WEBP_*` variable is read, that a `webp.toml` value is read *with its line*,
//! and that the JSON is machine-parseable — which a unit test cannot see.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The env vars that would otherwise leak a developer's own config into a test.
const CONFIG_VARS: &[&str] = &[
    "WEBP_QUALITY",
    "WEBP_EFFORT",
    "WEBP_CODEC",
    "WEBP_METADATA",
    "WEBP_COLOR",
    "WEBP_THREADS",
    "WEBP_MAX_PIXELS",
];

/// A `webp` command with the config environment isolated: every `WEBP_*` cleared,
/// the user config directory pointed at an empty temp (so no real `webp.toml` is
/// found), and the working directory set to `cwd`.
fn isolated(cwd: &TempDir, empty_home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("webp").expect("binary builds");
    for var in CONFIG_VARS {
        cmd.env_remove(var);
    }
    // Cover every platform's config-home lookup with the same empty directory.
    cmd.env("XDG_CONFIG_HOME", empty_home.path())
        .env("APPDATA", empty_home.path())
        .env("HOME", empty_home.path())
        .current_dir(cwd.path());
    cmd
}

/// Run `webp config <args...>` and return parsed JSON stdout.
fn config_json(cmd: &mut Command, args: &[&str]) -> Value {
    let output = cmd
        .arg("config")
        .args(args)
        .arg("--json")
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "config --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is valid JSON")
}

#[test]
fn an_env_var_shows_as_the_env_origin() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let mut cmd = isolated(&cwd, &home);
    cmd.env("WEBP_QUALITY", "90");
    let json = config_json(&mut cmd, &[]);
    assert_eq!(json["quality"]["value"], Value::from(90));
    assert_eq!(json["quality"]["origin"]["source"], Value::from("env"));
    assert_eq!(
        json["quality"]["origin"]["name"],
        Value::from("WEBP_QUALITY")
    );
}

#[test]
fn a_config_file_value_shows_its_path_and_line() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    // The value is on line 3, so the origin must report line 3, not "the file".
    fs::write(
        cwd.path().join("webp.toml"),
        "# a header comment\n\nquality = 80\n",
    )
    .expect("write webp.toml");

    let json = config_json(&mut isolated(&cwd, &home), &[]);
    assert_eq!(json["quality"]["value"], Value::from(80));
    assert_eq!(json["quality"]["origin"]["source"], Value::from("file"));
    assert_eq!(json["quality"]["origin"]["line"], Value::from(3));
}

#[test]
fn an_argument_beats_the_environment() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let mut cmd = isolated(&cwd, &home);
    cmd.env("WEBP_QUALITY", "50");
    // The flag layer is highest; it must win and report the args origin.
    let json = config_json(&mut cmd, &["--quality", "77"]);
    assert_eq!(json["quality"]["value"], Value::from(77));
    assert_eq!(json["quality"]["origin"]["source"], Value::from("args"));
}

#[test]
fn nothing_set_resolves_to_the_default() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let json = config_json(&mut isolated(&cwd, &home), &[]);
    assert_eq!(json["quality"]["value"], Value::from(75));
    assert_eq!(json["quality"]["origin"]["source"], Value::from("default"));
}

#[test]
fn get_prints_a_bare_value() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let output = isolated(&cwd, &home)
        .args(["config", "get", "quality"])
        .output()
        .expect("run");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "75");
}

#[test]
fn get_rejects_an_unknown_setting_as_a_usage_error() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let output = isolated(&cwd, &home)
        .args(["config", "get", "not-a-setting"])
        .output()
        .expect("run");
    assert_eq!(
        output.status.code(),
        Some(2),
        "unknown key is a usage error"
    );
}

#[test]
fn a_malformed_config_file_is_reported_not_ignored() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    fs::write(cwd.path().join("webp.toml"), "quality = 200\n").expect("write webp.toml");
    let output = isolated(&cwd, &home)
        .args(["config", "get", "quality"])
        .output()
        .expect("run");
    assert!(!output.status.success(), "an out-of-range value must fail");
}

#[test]
fn the_template_lists_every_setting_and_is_valid_toml_when_uncommented() {
    let cwd = TempDir::new().expect("cwd");
    let home = TempDir::new().expect("home");
    let output = isolated(&cwd, &home)
        .args(["config", "--template"])
        .output()
        .expect("run");
    assert!(output.status.success());
    let template = String::from_utf8_lossy(&output.stdout);
    assert!(template.contains("# quality ="), "{template}");
    assert!(template.contains("# max_pixels ="), "{template}");

    // Uncomment every setting line and confirm the result parses as TOML, so the
    // template can never ship a value the tool would reject.
    let uncommented: String = template
        .lines()
        .map(|line| line.strip_prefix("# ").unwrap_or(line))
        .filter(|line| {
            // A real assignment: a bare `key` immediately before ` = `. This skips
            // the description comments, which may themselves contain ` = `.
            line.split_once(" = ").is_some_and(|(key, _)| {
                !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    toml::from_str::<toml::Table>(&uncommented)
        .unwrap_or_else(|err| panic!("template is not valid TOML: {err}\n{uncommented}"));
}

/// `webp config` must not lie: a setting it reports as applied must actually change
/// the encode. This is the invariant that failed — env/file settings were shown by
/// `config` yet ignored by every encode, so `WEBP_QUALITY=10 webp config` said 10
/// while the encode used the default.
#[test]
fn env_settings_reach_the_encode_not_just_the_config_report() {
    let dir = TempDir::new().expect("temp dir");
    // A small noisy PPM, so lossy vs lossless differ in size.
    let ppm = dir.path().join("in.ppm");
    let mut bytes = b"P6\n8 8\n255\n".to_vec();
    bytes.extend((0..8u32 * 8 * 3).map(|i| u8::try_from(i * 37 % 251).unwrap_or(0)));
    fs::write(&ppm, &bytes).expect("write ppm");

    let encode = |env: &[(&str, &str)], out: &str| {
        let path = dir.path().join(out);
        let mut cmd = Command::cargo_bin("webp").expect("binary");
        for var in CONFIG_VARS {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.args(["encode"])
            .arg(&ppm)
            .arg("-o")
            .arg(&path)
            .assert()
            .success();
        fs::read(&path).expect("read output")
    };

    let plain = encode(&[], "plain.webp");
    let lossy = encode(
        &[("WEBP_CODEC", "lossy"), ("WEBP_QUALITY", "10")],
        "env.webp",
    );
    assert_ne!(
        plain, lossy,
        "WEBP_CODEC/WEBP_QUALITY changed nothing — config is being ignored by encode"
    );

    // And a CLI flag still beats the environment.
    let cli = encode_with_args(
        dir.path(),
        &ppm,
        &[("WEBP_QUALITY", "10")],
        &["--lossy", "-q", "90"],
        "cli.webp",
    );
    assert_ne!(cli, lossy, "an explicit -q must override WEBP_QUALITY");
}

fn encode_with_args(
    dir: &std::path::Path,
    input: &std::path::Path,
    env: &[(&str, &str)],
    extra: &[&str],
    out: &str,
) -> Vec<u8> {
    let path = dir.join(out);
    let mut cmd = Command::cargo_bin("webp").expect("binary");
    for var in CONFIG_VARS {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(["encode"])
        .arg(input)
        .arg("-o")
        .arg(&path)
        .args(extra)
        .assert()
        .success();
    fs::read(&path).expect("read output")
}
