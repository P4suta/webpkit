//! Generic committed-ledger diff machinery shared by the `metrics` / `work` /
//! `metrics --lossy` explain modes: JSON loading, the per-field delta record, and
//! the field-level rollup printer. The per-ledger divergence/explain specializations
//! live with their ledger structs in the respective subcommand modules.

use std::path::Path;

use anyhow::{Context, Result};

/// Load and parse a committed JSON ledger, with path context on failure. Used by
/// the `--explain` diff modes; the gate/bless paths compare raw text and parse
/// only on mismatch, so they do not share this.
pub(crate) fn load_ledger<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// One changed integer field of one ledger case: `old -> new`.
pub(crate) struct FieldDelta {
    pub(crate) case: String,
    pub(crate) field: String,
    pub(crate) old: u64,
    pub(crate) new: u64,
}

/// Print a field-level ledger diff — every changed `(case, field): old -> new`
/// with its signed delta, then a per-field rollup (how many cases moved and the
/// net change). The reusable core of `metrics --explain` / `work --explain`,
/// replacing throwaway `git diff corpus/*.json | grep` inspection.
pub(crate) fn print_field_deltas(deltas: &[FieldDelta]) {
    if deltas.is_empty() {
        println!("  no field changed (any diff is textual formatting only)");
        return;
    }
    println!(
        "  {:<30} {:<20} {:>14} {:>14} {:>13}",
        "case", "field", "old", "new", "delta"
    );
    for d in deltas {
        let delta = i128::from(d.new) - i128::from(d.old);
        println!(
            "  {:<30} {:<20} {:>14} {:>14} {delta:>+13}",
            d.case, d.field, d.old, d.new
        );
    }
    println!("  --- per-field rollup ---");
    let mut fields: Vec<&str> = deltas.iter().map(|d| d.field.as_str()).collect();
    fields.sort_unstable();
    fields.dedup();
    for f in fields {
        let rows = || deltas.iter().filter(|d| d.field == f);
        let count = rows().count();
        let net: i128 = rows().map(|d| i128::from(d.new) - i128::from(d.old)).sum();
        println!("  {f:<20} {count:>3} case(s)  net {net:+}");
    }
}
