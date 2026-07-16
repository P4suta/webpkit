//! Byte sources and sinks with the `-` stdin/stdout convention.
//!
//! A path argument of `-` selects the standard stream; anything else is a file.
//! Human-readable status is never written here — that is [`crate::report`]'s job —
//! so a `-o -` pipe stays byte-clean.

use std::{
    fs,
    io::{self, IsTerminal as _, Read, Write},
    path::{Path, PathBuf},
};

use crate::error::CliError;

/// The lowercased extension of a path, if it has one.
#[must_use]
pub(crate) fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

/// Whether a path already exists (a file or a directory).
///
/// Routed through this module so the overwrite guard's one filesystem question
/// stays with every other `fs` touch.
#[must_use]
pub(crate) fn exists(path: &Path) -> bool {
    path.exists()
}

/// Whether a path is an existing directory.
#[must_use]
pub(crate) fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// The current working directory, or `None` if it cannot be determined.
///
/// The starting point for the `webp.toml` walk-up. A failure here just means no
/// project config is found, not an error.
#[must_use]
pub(crate) fn current_dir() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

/// The user's config directory: `%APPDATA%` on Windows, else `$XDG_CONFIG_HOME`
/// or `$HOME/.config`. `None` when the environment names none of them.
#[must_use]
pub(crate) fn config_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
}

/// Whether standard output is a terminal.
#[must_use]
pub(crate) fn is_stdout_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Whether standard error is a terminal.
#[must_use]
pub(crate) fn is_stderr_terminal() -> bool {
    io::stderr().is_terminal()
}

/// The canonical directory this executable lives in.
///
/// The reference point for `doctor`'s drop-in shadow check: a `cwebp` found on
/// `PATH` is *this* toolkit's only when it sits in the same directory. `None` when
/// the path cannot be determined, which just disables the check.
#[must_use]
pub(crate) fn current_exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.parent().map(Path::to_path_buf)
}

/// The first executable named `name` on `PATH`, canonicalized.
///
/// Used by `doctor` to tell whether the `cwebp` / `dwebp` a user would actually
/// run is this toolkit's or libwebp's (the two share those names). `None` when no
/// such file is on `PATH`.
#[must_use]
pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| exe_candidates(&dir, name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

/// The filenames an executable `name` might have in `dir` on this platform.
fn exe_candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![dir.join(format!("{name}.exe")), dir.join(name)]
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(name)]
    }
}

