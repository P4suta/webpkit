//! Animation conformance (tool-free, reproducible).
//!
//! Any decode fixture that ships `expected.frame0.rgba` alongside `input.webp` is
//! a libwebp `img2webp`-authored `ANIM`/`ANMF` animation. Its `expected.frameN.rgba`
//! goldens are each frame as decoded by libwebp (`webpmux -get frame` + `dwebp`).
//! `webpkit::lossless::decode_frames` must reproduce every frame — both the lazy per-frame
//! image and the composited canvas — byte for byte, validating our ANIM/ANMF
//! reader and compositor against libwebp without invoking any tool here.

use std::path::{Path, PathBuf};

/// Collect `expected.frame0.rgba`, `expected.frame1.rgba`, … in order until one
/// is missing.
fn frame_goldens(case: &Path) -> Vec<PathBuf> {
    let mut goldens = Vec::new();
    let mut n = 0;
    loop {
        let path = case.join(format!("expected.frame{n}.rgba"));
        if !path.exists() {
            break;
        }
        goldens.push(path);
        n += 1;
    }
    goldens
}

#[test]
fn animation_fixtures_decode_frame_exact() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/decode");
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));

    let mut checked = 0;
    for entry in entries {
        let case = entry.unwrap().path();
        let input = case.join("input.webp");
        let goldens = frame_goldens(&case);
        // Only cases shipping per-frame goldens participate.
        if !input.exists() || goldens.is_empty() {
            continue;
        }
        let expected: Vec<Vec<u8>> = goldens.iter().map(|g| std::fs::read(g).unwrap()).collect();

        let webp = std::fs::read(&input).unwrap();

        // Lazy per-frame decode must match every golden.
        let frames: Vec<webpkit::lossless::Frame> = webpkit::lossless::decode_frames(&webp)
            .unwrap_or_else(|e| panic!("{}: decode_frames failed: {e}", case.display()))
            .collect::<webpkit::lossless::Result<_>>()
            .unwrap_or_else(|e| panic!("{}: a frame failed to decode: {e}", case.display()));
        assert_eq!(
            frames.len(),
            expected.len(),
            "{}: frame count mismatch",
            case.display()
        );
        for (n, (frame, want)) in frames.iter().zip(&expected).enumerate() {
            assert_eq!(
                frame.image().as_bytes(),
                want.as_slice(),
                "{}: frame {n} pixels mismatch",
                case.display()
            );
        }

        // Compositing full-canvas frames yields each frame unchanged; it must
        // also match the goldens (exercising the compositor's copy path).
        let composited: Vec<webpkit::lossless::CompositedFrame> =
            webpkit::lossless::decode_frames(&webp)
                .unwrap()
                .composited()
                .collect::<webpkit::lossless::Result<_>>()
                .unwrap_or_else(|e| panic!("{}: compositing failed: {e}", case.display()));
        for (n, (frame, want)) in composited.iter().zip(&expected).enumerate() {
            assert_eq!(
                frame.image().as_bytes(),
                want.as_slice(),
                "{}: composited frame {n} mismatch",
                case.display()
            );
        }

        checked += 1;
    }

    // Fail closed: a committed animation fixture must exist, so a fixture that
    // silently disappears (or a broken discovery walk) turns this test red rather
    // than passing vacuously.
    assert!(
        checked >= 1,
        "no animation fixtures with per-frame goldens found under {}; \
         run `cargo xtask gen-fixtures`",
        dir.display()
    );
}
