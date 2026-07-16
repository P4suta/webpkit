//! The CLI error type and its process exit codes.
//!
//! [`CliError`] wraps the codec's [`webpkit::Error`] plus the I/O and
//! usage failures that only exist at the command-line boundary, and maps each to
//! a meaningful, stable process exit code (see [`CliError::exit_code`]).
//!
//! The exit codes are a contract: [`EXIT_CODES`] is the documented table `webp
//! explain` prints, and a test pins it against [`CliError::code`] so the two
//! cannot drift.

use std::{fmt, io, process::ExitCode};

use webpkit::Error as CodecError;

use crate::diag::Diagnostic;

/// A command-line failure, carrying enough context for a helpful message and a
/// meaningful process exit code.
#[derive(Debug)]
pub(crate) enum CliError {
    /// The arguments were used incorrectly (exit code `2`, matching clap).
    Usage(String),
    /// A usage error that already carries its own rich diagnostic — a rejected
    /// flag or a flag typo, with a caret and tailored help (exit code `2`).
    Rejected(Box<Diagnostic>),
    /// The input could not be read (exit code `3`).
    ReadInput {
        /// The source label (a path, or `<stdin>`).
        label: String,
        /// The underlying I/O error, kept whole so its OS message survives.
        source: io::Error,
    },
    /// The output could not be written (exit code `4`).
    WriteOutput {
        /// The sink label (a path, or `<stdout>`).
        label: String,
        /// The underlying I/O error, kept whole so its OS message survives.
        source: io::Error,
    },
    /// The codec rejected the input or output (exit code `5`–`8`, by variant).
    Codec(CodecError),
    /// An input image format (PNG/PPM) could not be parsed (exit code `9`).
    Format(String),
    /// Raw-pixel input was misconfigured, e.g. missing dimensions (exit code `8`).
    RawConfig(String),
}

impl CliError {
    /// Build a [`CliError::ReadInput`] from a labeled I/O error.
    #[must_use]
    pub(crate) const fn read_input(label: String, source: io::Error) -> Self {
        Self::ReadInput { label, source }
    }

    /// Build a [`CliError::WriteOutput`] from a labeled I/O error.
    #[must_use]
    pub(crate) const fn write_output(label: String, source: io::Error) -> Self {
        Self::WriteOutput { label, source }
    }

    /// The process exit code for this failure.
    ///
    /// Codes are meaningful and stable: `2` usage, `3` input I/O, `4` output
    /// I/O, `5` decode/bitstream, `6` unsupported feature, `7` limit exceeded,
    /// `8` invalid image or raw config, `9` input-format parse.
    #[must_use]
    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.code())
    }

    const fn code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Rejected(_) => 2,
            Self::ReadInput { .. } => 3,
            Self::WriteOutput { .. } => 4,
            Self::Format(_) => 9,
            Self::RawConfig(_) => 8,
            Self::Codec(err) => match err {
                CodecError::UnsupportedFeature => 6,
                CodecError::LimitExceeded { .. } => 7,
                CodecError::InvalidDimensions | CodecError::PixelBufferMismatch => 8,
                _ => 5,
            },
        }
    }

    /// Present this failure as a [`Diagnostic`] for [`crate::report`] to render.
    ///
    /// A [`Self::Rejected`] already carries its diagnostic (built where the argv
    /// index was known, so the caret can point). Everything else becomes a plain
    /// headline; the I/O variants render the OS message rather than an
    /// [`io::ErrorKind`] summary, so "permission denied (os error 13)" reaches the
    /// user instead of "permission denied".
    #[must_use]
    pub(crate) fn to_diagnostic(&self) -> Diagnostic {
        match self {
            Self::Rejected(diag) => (**diag).clone(),
            Self::Usage(msg) | Self::Format(msg) | Self::RawConfig(msg) => {
                Diagnostic::new(msg.clone())
            },
            Self::ReadInput { label, source } => {
                Diagnostic::new(format!("cannot read `{label}`: {source}"))
            },
            Self::WriteOutput { label, source } => {
                Diagnostic::new(format!("cannot write `{label}`: {source}"))
            },
            Self::Codec(err) => codec_diagnostic(err),
        }
    }
}

/// A codec failure as a diagnostic. The limit case points at `webp explain 7`,
/// which is the only place that names what the ceiling protects against.
fn codec_diagnostic(err: &CodecError) -> Diagnostic {
    let diag = Diagnostic::new(err.to_string());
    match err {
        CodecError::LimitExceeded { .. } => diag
            .with_cause("a built-in safety limit was hit before a large allocation")
            .with_note("run `webp explain 7` for what this limit protects against"),
        _ => diag,
    }
}