/// Read a file to a string, or `None` if it is absent or unreadable.
///
/// For optional inputs like `webp.toml`, where "not there" is the common case and
/// not a failure. A file that is present but malformed is the caller's problem to
/// report once it has the bytes.
#[must_use]
pub(crate) fn read_optional_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Whether a path string carries a glob metacharacter (`*`, `?`, or `[`).
fn looks_like_glob(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Expand any glob patterns in `inputs`, leaving literal paths untouched.
///
/// A shell does not expand a glob before launching a native binary, so on Windows
/// `webp *.jpg` arrives as the literal `*.jpg`. An input is expanded **only** when
/// it does not already exist as a literal path: a real file named `[a].png` must
/// stay openable, so an existing path is never treated as a pattern. A pattern
/// that matches nothing is left as-is, so the usual "cannot read" error names it.
///
/// # Errors
///
/// [`CliError::Usage`] if a pattern is malformed, or [`CliError::ReadInput`] if a
/// directory cannot be traversed while matching.
pub(crate) fn expand_globs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, CliError> {
    let mut out = Vec::new();
    for input in inputs {
        let text = input.to_string_lossy();
        if exists(input) || !looks_like_glob(&text) {
            out.push(input.clone());
            continue;
        }
        let paths = glob::glob(&text)
            .map_err(|err| CliError::Usage(format!("bad pattern `{text}`: {err}")))?;
        let mut matched = false;
        for entry in paths {
            let path = entry.map_err(|err| {
                CliError::read_input(err.path().display().to_string(), err.into_error())
            })?;
            matched = true;
            out.push(path);
        }
        if !matched {
            out.push(input.clone());
        }
    }
    Ok(out)
}

/// Expand `inputs` into a flat file list, descending into directories.
///
/// Glob patterns are expanded first (see [`expand_globs`]). A path given
/// explicitly is taken as-is; `keep` filters only the entries discovered by
/// walking a directory, so naming a file directly always works whatever its
/// extension. Subdirectories are visited only when `recursive`.
///
/// # Errors
///
/// [`CliError::ReadInput`] if a directory cannot be listed, or [`CliError::Usage`]
/// for a malformed glob pattern.
pub(crate) fn collect_files(
    inputs: &[PathBuf],
    recursive: bool,
    keep: &dyn Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, CliError> {
    let expanded = expand_globs(inputs)?;
    let mut files = Vec::new();
    for input in &expanded {
        if input.is_dir() {
            walk(input, recursive, keep, &mut files)?;
        } else {
            files.push(input.clone());
        }
    }
    Ok(files)
}

fn walk(
    dir: &Path,
    recursive: bool,
    keep: &dyn Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), CliError> {
    let label = || dir.display().to_string();
    let entries = fs::read_dir(dir).map_err(|err| CliError::read_input(label(), err))?;
    for entry in entries {
        let path = entry
            .map_err(|err| CliError::read_input(label(), err))?
            .path();
        if path.is_dir() {
            if recursive {
                walk(&path, recursive, keep, out)?;
            }
        } else if keep(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Where a command reads its input bytes from.
pub(crate) enum Source {
    /// Standard input.
    Stdin,
    /// A file on disk.
    File(PathBuf),
}

/// Where a command writes its output bytes to.
pub(crate) enum Sink {
    /// Standard output.
    Stdout,
    /// A file on disk.
    File(PathBuf),
}

impl Source {
    /// Interpret a path argument: `-` means standard input.
    #[must_use]
    pub(crate) fn from_arg(path: &Path) -> Self {
        if path.as_os_str() == "-" {
            Self::Stdin
        } else {
            Self::File(path.to_path_buf())
        }
    }

    /// A human-readable label for status and error messages.
    #[must_use]
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Stdin => "<stdin>".to_owned(),
            Self::File(path) => path.display().to_string(),
        }
    }

    /// The lowercased file extension, if any (never for stdin).
    #[must_use]
    pub(crate) fn extension(&self) -> Option<String> {
        match self {
            Self::Stdin => None,
            Self::File(path) => extension_of(path),
        }
    }

    /// Read the whole input into memory.
    ///
    /// # Errors
    ///
    /// [`CliError::ReadInput`] on any I/O failure.
    pub(crate) fn read(&self) -> Result<Vec<u8>, CliError> {
        match self {
            Self::Stdin => {
                let mut buf = Vec::new();
                io::stdin()
                    .lock()
                    .read_to_end(&mut buf)
                    .map_err(|err| CliError::read_input(self.label(), err))?;
                Ok(buf)
            },
            Self::File(path) => {
                fs::read(path).map_err(|err| CliError::read_input(self.label(), err))
            },
        }
    }
}

impl Sink {
    /// Interpret a path argument: `-` means standard output.
    #[must_use]
    pub(crate) fn from_arg(path: &Path) -> Self {
        if path.as_os_str() == "-" {
            Self::Stdout
        } else {
            Self::File(path.to_path_buf())
        }
    }

    /// A human-readable label for status and error messages.
    #[must_use]
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Stdout => "<stdout>".to_owned(),
            Self::File(path) => path.display().to_string(),
        }
    }

    /// The lowercased file extension, if any (never for stdout).
    #[must_use]
    pub(crate) fn extension(&self) -> Option<String> {
        match self {
            Self::Stdout => None,
            Self::File(path) => extension_of(path),
        }
    }

    /// Write all bytes to the sink.
    ///
    /// # Errors
    ///
    /// [`CliError::WriteOutput`] on any I/O failure.
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), CliError> {
        match self {
            Self::Stdout => io::stdout()
                .lock()
                .write_all(bytes)
                .map_err(|err| CliError::write_output(self.label(), err)),
            Self::File(path) => {
                fs::write(path, bytes).map_err(|err| CliError::write_output(self.label(), err))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_globs, looks_like_glob};

    #[test]
    fn detects_glob_metacharacters() {
        assert!(looks_like_glob("*.png"));
        assert!(looks_like_glob("img?.jpg"));
        assert!(looks_like_glob("[abc].png"));
        assert!(!looks_like_glob("photo.png"));
    }

    #[test]
    fn an_existing_literal_is_never_expanded() {
        let dir = tempfile::tempdir().expect("temp dir");
        // A real file whose name contains glob metacharacters must stay openable:
        // it exists, so it is passed through verbatim rather than matched.
        let literal = dir.path().join("[a].png");
        std::fs::write(&literal, b"x").expect("write literal");
        let out = expand_globs(std::slice::from_ref(&literal)).expect("expand");
        assert_eq!(out, vec![literal]);
    }

    #[test]
    fn a_pattern_expands_to_its_matches() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.png"), b"x").expect("write a");
        std::fs::write(dir.path().join("b.png"), b"x").expect("write b");
        std::fs::write(dir.path().join("c.txt"), b"x").expect("write c");
        let mut out = expand_globs(&[dir.path().join("*.png")]).expect("expand");
        out.sort();
        let names: Vec<String> = out
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(names, ["a.png", "b.png"]);
    }

    #[test]
    fn a_pattern_with_no_matches_stays_literal() {
        let dir = tempfile::tempdir().expect("temp dir");
        // No match: the literal is kept so the normal "cannot read" error names it,
        // rather than the run silently converting nothing.
        let pattern = dir.path().join("*.png");
        let out = expand_globs(std::slice::from_ref(&pattern)).expect("expand");
        assert_eq!(out, vec![pattern]);
    }
}
