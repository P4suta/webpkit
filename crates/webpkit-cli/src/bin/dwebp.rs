//! The `dwebp` drop-in command-line tool (VP8L lossless).

use std::process::ExitCode;

fn main() -> ExitCode {
    webpkit_cli::cli::dwebp::main()
}
