//! Byte sources and sinks with the `-` stdin/stdout convention.
//!
//! A path argument of `-` selects the standard stream; anything else is a file.
//! Human-readable status is never written here — that is [`crate::report`]'s job —
//! so a `-o -` pipe stays byte-clean.

use std::{
    collections::BTreeSet,
    fs,
    io::{self, IsTerminal as _, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use crate::error::CliError;

/// Temp files currently mid-write, so a Ctrl-C can delete them.
///
/// `panic = "abort"` and `process::exit` both skip `Drop`, so a
/// [`tempfile::NamedTempFile`]'s own cleanup does not run on a signal. On the
/// normal success/error paths that `Drop` is enough and this set is emptied
/// before the file is dropped; the registry exists solely for the signal path.
fn temp_registry() -> &'static Mutex<BTreeSet<PathBuf>> {
    static REGISTRY: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn register_temp(path: &Path) {
    if let Ok(mut set) = temp_registry().lock() {
        set.insert(path.to_path_buf());
    }
}

fn unregister_temp(path: &Path) {
    if let Ok(mut set) = temp_registry().lock() {
        set.remove(path);
    }
}

/// Delete every temp file still registered as mid-write.
///
/// Called from the interrupt handler, and directly from tests. Best-effort: a
/// file that is already gone or unremovable is left, since the process is exiting
/// regardless.
fn clean_temps() {
    if let Ok(mut set) = temp_registry().lock() {
        for path in set.iter() {
            let _ = fs::remove_file(path);
        }
        set.clear();
    }
}

/// Install a Ctrl-C handler that deletes in-flight temp files, then exits `130`.
///
/// Idempotent by way of `ctrlc`'s single-handler contract — a second call just
/// fails and is ignored. Without this, an interrupt during a write would leave
/// the sibling temp file behind, since no `Drop` runs on a signal.
pub(crate) fn install_interrupt_cleanup() {
    let _ = ctrlc::set_handler(|| {
        clean_temps();
        std::process::exit(130);
    });
}

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
    /// A file is written atomically: bytes go to a sibling temp file which is then
    /// renamed over the target, so a failed or interrupted write never leaves a
    /// half-written `.webp` that looks valid. A plain in-place write truncates the
    /// target the instant it opens, so an interrupted one is worse than no write.
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
                atomic_write(path, bytes).map_err(|err| CliError::write_output(self.label(), err))
            },
        }
    }
}

/// Write `bytes` to a sibling temp file, then rename it over `path`.
///
/// The temp lives in the target's own directory so the final step is a rename
/// within one filesystem, which is atomic; a temp in the system tmp dir could be
/// on another device, where `rename` fails. The temp is registered before the
/// write and unregistered after, so a Ctrl-C in between deletes it.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

    let mut temp = tempfile::Builder::new()
        .prefix(".webp-tmp-")
        .tempfile_in(&dir)?;
    let temp_path = temp.path().to_path_buf();
    register_temp(&temp_path);

    // On any early return here, `temp`'s `Drop` deletes the file — the registry is
    // only for the signal path where `Drop` never runs.
    let result = temp
        .write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .and_then(|()| temp.persist(path).map(|_file| ()).map_err(|err| err.error));

    unregister_temp(&temp_path);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::{
        Sink, atomic_write, clean_temps, expand_globs, looks_like_glob, register_temp,
        temp_registry,
    };

    /// Serialize the tests that touch the process-global temp registry.
    ///
    /// `clean_temps` drains the whole registry, so two write tests running at once
    /// could have one delete the other's in-flight temp. They share real process
    /// state, so they must not run concurrently.
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn an_atomic_write_leaves_no_temp_and_the_right_bytes() {
        let _guard = serial();
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("out.webp");
        Sink::File(target.clone())
            .write(b"payload")
            .expect("atomic write");

        assert_eq!(std::fs::read(&target).expect("read target"), b"payload");
        // No `.webp-tmp-*` sibling survived a successful write.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".webp-tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "a temp file survived: {leftovers:?}");
    }

    #[test]
    fn an_atomic_write_replaces_an_existing_file_wholesale() {
        let _guard = serial();
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("out.webp");
        std::fs::write(&target, b"the old and much longer contents").expect("seed");
        Sink::File(target.clone()).write(b"new").expect("overwrite");
        assert_eq!(std::fs::read(&target).expect("read"), b"new");
    }

    #[test]
    fn the_interrupt_cleanup_deletes_a_registered_temp() {
        let _guard = serial();
        let dir = tempfile::tempdir().expect("temp dir");
        // Simulate a temp file caught mid-write when a signal arrives: it is on
        // disk and registered, and the handler's cleanup must remove it.
        let temp = dir.path().join(".webp-tmp-simulated");
        std::fs::write(&temp, b"half a webp").expect("write temp");
        register_temp(&temp);
        assert!(temp_registry().lock().expect("lock").contains(&temp));

        clean_temps();

        assert!(
            !temp.exists(),
            "the interrupted temp file was not cleaned up"
        );
        assert!(temp_registry().lock().expect("lock").is_empty());
    }

    #[test]
    fn atomic_write_writes_through_a_real_parent_directory() {
        let _guard = serial();
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("bare.webp");
        atomic_write(&target, b"ok").expect("write");
        assert_eq!(std::fs::read(&target).expect("read"), b"ok");
    }

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
