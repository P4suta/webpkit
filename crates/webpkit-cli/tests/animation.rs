//! Integration tests for animation decoding, driven by a committed fixture.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;
use predicates::str::contains;

/// A committed 16x16, 3-frame animation from the conformance fixtures.
const ANIM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../webpkit-lossless-conformance/fixtures/decode/animation_frames/input.webp"
);

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

#[test]
fn info_reports_the_animation_frame_count() {
    webp()
        .args(["info", ANIM])
        .assert()
        .success()
        .stdout(contains("animation"))
        .stdout(contains("Frames:     3"));
}

#[test]
fn decode_frames_all_writes_numbered_files() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("frame.png");
    webp()
        .args(["decode", ANIM, "--frames", "all", "-o"])
        .arg(&target)
        .assert()
        .success();

    for index in 0..3 {
        let path = out.path().join(format!("frame-{index:03}.png"));
        assert!(path.is_file(), "missing {}", path.display());
    }
}

#[test]
fn decode_single_frame_writes_one_file() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("one.png");
    webp()
        .args(["decode", ANIM, "--frame", "1", "-o"])
        .arg(&target)
        .assert()
        .success();
    assert!(target.is_file());
}

#[test]
fn decode_out_of_range_frame_is_a_usage_error() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("x.png");
    webp()
        .args(["decode", ANIM, "--frame", "99", "-o"])
        .arg(&target)
        .assert()
        .code(2);
}

/// `--layout` reached the still path and not the animation path, so
/// `webp decode anim.webp --format raw --layout bgra8` wrote RGBA bytes and
/// exited 0. Nothing caught it: the two paths took different arguments, so the
/// compiler could not notice the omission and no test compared them.
#[test]
fn layout_is_honored_for_animations_as_it_is_for_stills() {
    let dir = tempfile::tempdir().expect("tempdir");
    let raw = |name: &str, layout: &str| {
        let out = dir.path().join(name);
        webp()
            .args(["decode", ANIM, "--frame", "0", "--format", "raw"])
            .args(["--layout", layout])
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        std::fs::read(&out).expect("read output")
    };

    let rgba = raw("rgba.raw", "rgba8");
    let bgra = raw("bgra.raw", "bgra8");

    assert_eq!(rgba.len(), bgra.len(), "same pixels, different order");
    assert_ne!(rgba, bgra, "--layout bgra8 must not emit rgba8 bytes");
    // Byte 0 is R in rgba8 and B in bgra8: the first pixel's channels swap.
    assert_eq!(rgba[0], bgra[2], "R and B are exchanged");
    assert_eq!(rgba[2], bgra[0], "R and B are exchanged");
    assert_eq!(rgba[3], bgra[3], "alpha stays put");
}

/// A truncated animation still describes itself.
///
/// `info` used to fail outright here: counting frames meant decoding every
/// frame's pixels, so one bad frame killed a report whose every other line was
/// already computed. A still in the same state reported fine — the asymmetry was
/// the tell. Both facts are in the `ANIM`/`ANMF` headers, which survive.
#[test]
fn info_describes_a_truncated_animation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let whole = std::fs::read(ANIM).expect("read fixture");
    let cut = dir.path().join("cut.webp");
    std::fs::write(&cut, &whole[..whole.len() * 7 / 10]).expect("write truncated");

    // The premise: the frames past the break really are gone.
    webp()
        .args(["decode", "--frames", "all"])
        .arg(&cut)
        .arg("-o")
        .arg(dir.path().join("out.png"))
        .assert()
        .failure();

    // The point: the file describes itself anyway, and says how much survived.
    webp()
        .arg("info")
        .arg(&cut)
        .assert()
        .success()
        .stdout(contains("Canvas:     16x16"))
        .stdout(contains("animation"))
        .stdout(contains("Frames:     1"));
}

/// Reaching frame 0 must not require frames it cannot reach. The eager collect
/// decoded every frame before using one, so a partially-downloaded animation
/// gave an error instead of the frame that had arrived.
#[test]
fn the_first_frame_of_a_truncated_animation_still_decodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let whole = std::fs::read(ANIM).expect("read fixture");
    let cut = dir.path().join("cut.webp");
    std::fs::write(&cut, &whole[..whole.len() * 7 / 10]).expect("write truncated");

    webp()
        .arg("decode")
        .arg(&cut)
        .arg("-o")
        .arg(dir.path().join("first.png"))
        .assert()
        .success();
}

/// Decode the fixture animation's three frames to PNG, returning their paths.
fn fixture_frames_as_png(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    (0..3)
        .map(|i| {
            let png = dir.join(format!("src-{i}.png"));
            webp()
                .args(["decode", ANIM, "--frame"])
                .arg(i.to_string())
                .arg("-o")
                .arg(&png)
                .assert()
                .success();
            png
        })
        .collect()
}

