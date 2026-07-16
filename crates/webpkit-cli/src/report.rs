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

use anstyle::Style;

use crate::{error::CliError, term};

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
        Self { level }
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

/// Print an error line to stderr, prefixed with `error:`.
///
/// A free function rather than a [`Reporter`] method because errors are always
/// shown — even under `--quiet` — so verbosity never applies.
pub(crate) fn error(err: &CliError) {
    labeled(term::error(), "error", &err.to_string());
}

/// Print a non-fatal warning to stderr (always shown), prefixed with `warning:`.
pub(crate) fn warn(message: &str) {
    labeled(term::warning(), "warning", message);
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
