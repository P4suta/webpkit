//! Byte sources and sinks with the `-` stdin/stdout convention.
//!
//! A path argument of `-` selects the standard stream; anything else is a file.
//! Human-readable status is never written here — that is [`crate::report`]'s job —
//! so a `-o -` pipe stays byte-clean.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::error::CliError;

/// The lowercased extension of a path, if it has one.
#[must_use]
pub(crate) fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

/// Expand `inputs` into a flat file list, descending into directories.
///
/// A path given explicitly is taken as-is; `keep` filters only the entries
/// discovered by walking a directory, so naming a file directly always works
/// whatever its extension. Subdirectories are visited only when `recursive`.
///
/// # Errors
///
/// [`CliError::ReadInput`] if a directory cannot be listed.
pub(crate) fn collect_files(
    inputs: &[PathBuf],
    recursive: bool,
    keep: &dyn Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for input in inputs {
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
    let entries = fs::read_dir(dir).map_err(|err| CliError::read_input(label(), &err))?;
    for entry in entries {
        let path = entry
            .map_err(|err| CliError::read_input(label(), &err))?
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
                    .map_err(|err| CliError::read_input(self.label(), &err))?;
                Ok(buf)
            },
            Self::File(path) => {
                fs::read(path).map_err(|err| CliError::read_input(self.label(), &err))
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
                .map_err(|err| CliError::write_output(self.label(), &err)),
            Self::File(path) => {
                fs::write(path, bytes).map_err(|err| CliError::write_output(self.label(), &err))
            },
        }
    }
}
