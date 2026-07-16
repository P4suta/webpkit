//! `webp doctor`: a real environment diagnostic, not a stub.
//!
//! The load-bearing check is the drop-in shadow: `cwebp` / `dwebp` share their
//! names with libwebp's tools, so the one earliest on `PATH` — which is what runs
//! when a user types the command — may not be this toolkit's. That is a documented
//! source of "webpkit gave me the wrong answer" reports, and nothing else detects
//! it. The rest report config validity, the terminal, text encoding, the thread
//! pool, the build's features, and that nothing here ever touches the network.

use std::process::ExitCode;

use crate::{config, io, report};

/// A check's outcome. Only [`Severity::Error`] changes the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// The check passed.
    Ok,
    /// The check found something worth flagging, but the tool still works.
    Warn,
    /// The check found a real problem; `doctor` exits non-zero.
    Error,
}

/// One diagnostic: a headline plus indented detail lines.
struct Check {
    severity: Severity,
    title: String,
    detail: Vec<String>,
}

impl Check {
    fn new(severity: Severity, title: impl Into<String>, detail: Vec<String>) -> Self {
        Self {
            severity,
            title: title.into(),
            detail,
        }
    }

    /// The report lines: a `[tag] title` headline, then indented detail.
    fn render(&self) -> Vec<String> {
        let tag = match self.severity {
            Severity::Ok => "ok  ",
            Severity::Warn => "warn",
            Severity::Error => "err ",
        };
        std::iter::once(format!("[{tag}] {}", self.title))
            .chain(self.detail.iter().map(|line| format!("       {line}")))
            .collect()
    }
}

/// Run every check, print the report to stdout, and return the exit code: `1`
/// (predicate false) if any check is an error, else `0`.
///
/// A failing check is reported in-band rather than raised, so there is no error
/// path — the diagnostic *is* the output.
pub(crate) fn run() -> ExitCode {
    let our_dir = io::current_exe_dir();
    let checks = [
        drop_in("cwebp", our_dir.as_deref()),
        drop_in("dwebp", our_dir.as_deref()),
        config_check(),
        terminal_check(),
        encoding_check(),
        threads_check(),
        features_check(),
        network_check(),
    ];
    let mut had_error = false;
    for check in &checks {
        had_error |= check.severity == Severity::Error;
        for line in check.render() {
            report::out(&line);
        }
    }
    if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Whether the `name` drop-in a user would run is this toolkit's or another's.
fn drop_in(name: &str, our_dir: Option<&std::path::Path>) -> Check {
    let Some(found) = io::find_on_path(name) else {
        return Check::new(
            Severity::Ok,
            format!("`{name}` drop-in"),
            vec![format!("not on PATH; run it as `webp` instead")],
        );
    };
    let same_dir = our_dir.is_some_and(|dir| found.parent() == Some(dir));
    if same_dir {
        return Check::new(
            Severity::Ok,
            format!("`{name}` resolves to this toolkit"),
            vec![found.display().to_string()],
        );
    }
    let fix = our_dir.map_or_else(
        || "fix: run `webp encode` / `webp decode` to be sure which codec runs".to_owned(),
        |dir| {
            format!(
                "fix: put {} earlier on PATH, or use `webp` directly",
                dir.display()
            )
        },
    );
    Check::new(
        Severity::Warn,
        format!("`{name}` on PATH is not this toolkit"),
        vec![
            format!("first on PATH: {}", found.display()),
            "this is most likely libwebp's tool of the same name — it, not this one, \
             runs when you type the command"
                .to_owned(),
            fix,
        ],
    )
}

/// Whether configuration resolves: a malformed `webp.toml` is a real error.
fn config_check() -> Check {
    match config::resolve(config::Partial::default()) {
        Ok(resolution) if resolution.files.is_empty() => Check::new(
            Severity::Ok,
            "configuration",
            vec!["no webp.toml found; using built-in defaults".to_owned()],
        ),
        Ok(resolution) => Check::new(
            Severity::Ok,
            "configuration",
            resolution
                .files
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        ),
        Err(err) => Check::new(
            Severity::Error,
            "configuration is invalid",
            vec![err.to_string()],
        ),
    }
}

/// The terminal facts that decide color and progress rendering.
fn terminal_check() -> Check {
    let mut detail = vec![
        format!("stdout is a terminal: {}", yes_no(io::is_stdout_terminal())),
        format!("stderr is a terminal: {}", yes_no(io::is_stderr_terminal())),
        format!("TERM: {}", env_or("TERM", "(unset)")),
    ];
    if std::env::var_os("NO_COLOR").is_some() {
        detail.push("NO_COLOR is set (color suppressed unless --color forces it)".to_owned());
    }
    if std::env::var_os("CLICOLOR_FORCE").is_some() {
        detail.push("CLICOLOR_FORCE is set (color even when piped)".to_owned());
    }
    Check::new(Severity::Ok, "terminal", detail)
}

/// Best-effort text-encoding report: Unicode output is only safe on a UTF-8 stream.
fn encoding_check() -> Check {
    #[cfg(windows)]
    let detail = vec![
        "on Windows, non-ASCII output depends on the console code page".to_owned(),
        "status text stays ASCII-safe; run `chcp 65001` for a UTF-8 console".to_owned(),
    ];
    #[cfg(not(windows))]
    let detail = {
        let locale = std::env::var("LC_ALL")
            .or_else(|_ignored| std::env::var("LC_CTYPE"))
            .or_else(|_ignored| std::env::var("LANG"))
            .unwrap_or_default();
        let utf8 = locale
            .to_ascii_uppercase()
            .replace('-', "")
            .contains("UTF8");
        vec![
            if locale.is_empty() {
                "locale: (unset)".to_owned()
            } else {
                format!("locale: {locale}")
            },
            format!("UTF-8 locale: {}", yes_no(utf8)),
        ]
    };
    Check::new(Severity::Ok, "text encoding", detail)
}

/// The size of the shared rayon pool `--threads` bounds.
fn threads_check() -> Check {
    Check::new(
        Severity::Ok,
        "threads",
        vec![
            format!("rayon worker threads: {}", rayon::current_num_threads()),
            "set with --threads N (0 = one per core)".to_owned(),
        ],
    )
}

/// Which optional input formats this build was compiled with.
fn features_check() -> Check {
    let formats = cfg!(feature = "formats");
    Check::new(
        Severity::Ok,
        "build features",
        vec![format!(
            "formats (JPEG/GIF/TIFF/BMP input): {}",
            if formats { "on" } else { "off" }
        )],
    )
}

/// The active statement that this tool never uses the network.
fn network_check() -> Check {
    Check::new(
        Severity::Ok,
        "network",
        vec![
            "never used by this tool — nothing is fetched, uploaded, or checked \
              for updates"
                .to_owned(),
        ],
    )
}

/// A named environment variable's value, or `fallback` when unset.
fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_ignored| fallback.to_owned())
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
