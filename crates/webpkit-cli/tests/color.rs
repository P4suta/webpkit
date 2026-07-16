//! Color policy, observed on real processes.
//!
//! The decision itself is a pure function unit-tested in `src/term.rs`; these
//! tests only check that the answer reaches the wire, because that is the part
//! a unit test cannot see. `assert_cmd` spawns over pipes, so stderr is never a
//! terminal here — which makes "plain by default" the case under test and every
//! assertion deterministic.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;

/// stderr of a failing run, with the environment cleared of the color variables
/// so a developer's own `NO_COLOR` cannot change the result.
fn stderr_of(bin: &str, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = Command::cargo_bin(bin).expect("binary builds");
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.args(args).output().expect("run binary");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const ESC: &str = "\u{1b}[";

/// An argument set that always fails, so there is always an `error:` to style.
const FAILING: [&str; 2] = ["info", "no-such-file.webp"];

#[test]
fn a_redirected_stream_is_plain_by_default() {
    assert!(!stderr_of("webp", &FAILING, &[]).contains(ESC));
}

#[test]
fn color_always_survives_redirection() {
    let err = stderr_of(
        "webp",
        &["info", "no-such-file.webp", "--color", "always"],
        &[],
    );
    assert!(err.contains(ESC), "expected ANSI in {err:?}");
}

#[test]
fn color_never_stays_plain_even_when_forced() {
    let err = stderr_of(
        "webp",
        &["info", "no-such-file.webp", "--color", "never"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert!(!err.contains(ESC), "expected no ANSI in {err:?}");
}

#[test]
fn clicolor_force_colors_a_pipe() {
    let err = stderr_of("webp", &FAILING, &[("CLICOLOR_FORCE", "1")]);
    assert!(err.contains(ESC), "expected ANSI in {err:?}");
}

#[test]
fn no_color_is_honored_and_clicolor_force_overrides_it() {
    let plain = stderr_of("webp", &FAILING, &[("NO_COLOR", "1")]);
    assert!(
        !plain.contains(ESC),
        "NO_COLOR should stay plain: {plain:?}"
    );

    // The narrower request wins: "color even when piping" is an exception to a
    // blanket opt-out, so setting both asks for the exception.
    let forced = stderr_of(
        "webp",
        &FAILING,
        &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")],
    );
    assert!(
        forced.contains(ESC),
        "CLICOLOR_FORCE should win: {forced:?}"
    );
}

#[test]
fn an_empty_no_color_is_not_set() {
    // The convention is presence-with-a-value, so `NO_COLOR=` must not disable.
    let err = stderr_of(
        "webp",
        &FAILING,
        &[("NO_COLOR", ""), ("CLICOLOR_FORCE", "1")],
    );
    assert!(err.contains(ESC), "expected ANSI in {err:?}");
}

#[test]
fn a_dumb_terminal_opts_out() {
    let err = stderr_of("webp", &FAILING, &[("TERM", "dumb")]);
    assert!(!err.contains(ESC), "expected no ANSI in {err:?}");
}

/// The drop-ins get the same treatment, in their own grammar. A libwebp user who
/// never asked for color still gets it on a terminal, and never gets it in a log.
#[test]
fn the_dropins_accept_color_in_their_own_grammar() {
    let err = stderr_of("cwebp", &["-color", "always", "-near_lossless", "60"], &[]);
    assert!(err.contains(ESC), "cwebp -color always: {err:?}");

    let err = stderr_of("dwebp", &["-color", "always", "-yuv"], &[]);
    assert!(err.contains(ESC), "dwebp -color always: {err:?}");
}

#[test]
fn an_unknown_color_mode_is_a_usage_error() {
    let output = Command::cargo_bin("cwebp")
        .expect("binary builds")
        .args(["-color", "chartreuse", "-", "-o", "-"])
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("auto, always, or never"), "{err:?}");
}
