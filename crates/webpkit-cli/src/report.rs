//! Verbosity-aware status reporting.
//!
//! Every human-readable line goes to **stderr** so that `-o -` can stream image
//! bytes to stdout untouched. `-q`/`--quiet` silences everything but errors;
//! `-v`/`--verbose` adds per-stage detail.

use crate::error::CliError;

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
    #[allow(
        clippy::print_stderr,
        reason = "the CLI reports human-readable status on stderr by design"
    )]
    pub(crate) fn status(&self, message: &str) {
        if self.level != Level::Quiet {
            eprintln!("{message}");
        }
    }

    /// A per-stage detail line, shown only at [`Level::Verbose`].
    #[allow(
        clippy::print_stderr,
        reason = "the CLI reports human-readable status on stderr by design"
    )]
    pub(crate) fn detail(&self, message: &str) {
        if self.level == Level::Verbose {
            eprintln!("{message}");
        }
    }
}

/// Print an error line to stderr, prefixed with `error:`.
///
/// A free function rather than a [`Reporter`] method because errors are always
/// shown — even under `--quiet` — so verbosity never applies.
#[allow(
    clippy::print_stderr,
    reason = "the CLI reports errors on stderr by design"
)]
pub(crate) fn error(err: &CliError) {
    eprintln!("error: {err}");
}

/// Print a non-fatal warning to stderr (always shown), prefixed with `warning:`.
#[allow(
    clippy::print_stderr,
    reason = "the CLI reports warnings on stderr by design"
)]
pub(crate) fn warn(message: &str) {
    eprintln!("warning: {message}");
}
