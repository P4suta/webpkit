//! Comment-hygiene gate (`lint-comments`): fail on narrative / history / measurement-boast phrasing in library and CLI source comments.
//! The record for *why* a value was chosen belongs in Linear/ADR; a comment states the constraint/reason in one line.
//! Deliberately small and conservative — it flags a handful of unambiguous phrases, never legitimate constraint comments.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::common::workspace_root;

/// Source trees scanned by the gate (product library + CLI), relative to the root.
const SCANNED_DIRS: [&str; 2] = ["crates/webpkit/src", "crates/webpkit-cli/src"];

/// A forbidden comment rule, matched case-insensitively against one comment line.
enum Rule {
    /// The lowercased comment contains this exact phrase.
    Phrase(&'static str),
    /// The lowercased comment has `first` as a whole word strictly before a later whole-word `second` (ordered co-occurrence, so "win is measured" is exempt).
    Ordered(&'static str, &'static str),
}

/// The forbidden phrase set, each paired with the class it belongs to.
/// Kept small and unambiguous so the gate never fires on a legitimate constraint comment.
const FORBIDDEN: &[(Rule, &str)] = &[
    // Measurement-boast: a benchmarked "win" claim is Linear/ADR's job, not a comment's.
    (Rule::Ordered("measured", "win"), "measurement-boast"),
    // Comparison-boast against another encoder.
    (Rule::Phrase("pareto win"), "comparison-boast"),
    // Code-history narrative: what the code used to do is recoverable from git.
    (Rule::Phrase("we used to"), "code-history"),
    (Rule::Phrase("in the past"), "code-history"),
    // Audience narrative in place of a constraint/reason.
    (Rule::Phrase("for our consumers"), "audience-narrative"),
];

/// Scan the library and CLI source comments; fail (nonzero) on any forbidden hit.
pub(crate) fn lint_comments() -> Result<()> {
    let root = workspace_root()?;
    let mut files = Vec::new();
    for dir in SCANNED_DIRS {
        collect_rs(&root.join(dir), &mut files)?;
    }
    files.sort();

    let mut hits = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        for (lineno, line) in text.lines().enumerate() {
            let Some(comment) = comment_text(line) else {
                continue;
            };
            let lower = comment.to_ascii_lowercase();
            for (rule, class) in FORBIDDEN {
                if rule.matches(&lower) {
                    hits += 1;
                    let rel = file.strip_prefix(&root).unwrap_or(file);
                    println!(
                        "{}:{} [{class}] {}",
                        rel.display(),
                        lineno + 1,
                        comment.trim()
                    );
                    break;
                }
            }
        }
    }

    if hits > 0 {
        bail!(
            "lint-comments: {hits} forbidden comment(s) — state the constraint in one line, \
             record the rationale in Linear/ADR"
        );
    }
    println!("lint-comments: {} files clean", files.len());
    Ok(())
}

/// Recursively gather `*.rs` paths under `dir` into `out`.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// The line-comment text of `line` (everything after the first `//`), or `None`.
/// A pragmatic scan: it does not track block comments or `//` inside string literals, which the forbidden phrases never appear in.
fn comment_text(line: &str) -> Option<&str> {
    line.find("//").map(|i| &line[i + 2..])
}

impl Rule {
    /// Whether this rule matches the already-lowercased comment text.
    fn matches(&self, lower: &str) -> bool {
        match *self {
            Self::Phrase(p) => lower.contains(p),
            Self::Ordered(a, b) => {
                word_index(lower, a, 0).is_some_and(|end| word_index(lower, b, end).is_some())
            },
        }
    }
}

/// Start index of the first whole-word `word` in `hay` at or after `from`, where a whole word is not flanked by ASCII alphanumerics (so "win" ignores "window").
/// Returns the index just past the match, for chaining ordered lookups.
fn word_index(hay: &str, word: &str, from: usize) -> Option<usize> {
    let bytes = hay.as_bytes();
    let mut i = from;
    while let Some(rel) = hay[i..].find(word) {
        let start = i + rel;
        let end = start + word.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return Some(end);
        }
        i = start + 1;
    }
    None
}
