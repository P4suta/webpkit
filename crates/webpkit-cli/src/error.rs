//! The CLI error type and its process exit codes.
//!
//! [`CliError`] wraps the codec's [`webpkit::Error`] plus the I/O and
//! usage failures that only exist at the command-line boundary, and maps each to
//! a meaningful, stable process exit code (see [`CliError::exit_code`]).

use std::{fmt, io, process::ExitCode};

use webpkit::Error as CodecError;

/// A command-line failure, carrying enough context for a helpful message and a
/// meaningful process exit code.
#[derive(Debug)]
pub(crate) enum CliError {
    /// The arguments were used incorrectly (exit code `2`, matching clap).
    Usage(String),
    /// The input could not be read (exit code `3`).
    ReadInput {
        /// The source label (a path, or `<stdin>`).
        label: String,
        /// The underlying I/O error kind.
        kind: io::ErrorKind,
    },
    /// The output could not be written (exit code `4`).
    WriteOutput {
        /// The sink label (a path, or `<stdout>`).
        label: String,
        /// The underlying I/O error kind.
        kind: io::ErrorKind,
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
    pub(crate) fn read_input(label: String, err: &io::Error) -> Self {
        Self::ReadInput {
            label,
            kind: err.kind(),
        }
    }

    /// Build a [`CliError::WriteOutput`] from a labeled I/O error.
    #[must_use]
    pub(crate) fn write_output(label: String, err: &io::Error) -> Self {
        Self::WriteOutput {
            label,
            kind: err.kind(),
        }
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
            Self::Usage(_) => 2,
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
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(msg) | Self::Format(msg) | Self::RawConfig(msg) => f.write_str(msg),
            Self::ReadInput { label, kind } => write!(f, "reading {label}: {kind}"),
            Self::WriteOutput { label, kind } => write!(f, "writing {label}: {kind}"),
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