/// The documented exit codes and what each means, printed by `webp explain`.
///
/// Each row is `(code, short name, explanation)`. The set of non-zero codes here
/// mirrors [`CliError::code`] exactly, pinned by [`tests::the_table_matches_the_codes`].
pub(crate) const EXIT_CODES: &[(u8, &str, &str)] = &[
    (
        0,
        "success",
        "The operation completed and any output was written in full.",
    ),
    (
        2,
        "usage",
        "The arguments were used incorrectly: an unknown or rejected flag, a \
         missing value, or a missing input/output. Nothing was read or written.",
    ),
    (
        3,
        "read",
        "The input could not be read — it does not exist, is not readable, or the \
         stream ended early. The error names the path and the OS reason.",
    ),
    (
        4,
        "write",
        "The output could not be written — the directory is missing, the disk is \
         full, or the destination is not writable.",
    ),
    (
        5,
        "codec",
        "The bytes are not a decodable WebP bitstream, or the encoder could not \
         produce one. This is a malformed or unsupported file, not a usage error.",
    ),
    (
        6,
        "unsupported",
        "The file is a well-formed WebP that uses a feature this build does not \
         decode.",
    ),
    (
        7,
        "limit",
        "The image's pixel count exceeds the decode limit, which is enforced \
         before any large allocation so a hostile header cannot exhaust memory.",
    ),
    (
        8,
        "dimensions",
        "The image dimensions are out of range (1..=16384 per side), or a raw \
         input's buffer length does not match its declared width and height.",
    ),
    (
        9,
        "format",
        "An input image (PNG/PPM/PAM) could not be parsed. The bytes are not the \
         format they were taken to be.",
    ),
];

/// Look up an exit code by number or short name, for `webp explain`.
///
/// # Errors
///
/// [`CliError::Usage`] if `query` is neither a documented code nor a known name.
pub(crate) fn explain(query: &str) -> Result<Vec<String>, CliError> {
    let numeric = query.parse::<u8>().ok();
    let entry = EXIT_CODES
        .iter()
        .find(|&&(code, name, _)| numeric == Some(code) || query == name);
    if let Some((code, name, meaning)) = entry {
        Ok(vec![
            format!("exit {code}: {name}"),
            String::new(),
            (*meaning).to_owned(),
        ])
    } else {
        let names = EXIT_CODES
            .iter()
            .map(|(code, name, _)| format!("{code} ({name})"))
            .collect::<Vec<_>>()
            .join(", ");
        Err(CliError::Usage(format!(
            "`{query}` is not a documented exit code; known codes are {names}"
        )))
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(msg) | Self::Format(msg) | Self::RawConfig(msg) => f.write_str(msg),
            Self::Rejected(diag) => f.write_str(diag.title()),
            Self::ReadInput { label, source } => write!(f, "cannot read `{label}`: {source}"),
            Self::WriteOutput { label, source } => write!(f, "cannot write `{label}`: {source}"),
            Self::Codec(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<CodecError> for CliError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use webpkit::Error as CodecError;

    use super::{CliError, EXIT_CODES, explain};

    /// Representative instances covering every arm of [`CliError::code`], so the
    /// set of producible codes is concrete and checkable.
    fn every_code() -> Vec<u8> {
        let io = || std::io::Error::from(std::io::ErrorKind::NotFound);
        [
            CliError::Usage(String::new()),
            CliError::Rejected(Box::new(crate::diag::Diagnostic::new(""))),
            CliError::read_input(String::new(), io()),
            CliError::write_output(String::new(), io()),
            CliError::Format(String::new()),
            CliError::RawConfig(String::new()),
            CliError::Codec(CodecError::UnsupportedFeature),
            CliError::Codec(CodecError::LimitExceeded {
                pixels: 1,
                limit: 0,
            }),
            CliError::Codec(CodecError::InvalidDimensions),
            CliError::Codec(CodecError::PixelBufferMismatch),
            CliError::Codec(CodecError::NotWebp),
        ]
        .iter()
        .map(CliError::code)
        .collect()
    }

    /// The explain table and the code map are two spellings of one contract; this
    /// is what keeps them from drifting. Add a `CliError` code without a row, or a
    /// row without a producer, and this fails.
    #[test]
    fn the_table_matches_the_codes() {
        let documented: BTreeSet<u8> = EXIT_CODES.iter().map(|&(code, ..)| code).collect();
        let mut producible: BTreeSet<u8> = every_code().into_iter().collect();
        producible.insert(0); // success is not a `CliError`, but is a documented code.
        assert_eq!(
            producible, documented,
            "every producible exit code needs a row in EXIT_CODES, and vice versa"
        );
    }

    /// Exactly the nine documented codes, so "9-entry table" cannot silently grow.
    #[test]
    fn the_table_has_nine_entries() {
        assert_eq!(EXIT_CODES.len(), 9);
    }

    #[test]
    fn explain_finds_a_code_by_number_and_by_name() {
        let by_number = explain("7").expect("7 is documented");
        assert!(by_number[0].contains("limit"), "{by_number:?}");
        assert!(by_number.join(" ").contains("memory"), "{by_number:?}");
        let by_name = explain("limit").expect("`limit` is documented");
        assert_eq!(by_name, by_number);
    }

    #[test]
    fn explain_rejects_an_unknown_code() {
        assert!(explain("42").is_err());
        assert!(explain("banana").is_err());
    }
}
