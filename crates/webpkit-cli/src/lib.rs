//! Shared library behind the `webpkit` command-line tools.
//!
//! The `cwebp` / `dwebp` drop-ins and the `webp` brand tool are thin `main`
//! shims (`src/bin/*.rs`) over one implementation, selected by [`Personality`].
//!
//! This crate ships binaries; its library surface is deliberately just those two
//! items. Everything else — argument parsing, codec glue, byte I/O, reporting,
//! error handling — is internal, so it can be reshaped without a breaking
//! release.
#![forbid(unsafe_code)]
// Every module below is private, which puts two deny-level lints in deadlock:
// `unreachable_pub` rejects `pub` on an item that cannot be reached from outside,
// and `redundant_pub_crate` rejects the `pub(crate)` that would satisfy it. No
// spelling satisfies both, so one has to go.
//
// Keeping `pub(crate)` is the safer half. If a module is ever made `pub` again,
// `pub(crate)` items stay crate-private and the API surface has to be widened
// deliberately; bare `pub` items would all become public API in silence.
#![allow(
    clippy::redundant_pub_crate,
    reason = "unresolvable conflict with `unreachable_pub`; see above"
)]

use std::process::ExitCode;

mod bulk;
mod cli;
mod codec;
mod config;
mod diag;
mod diff;
mod doctor;
mod effort;
mod error;
mod format;
mod inspect;
mod io;
mod metadata;
mod personality;
mod preprocess;
mod report;
mod strategy;
mod term;

pub use personality::Personality;

/// Run a personality against this process's arguments, returning its exit code.
///
/// Exit codes are meaningful and stable: `0` success, `1` predicate false
/// (`diff`/`doctor`), `2` usage, `3` input I/O, `4` output I/O, `5`
/// decode/bitstream, `6` unsupported feature, `7` limit exceeded, `8` invalid
/// image, `9` input-format parse.
#[must_use]
pub fn run(personality: Personality) -> ExitCode {
    match personality {
        Personality::Webp => cli::brand::main(),
        Personality::Cwebp => cli::cwebp::main(),
        Personality::Dwebp => cli::dwebp::main(),
    }
}
