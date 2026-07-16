//! Which command-line tool a process presents itself as.

/// The command-line tool a process presents itself as.
///
/// The three binaries share one implementation; the personality selects the
/// argument grammar and the help text. `Cwebp` and `Dwebp` speak libwebp's
/// single-dash grammar and keep libwebp's contract — the stdout byte stream,
/// the exit codes, and the overwrite behavior a script may depend on. `Webp` is
/// free of that history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Personality {
    /// The `webp` brand tool.
    Webp,
    /// The libwebp-compatible `cwebp` encoder.
    Cwebp,
    /// The libwebp-compatible `dwebp` decoder.
    Dwebp,
}
