//! Terminal styling policy.
//!
//! The decision of *whether* to style is a pure function of a flag and the
//! environment ([`decide`]), so it is exhaustively testable without a terminal.
//! Only [`Env::detect`] touches the outside world, and only [`install`] acts on
//! the answer.
//!
//! Rendering itself belongs to `anstream`: it strips ANSI from a redirected
//! stream and, on Windows, deals with enabling virtual-terminal processing.

use std::io::IsTerminal as _;

use anstyle::{AnsiColor, Color, Style};
use clap::ValueEnum;

use crate::error::CliError;

/// When to emit ANSI styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum ColorChoice {
    /// Style only when the stream is a terminal that wants it (the default).
    #[default]
    Auto,
    /// Style even when the stream is redirected.
    Always,
    /// Never style.
    Never,
}

/// What the environment asks for, independent of any stream.
///
/// `CLICOLOR_FORCE` and `NO_COLOR` can both be set, so the two are resolved into
/// one answer here rather than left for [`decide`] to untangle: the conflict has
/// exactly one resolution, and this is where it is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// `CLICOLOR_FORCE` — color even when the stream is redirected.
    Force,
    /// `NO_COLOR` — no color anywhere.
    Suppress,
    /// Neither is set; the stream decides.
    Unset,
}

impl Request {
    /// Read `CLICOLOR_FORCE` and `NO_COLOR`, both of which count only when set to
    /// a non-empty value.
    ///
    /// `Force` wins when both are set: it is the narrower request — "color even
    /// when piping" — against a blanket opt-out, so a user who sets both is
    /// asking for the exception.
    fn detect() -> Self {
        let set = |key: &str| std::env::var_os(key).is_some_and(|value| !value.is_empty());
        if set("CLICOLOR_FORCE") {
            Self::Force
        } else if set("NO_COLOR") {
            Self::Suppress
        } else {
            Self::Unset
        }
    }
}

/// The environment facts [`decide`] consults, gathered so the decision is pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Env {
    /// Whether the stream being styled is a terminal.
    pub(crate) is_terminal: bool,
    /// `TERM` is `dumb`, i.e. the terminal cannot render escapes.
    pub(crate) dumb_term: bool,
    /// What the color environment variables ask for.
    pub(crate) request: Request,
}

impl Env {
    /// Read the environment, taking whether the stream is a terminal as given.
    fn detect(is_terminal: bool) -> Self {
        Self {
            is_terminal,
            dumb_term: std::env::var_os("TERM").is_some_and(|term| term == "dumb"),
            request: Request::detect(),
        }
    }
}

/// Whether to style, given the flag and the environment.
///
/// Precedence, most specific first: the flag, then the environment's
/// [`Request`], then whether the stream is a terminal that can render escapes.
/// An explicit `--color` outranks the environment because it is this invocation
/// speaking. `CLICOLOR_FORCE` outranks a dumb terminal for the same reason —
/// someone set it on purpose.
pub(crate) const fn decide(choice: ColorChoice, env: Env) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => match env.request {
            Request::Force => true,
            Request::Suppress => false,
            Request::Unset => env.is_terminal && !env.dumb_term,
        },
    }
}

/// Parse a `--color` value for the hand-written grammars, which have no clap to
/// ask.
///
/// # Errors
///
/// [`CliError::Usage`] naming the accepted values.
pub(crate) fn parse_choice(value: &str) -> Result<ColorChoice, CliError> {
    ColorChoice::from_str(value, true).map_err(|_ignored| {
        CliError::Usage(format!(
            "`{value}` is not a color mode; expected auto, always, or never"
        ))
    })
}

/// Find the `--color` value in raw argv, before anything is parsed.
///
/// A usage error has to be styled the way the failing command line asked for,
/// and that command line is exactly the one that could not be parsed — so the
/// choice must be known before parsing is allowed to fail. Junk here is ignored
/// rather than reported: the real parse reaches the same flag a moment later and
/// reports it properly, with the value quoted.
pub(crate) fn prescan(args: &[std::ffi::OsString]) -> ColorChoice {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        // `--` ends the flags; nothing after it is one.
        if arg == "--" {
            break;
        }
        let Some(value) = (match arg.to_str() {
            Some("-color" | "--color") => rest.next().and_then(|next| next.to_str()),
            Some(inline) => inline.strip_prefix("--color="),
            None => None,
        }) else {
            continue;
        };
        if let Ok(choice) = parse_choice(value) {
            return choice;
        }
    }
    ColorChoice::Auto
}

/// Resolve `choice` against stderr and hand the answer to `anstream`.
///
/// Called once, before any output. stderr is the reference stream because that
/// is where every human-readable line goes; stdout may be carrying image bytes,
/// and `anstream` still decides that stream separately on its own.
pub(crate) fn install(choice: ColorChoice) {
    let styled = decide(choice, Env::detect(std::io::stderr().is_terminal()));
    let global = if styled {
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    };
    global.write_global();
}

/// The style for an `error:` label.
pub(crate) const fn error() -> Style {
    Style::new()
        .bold()
        .fg_color(Some(Color::Ansi(AnsiColor::Red)))
}

/// The style for a `warning:` label.
pub(crate) const fn warning() -> Style {
    Style::new()
        .bold()
        .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
}

#[cfg(test)]
mod tests {
    use super::{ColorChoice, Env, Request, decide};

    const REQUESTS: [Request; 3] = [Request::Force, Request::Suppress, Request::Unset];

    const fn env(is_terminal: bool, dumb_term: bool, request: Request) -> Env {
        Env {
            is_terminal,
            dumb_term,
            request,
        }
    }

    /// Exhaustive over every environment, not a sample: the interesting bugs
    /// here are precedence bugs in the corners where two signals disagree.
    fn every_env() -> impl Iterator<Item = Env> {
        REQUESTS.into_iter().flat_map(|request| {
            [false, true].into_iter().flat_map(move |is_terminal| {
                [false, true]
                    .into_iter()
                    .map(move |dumb| env(is_terminal, dumb, request))
            })
        })
    }

    #[test]
    fn an_explicit_flag_beats_every_environment() {
        for e in every_env() {
            assert!(decide(ColorChoice::Always, e), "--color always: {e:?}");
            assert!(!decide(ColorChoice::Never, e), "--color never: {e:?}");
        }
    }

    #[test]
    fn auto_follows_the_terminal_when_nothing_is_asked() {
        assert!(decide(ColorChoice::Auto, env(true, false, Request::Unset)));
        assert!(!decide(
            ColorChoice::Auto,
            env(false, false, Request::Unset)
        ));
    }

    #[test]
    fn a_dumb_terminal_opts_out_but_force_overrides_it() {
        assert!(!decide(ColorChoice::Auto, env(true, true, Request::Unset)));
        assert!(decide(ColorChoice::Auto, env(true, true, Request::Force)));
    }

    #[test]
    fn suppress_wins_over_any_terminal() {
        for e in every_env().filter(|e| e.request == Request::Suppress) {
            assert!(!decide(ColorChoice::Auto, e), "{e:?}");
        }
    }

    #[test]
    fn force_colors_even_a_pipe() {
        for e in every_env().filter(|e| e.request == Request::Force) {
            assert!(decide(ColorChoice::Auto, e), "{e:?}");
        }
    }
}
