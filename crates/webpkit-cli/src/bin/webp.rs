//! The `webp` brand command-line tool.

use std::process::ExitCode;

fn main() -> ExitCode {
    webpkit_cli::cli::brand::main()
}
