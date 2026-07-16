//! The `dwebp` binary.

use std::process::ExitCode;

use webpkit_cli::Personality;

fn main() -> ExitCode {
    webpkit_cli::run(Personality::Dwebp)
}
