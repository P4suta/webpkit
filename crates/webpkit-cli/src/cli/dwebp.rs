//! The `dwebp` drop-in: libwebp's `dwebp` command grammar, VP8L-lossless only.
//!
//! Decodes a WebP to PNG (default), PPM, or PAM. YUV/PGM output is rejected
//! (there is no lossy RGB→YUV step here); `-flip` and `-alpha` are honored.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use webpkit::{Image, PixelLayout};

use crate::{
    codec,
    error::CliError,
    format::{self, OutputFormat},
    io::{Sink, Source},
    report::Reporter,
};

/// Parse `dwebp`-style arguments, decode, and return a process exit code.
#[must_use]
pub(crate) fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            crate::report::error(&err);
            err.exit_code()
        },
    }
}

/// A parsed `dwebp` invocation.
#[derive(Default)]
struct Config {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    format: Option<OutputFormat>,
    flip: bool,
    alpha: bool,
    verbose: u8,
    quiet: bool,
}

fn run(args: &[OsString]) -> Result<(), CliError> {
    let config = match parse(args)? {
        Parsed::Run(config) => config,
        Parsed::Handled => return Ok(()),
    };
    let reporter = Reporter::new(config.verbose, config.quiet);
    let input = config
        .input
        .ok_or_else(|| CliError::Usage("no input file (use `-` for stdin)".to_owned()))?;
    let output = config
        .output
        .ok_or_else(|| CliError::Usage("no output file (use `-o <file>`, or `-o -`)".to_owned()))?;

    let source = Source::from_arg(&input);
    let sink = Sink::from_arg(&output);
    let bytes = source.read()?;
    let mut image = codec::decode(&bytes, PixelLayout::Rgba8)?;
    if config.alpha {
        image = alpha_as_gray(&image);
    }
    if config.flip {
        image = flip_vertically(&image);
    }
    let format = OutputFormat::resolve(config.format, sink.extension().as_deref());
    let out = format::write_image(&image, format, image.metadata())?;
    sink.write(&out)?;
    reporter.status(&format!(
        "{} -> {} ({}x{}, {} bytes)",
        source.label(),
        sink.label(),
        image.width(),
        image.height(),
        out.len(),
    ));
    Ok(())
}

enum Parsed {
    Run(Box<Config>),
    Handled,
}

fn parse(args: &[OsString]) -> Result<Parsed, CliError> {
    let mut config = Config::default();
    let mut index = 0;
    while index < args.len() {
        let token = args[index].to_string_lossy().into_owned();
        match token.as_str() {
            "-h" | "-help" | "--help" => {
                print_help();
                return Ok(Parsed::Handled);
            },
            "-version" | "--version" => {
                print_version();
                return Ok(Parsed::Handled);
            },
            "-o" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::Usage("`-o` needs a value".to_owned()))?;
                config.output = Some(PathBuf::from(value));
            },
            "-png" => config.format = Some(OutputFormat::Png),
            "-ppm" => config.format = Some(OutputFormat::Ppm),
            "-pam" => config.format = Some(OutputFormat::Pam),
            "-flip" => config.flip = true,
            "-alpha" => config.alpha = true,
            "-quiet" => config.quiet = true,
            "-v" => config.verbose = config.verbose.saturating_add(1),
            // Accepted for compatibility; no-ops for a lossless RGBA decoder.
            "-nofancy" | "-nofilter" | "-nodither" | "-alpha_dither" | "-mt" | "-incremental"
            | "-noasm" => {},
            "-dither" => {
                index += 1; // consume and ignore the strength value
            },
            "-yuv" | "-pgm" => {
                return Err(CliError::Usage(format!(
                    "`{token}` needs a lossy RGB→YUV conversion; use -png/-ppm/-pam"
                )));
            },
            "-bmp" | "-tiff" => {
                return Err(CliError::Usage(format!(
                    "`{token}` output is not supported; use -png, -ppm, or -pam"
                )));
            },
            "-crop" | "-resize" | "-scale" => {
                return Err(CliError::Usage(format!("`{token}` is not supported yet")));
            },
            "--" => {
                index += 1;
                if index < args.len() {
                    config.input = Some(PathBuf::from(&args[index]));
                }
            },
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(CliError::Usage(format!("unknown option `{other}`")));
            },
            _ => config.input = Some(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    Ok(Parsed::Run(Box::new(config)))
}

/// Replace each pixel's RGB with its alpha value (opaque), visualizing the
/// alpha plane as grayscale — mirroring `dwebp -alpha`.
fn alpha_as_gray(image: &Image) -> Image {
    let mut pixels = image.as_bytes().to_vec();
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3];
        px[0] = a;
        px[1] = a;
        px[2] = a;
        px[3] = 0xff;
    }
    Image::from_parts(
        image.dimensions(),
        PixelLayout::Rgba8,
        pixels,
        false,
        image.metadata().clone(),
    )
}

/// Flip an RGBA8 image vertically (top-to-bottom row reversal).
fn flip_vertically(image: &Image) -> Image {
    let width = image.width() as usize;
    let row = width * 4;
    let mut pixels = Vec::with_capacity(image.as_bytes().len());
    for chunk in image.as_bytes().chunks_exact(row).rev() {
        pixels.extend_from_slice(chunk);
    }
    Image::from_parts(
        image.dimensions(),
        PixelLayout::Rgba8,
        pixels,
        image.has_alpha(),
        image.metadata().clone(),
    )
}

#[allow(
    clippy::print_stdout,
    reason = "help/version print to stdout by CLI convention"
)]
fn print_help() {
    println!(
        "dwebp (webpkit) — decode WebP VP8L (lossless) to PNG/PPM/PAM\n\n\
         Usage: dwebp [options] <input.webp> -o <output>\n\n\
         Options:\n\
         \x20 -o <file>   output file (`-` for stdout)\n\
         \x20 -png        PNG output (default)\n\
         \x20 -ppm / -pam netpbm output\n\
         \x20 -flip       flip vertically\n\
         \x20 -alpha      output the alpha plane as grayscale\n\
         \x20 -quiet / -v quieter / more verbose\n\
         \x20 -version    print version\n"
    );
}

#[allow(
    clippy::print_stdout,
    reason = "help/version print to stdout by CLI convention"
)]
fn print_version() {
    println!("dwebp (webpkit) {}", env!("CARGO_PKG_VERSION"));
}
