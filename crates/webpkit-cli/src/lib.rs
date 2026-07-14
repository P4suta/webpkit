//! Shared library behind the `webpkit` command-line tools.
//!
//! The `cwebp` / `dwebp` drop-in binaries and the `webp` brand tool are thin
//! `main` shims (`src/bin/*.rs`) over the argument parsing, codec glue, byte
//! I/O, reporting, and error handling collected here.
#![forbid(unsafe_code)]

pub mod bulk;
pub mod cli;
pub mod codec;
pub mod effort;
pub mod error;
pub mod format;
pub mod io;
pub mod metadata;
pub mod report;
