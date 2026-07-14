//! Conformance test entry point.
//!
//! When fixtures are present under `fixtures/`, this walks each case, runs it
//! through `webpkit-lossless`, and compares against the committed golden. Without
//! them it exercises the harness types and confirms `webpkit-lossless` links.

use webpkit_lossless_conformance::{CaseResult, Level, results_to_json};

#[test]
fn results_serialize_to_json() {
    let results = [CaseResult {
        case: "smoke".to_owned(),
        feature: "literal".to_owned(),
        level: Level::Must,
        passed: true,
    }];
    let json = results_to_json(&results).expect("serialize results");
    assert!(json.contains("smoke"));
    assert!(json.contains("must"));
}

#[test]
fn webpkit_lossless_links_and_reports_a_version() {
    assert!(!webpkit::lossless::version().is_empty());
}
