//! Harness smoke tests.
//!
//! The fixture walk lives in `decode.rs`, `encode.rs`, `metadata.rs` and
//! `ledger.rs` — this file only checks that the harness types serialize and that
//! `webpkit` links. (It once claimed to be the fixture entry point; it never was.)

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
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let first = &parsed[0];
    assert_eq!(first["case"], "smoke");
    assert_eq!(first["level"], "must");
    assert_eq!(first["passed"], true);
}

#[test]
fn webpkit_links_and_reports_a_version() {
    assert!(!webpkit::version().is_empty());
}