/// `animate` assembles stills into an animation whose frame count, per-frame delay,
/// and loop count are read back by `info`, and whose frames decode to the inputs.
#[test]
fn animate_assembles_stills_and_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pngs = fixture_frames_as_png(dir.path());
    let out = dir.path().join("assembled.webp");

    webp()
        .arg("animate")
        .args(&pngs)
        .args(["--delay", "40,60,80", "--loop", "2", "-o"])
        .arg(&out)
        .assert()
        .success();

    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("Frames:     3"))
        .stdout(contains("Loop:       2 time(s)"))
        .stdout(contains("40 ms"))
        .stdout(contains("80 ms"));

    // Each assembled frame decodes back to the exact input PNG (lossless default).
    for (i, src) in pngs.iter().enumerate() {
        let back = dir.path().join(format!("back-{i}.png"));
        webp()
            .args(["decode"])
            .arg(&out)
            .args(["--frame"])
            .arg(i.to_string())
            .arg("-o")
            .arg(&back)
            .assert()
            .success();
        assert_eq!(
            std::fs::read(src).expect("read src"),
            std::fs::read(&back).expect("read back"),
            "frame {i} must round-trip pixel-identically"
        );
    }
}

/// A translucent still animated then decoded keeps its alpha, so `info` reports it.
#[test]
fn animate_preserves_alpha() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A 2x2 raw RGBA image with a non-opaque alpha (0x80), encoded to WebP then
    // decoded to a PNG that carries the alpha.
    let raw = dir.path().join("a.rgba");
    std::fs::write(&raw, [10u8, 20, 30, 0x80].repeat(4)).expect("write raw");
    let webp_still = dir.path().join("a.webp");
    webp()
        .args([
            "encode",
            "--input-format",
            "raw",
            "--width",
            "2",
            "--height",
            "2",
            "-o",
        ])
        .arg(&webp_still)
        .arg(&raw)
        .assert()
        .success();
    let png = dir.path().join("a.png");
    webp()
        .args(["decode"])
        .arg(&webp_still)
        .arg("-o")
        .arg(&png)
        .assert()
        .success();

    let out = dir.path().join("alpha-anim.webp");
    webp()
        .args(["animate"])
        .arg(&png)
        .arg(&png)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("Alpha:      yes"));
}

/// `mux set --loop` edits the ANIM header while every frame's bytes pass through
/// verbatim (proven by extracting a frame from both files and byte-comparing).
#[test]
fn mux_set_loop_keeps_frames_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let edited = dir.path().join("edited.webp");
    webp()
        .args(["mux", "set", ANIM, "--loop", "9", "-o"])
        .arg(&edited)
        .assert()
        .success();
    webp()
        .args(["info"])
        .arg(&edited)
        .assert()
        .success()
        .stdout(contains("Loop:       9 time(s)"));

    // Frame 1 extracted from the original and from the edited file are identical:
    // the loop edit did not touch any frame's encoded bytes.
    let from_original = dir.path().join("orig-f1.webp");
    let from_edited = dir.path().join("edit-f1.webp");
    webp()
        .args(["mux", "get-frame", ANIM, "1", "-o"])
        .arg(&from_original)
        .assert()
        .success();
    webp()
        .args(["mux", "get-frame"])
        .arg(&edited)
        .args(["1", "-o"])
        .arg(&from_edited)
        .assert()
        .success();
    assert_eq!(
        std::fs::read(&from_original).expect("read orig frame"),
        std::fs::read(&from_edited).expect("read edited frame"),
        "an untouched frame must pass through verbatim"
    );
}

/// `mux remove` then `mux insert` of the extracted frame reconstructs the original
/// animation byte-for-byte — the strongest lossless-passthrough proof.
#[test]
fn mux_remove_then_insert_reconstructs_the_original() {
    let dir = tempfile::tempdir().expect("tempdir");
    let frame = dir.path().join("f1.webp");
    let removed = dir.path().join("removed.webp");
    let reinserted = dir.path().join("reinserted.webp");

    webp()
        .args(["mux", "get-frame", ANIM, "1", "-o"])
        .arg(&frame)
        .assert()
        .success();
    webp()
        .args(["mux", "remove", ANIM, "1", "-o"])
        .arg(&removed)
        .assert()
        .success();
    webp()
        .args(["mux", "insert"])
        .arg(&removed)
        .arg(&frame)
        .args(["--at", "1", "-o"])
        .arg(&reinserted)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(ANIM).expect("read original"),
        std::fs::read(&reinserted).expect("read reinserted"),
        "remove + reinsert must reconstruct the original bytes"
    );
}

/// `mux remove` drops a frame and the result decodes with one fewer frame.
#[test]
fn mux_remove_drops_a_frame() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("two.webp");
    webp()
        .args(["mux", "remove", ANIM, "0", "-o"])
        .arg(&out)
        .assert()
        .success();
    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("Frames:     2"));
}

/// `info` reports the per-frame table and the background color for an animation.
#[test]
fn info_reports_per_frame_table_and_background() {
    webp()
        .args(["info", ANIM])
        .assert()
        .success()
        .stdout(contains("Background: #"))
        .stdout(contains("frame 0:"))
        .stdout(contains("frame 2:"))
        .stdout(contains("lossless"));
}

/// `info --json` includes the per-frame array, the background, and the schema bump.
#[test]
fn info_json_carries_frames_and_schema_two() {
    webp()
        .args(["info", ANIM, "--json"])
        .assert()
        .success()
        .stdout(contains("\"schema\": 2"))
        .stdout(contains("\"frames\":"))
        .stdout(contains("\"codec\": \"lossless\""))
        .stdout(contains("\"background\":"))
        .stdout(contains("\"duration_ms\":"));
}
