//! Metadata-extraction conformance (tool-free, reproducible).
//!
//! Any decode fixture that ships `expected.{icc,exif,xmp}` alongside `input.webp`
//! is an extended (`VP8X`) container authored by libwebp `webpmux` at
//! generation time. `webpkit::lossless::decode` must recover the embedded metadata bytes
//! exactly — validating our VP8X / chunk parser against libwebp's container
//! writer, without invoking any tool here.

use std::path::Path;

#[test]
fn metadata_fixtures_extract_exactly() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/decode");
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));

    let mut checked = 0;
    for entry in entries {
        let case = entry.unwrap().path();
        let input = case.join("input.webp");
        let goldens = [
            ("ICC", case.join("expected.icc")),
            ("Exif", case.join("expected.exif")),
            ("XMP", case.join("expected.xmp")),
        ];
        // Only cases shipping at least one metadata golden participate.
        if !input.exists() || !goldens.iter().any(|(_, path)| path.exists()) {
            continue;
        }

        let webp = std::fs::read(&input).unwrap();
        let image = webpkit::lossless::decode(&webp)
            .unwrap_or_else(|e| panic!("{}: webpkit::lossless decode failed: {e}", case.display()));
        let metadata = image.metadata();
        let got = [
            metadata.icc_profile.as_deref(),
            metadata.exif.as_deref(),
            metadata.xmp.as_deref(),
        ];

        for ((kind, golden), got) in goldens.iter().zip(got) {
            if golden.exists() {
                let expected = std::fs::read(golden).unwrap();
                assert_eq!(
                    got,
                    Some(expected.as_slice()),
                    "{}: {kind} metadata mismatch",
                    case.display()
                );
            } else {
                assert_eq!(got, None, "{}: unexpected {kind} metadata", case.display());
            }
        }
        checked += 1;
    }

    // The committed corpus ships VP8X metadata cases, so zero checked means the
    // participation filter stopped matching — a silent skip of the whole VP8X
    // metadata parser, not an empty directory. Every sibling conformance test
    // asserts this; this one only warned, so it could pass while testing nothing.
    assert!(
        checked > 0,
        "no VP8X metadata fixture participated; the corpus is expected to ship them \
         (run `cargo xtask gen-fixtures` if the fixtures are genuinely absent)"
    );
}
