//! Verbosity-aware status reporting.
//!
//! Every human-readable line goes to **stderr** so that `-o -` can stream image
//! bytes to stdout untouched. `-q`/`--quiet` silences everything but errors;
//! `-v`/`--verbose` adds per-stage detail.
//!
//! Printing goes through `anstream`, which drops the styling when stderr is not
//! a terminal. Styles are therefore written unconditionally here — whether they
//! survive is [`crate::term`]'s decision, made once at startup, and this module
//! never asks.

use std::fmt::Write as _;

use anstyle::Style;

use crate::{diag::Diagnostic, term};

/// How much status output to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// Errors only.
    Quiet,
    /// A one-line summary per operation (the default).
    Normal,
    /// Per-stage detail.
    Verbose,
}

/// Emits status, detail, and error lines to stderr according to a [`Level`].
#[derive(Debug)]
pub(crate) struct Reporter {
    level: Level,
    dry_run: bool,
}

impl Reporter {
    /// Build a reporter from the parsed `-v` count and `-q` flag. `quiet` wins.
    #[must_use]
    pub(crate) const fn new(verbose: u8, quiet: bool) -> Self {
        let level = if quiet {
            Level::Quiet
        } else if verbose >= 1 {
            Level::Verbose
        } else {
            Level::Normal
        };
        Self {
            level,
            dry_run: false,
        }
    }

    /// Mark this reporter as a `--dry-run`: callers skip writing and report the
    /// plan instead.
    #[must_use]
    pub(crate) const fn dry(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Whether this is a dry run — no encoding, no writing, just the plan.
    #[must_use]
    pub(crate) const fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// A one-line summary, shown at [`Level::Normal`] and above.
    pub(crate) fn status(&self, message: &str) {
        if self.level != Level::Quiet {
            line(message);
        }
    }

    /// A per-stage detail line, shown only at [`Level::Verbose`].
    pub(crate) fn detail(&self, message: &str) {
        if self.level == Level::Verbose {
            line(message);
        }
    }
}

/// Render a diagnostic to stderr, rustc-style: an `error:` headline, an optional
/// argv caret, then `cause` / `help` / `note` blocks.
///
/// A free function rather than a [`Reporter`] method because errors are always
/// shown — even under `--quiet` — so verbosity never applies. Styles are written
/// unconditionally; [`crate::term`] decided at startup whether `anstream` keeps
/// them.
pub(crate) fn error(diag: &Diagnostic) {
    let head = term::error();
    let label = Style::new().bold();
    let mut out = String::new();
    let _ = write!(out, "{head}error:{head:#} {}", diag.title());

    let has_body = diag.cause().is_some() || !diag.help().is_empty() || !diag.notes().is_empty();
    if let Some(span) = diag.span() {
        let pad = " ".repeat(span.start());
        let caret = "^".repeat(span.width());
        let _ = write!(out, "\n\n  {}\n  {pad}{head}{caret}{head:#}", span.line());
        // A blank line sets the cause/help/note block off from the caret, the way
        // rustc separates its underline from the notes below it.
        if has_body {
            out.push('\n');
        }
    }

    if let Some(cause) = diag.cause() {
        push_labeled(
            &mut out,
            label,
            "cause",
            &cause.split('\n').collect::<Vec<_>>(),
        );
    }
    let help: Vec<&str> = diag
        .help()
        .iter()
        .flat_map(|line| line.split('\n'))
        .collect();
    push_labeled(&mut out, label, "help", &help);
    for note in diag.notes() {
        push_labeled(
            &mut out,
            label,
            "note",
            &note.split('\n').collect::<Vec<_>>(),
        );
    }
    block(&out);
}

/// Append a `label: lines...` block, with continuation lines aligned under the
/// first. A no-op when `lines` is empty, so an absent block prints nothing.
fn push_labeled(out: &mut String, style: Style, label: &str, lines: &[&str]) {
    let indent = " ".repeat(label.len() + 4); // "  " + label + ": "
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            let _ = write!(out, "\n  {style}{label}:{style:#} {line}");
        } else {
            let _ = write!(out, "\n{indent}{line}");
        }
    }
}

/// Print a non-fatal warning to stderr (always shown), prefixed with `warning:`.
pub(crate) fn warn(message: &str) {
    labeled(term::warning(), "warning", message);
}

/// A `dry run:` plan line to stderr, always shown — it is the whole output of a
/// dry run, so `--quiet` does not silence it.
pub(crate) fn plan(message: &str) {
    line(&format!("dry run: {message}"));
}

/// Print a report line to **stdout**.
///
/// stdout carries either image bytes or a report, never both, so this is only
/// for commands that produce no image: `info`, `--help`, `--version`. Status,
/// progress, and diagnostics never come here — they would corrupt a `-o -` pipe.
#[allow(
    clippy::print_stdout,
    reason = "this module is the CLI's one stdout writer; anstream::println shadows \
              the std macro, so the lint fires here and only here"
)]
pub(crate) fn out(message: &str) {
    use anstream::println;
    println!("{message}");
}

/// `<label>: <message>`, with the label styled.
#[allow(
    clippy::print_stderr,
    reason = "this module is the CLI's one stderr writer; anstream::eprintln shadows \
              the std macro, so the lint fires here and only here"
)]
fn labeled(style: Style, label: &str, message: &str) {
    use anstream::eprintln;
    eprintln!("{style}{label}:{style:#} {message}");
}

#[allow(
    clippy::print_stderr,
    reason = "this module is the CLI's one stderr writer; anstream::eprintln shadows \
              the std macro, so the lint fires here and only here"
)]
fn line(message: &str) {
    use anstream::eprintln;
    eprintln!("{message}");
}

/// Print a pre-assembled multi-line block to stderr.
#[allow(
    clippy::print_stderr,
    reason = "this module is the CLI's one stderr writer; anstream::eprintln shadows \
              the std macro, so the lint fires here and only here"
)]
fn block(text: &str) {
    use anstream::eprintln;
    eprintln!("{text}");
}
