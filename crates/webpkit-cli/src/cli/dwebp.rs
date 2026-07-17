//! The `dwebp` drop-in: libwebp's `dwebp` command grammar.
//!
//! Decodes a WebP to PNG (default), PPM, or PAM, or — for a lossy `VP8 ` still —
//! to raw YUV 4:2:0 planes (`-yuv`) or their PGM arrangement (`-pgm`); `-flip` and
//! `-alpha` are honored on the RGBA path.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use webpkit::{Image, PixelLayout};

use crate::{
    codec,
    diag::{self, ArgvSpan, Diagnostic},
    error::CliError,
    format::{self, OutputFormat},
    io::{Sink, Source},
    report::Reporter,
    term,
};

/// Every flag `dwebp` recognizes, accepted or rejected — the search space for a
/// did-you-mean suggestion.
const KNOWN_FLAGS: &[&str] = &[
    "-o",
    "-png",
    "-ppm",
    "-pam",
    "-flip",
    "-alpha",
    "-quiet",
    "-v",
    "-color",
    "-nofancy",
    "-nofilter",
    "-nodither",
    "-alpha_dither",
    "-mt",
    "-incremental",
    "-noasm",
    "-dither",
    "-version",
    "-yuv",
    "-pgm",
    "-bmp",
    "-tiff",
    "-crop",
    "-resize",
    "-scale",
];

/// Parse `dwebp`-style arguments, decode, and return a process exit code.
#[must_use]
pub(crate) fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    term::install(term::prescan(&args));
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            crate::report::error(&err.to_diagnostic());
            err.exit_code()
        },
    }
}

/// The native YUV-plane output `dwebp` writes for `-yuv`/`-pgm`.
#[derive(Clone, Copy)]
enum YuvOut {
    /// `-yuv`: raw planar `Y`, then `U`, then `V`.
    Planar,
    /// `-pgm`: the planes stacked into a grayscale PGM (IMC4 layout).
    Pgm,
}

impl YuvOut {
    /// The flag that selected this output, for a diagnostic that names it.
    const fn flag(self) -> &'static str {
        match self {
            Self::Planar => "-yuv",
            Self::Pgm => "-pgm",
        }
    }
}

/// A parsed `dwebp` invocation.
#[derive(Default)]
struct Config {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    format: Option<OutputFormat>,
    yuv: Option<YuvOut>,
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
    // The YUV planes are the lossy decoder's native reconstruction, so this path
    // is distinct from the RGBA one (there is no `Image` to flip or recolor here).
    if let Some(layout) = config.yuv {
        if config.flip || config.alpha {
            return Err(CliError::Usage(format!(
                "`{}` writes the decoder's native YUV planes; `-flip`/`-alpha` are \
                 RGBA transforms that do not apply — drop them, or decode to an RGBA \
                 format (`-png`/`-ppm`/`-pam`) to use them",
                layout.flag(),
            )));
        }
        let yuv = decode_yuv(&bytes, layout)?;
        let out = match layout {
            YuvOut::Planar => format::yuv::write_yuv(&yuv),
            YuvOut::Pgm => format::yuv::write_pgm(&yuv),
        };
        sink.write(&out)?;
        reporter.status(&format!(
            "{} -> {} ({}x{} YUV 4:2:0, {} bytes)",
            source.label(),
            sink.label(),
            yuv.width(),
            yuv.height(),
            out.len(),
        ));
        return Ok(());
    }
    let mut image = codec::decode(
        &bytes,
        PixelLayout::Rgba8,
        Some(webpkit::DEFAULT_MAX_PIXELS),
    )?;
    if config.alpha {
        image = alpha_as_gray(&image)?;
    }
    if config.flip {
        image = flip_vertically(&image)?;
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
    // The command line as strings, for the caret a rejection or a typo draws.
    let rendered: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
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
            "-o" => config.output = Some(PathBuf::from(value(args, &mut index, "-o")?)),
            "-png" => config.format = Some(OutputFormat::Png),
            "-ppm" => config.format = Some(OutputFormat::Ppm),
            "-pam" => config.format = Some(OutputFormat::Pam),
            "-yuv" => config.yuv = Some(YuvOut::Planar),
            "-pgm" => config.yuv = Some(YuvOut::Pgm),
            "-flip" => config.flip = true,
            "-alpha" => config.alpha = true,
            "-quiet" => config.quiet = true,
            // Applied from a prescan in `main`, before parsing can fail; parsed again
            // here to consume the value and to reject a bad one by name.
            "-color" | "--color" => {
                let when = value(args, &mut index, &token)?
                    .to_string_lossy()
                    .into_owned();
                term::parse_choice(&when)?;
            },
            "-v" => config.verbose = config.verbose.saturating_add(1),
            // Accepted for compatibility; no-ops for a lossless RGBA decoder.
            "-nofancy" | "-nofilter" | "-nodither" | "-alpha_dither" | "-mt" | "-incremental"
            | "-noasm" => {},
            #[cfg(feature = "formats")]
            "-bmp" => config.format = Some(OutputFormat::Bmp),
            #[cfg(feature = "formats")]
            "-tiff" => config.format = Some(OutputFormat::Tiff),
            "-crop" | "-resize" | "-scale" | "-dither" => {
                return Err(reject(&rendered, index, &token));
            },
            // Without the `formats` feature the `image`-crate encoders are absent, so
            // BMP/TIFF stay rejected by name rather than accepted and then failing.
            #[cfg(not(feature = "formats"))]
            "-bmp" | "-tiff" => {
                return Err(reject(&rendered, index, &token));
            },
            "--" => {
                index += 1;
                if index < args.len() {
                    config.input = Some(PathBuf::from(&args[index]));
                }
            },
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(CliError::Rejected(Box::new(diag::unknown_flag(
                    "dwebp",
                    &rendered,
                    index,
                    other,
                    KNOWN_FLAGS,
                ))));
            },
            _ => config.input = Some(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    Ok(Parsed::Run(Box::new(config)))
}

/// The tailored cause and help for one rejected `dwebp` output/preprocessing flag.
struct Rejection {
    cause: &'static str,
    help: &'static [&'static str],
}

fn rejection_of(flag: &str) -> Rejection {
    match flag {
        "-bmp" | "-tiff" => Rejection {
            cause: "BMP and TIFF output needs the `formats` feature, which this build \
                    was compiled without.",
            help: &["choose an always-available format:", "  -png | -ppm | -pam"],
        },
        "-dither" => Rejection {
            cause: "this decoder reconstructs the exact pixels; libwebp's -dither \
                    perturbs its lossy output to hide banding, which has nothing to \
                    act on here.",
            help: &["decode without -dither; the result is already exact."],
        },
        _ => Rejection {
            cause: "cropping, resizing, and scaling are pixel preprocessing this decoder does \
                    not perform.",
            help: &["decode first, then transform the result with an image tool."],
        },
    }
}

/// Build the rejection diagnostic for `flag` at `index`, with a caret and its own
/// cause and help.
fn reject(args: &[String], index: usize, flag: &str) -> CliError {
    let rejection = rejection_of(flag);
    let mut diag = Diagnostic::new(format!("`{flag}` is not supported by this decoder"))
        .with_cause(rejection.cause)
        .with_help(rejection.help.iter().copied());
    if let Some(span) = ArgvSpan::at_token("dwebp", args, index) {
        diag = diag.with_span(span);
    }
    CliError::Rejected(Box::new(diag))
}

/// Decode a lossy WebP to its native YUV 4:2:0 planes, turning the lossless or
/// animated case — which has no YUV form — into a message that names the RGBA
/// formats to reach for instead. Other decode failures pass through unchanged.
fn decode_yuv(bytes: &[u8], layout: YuvOut) -> Result<webpkit::YuvImage, CliError> {
    webpkit::decode_yuv(bytes).map_err(|err| match err {
        webpkit::Error::UnsupportedFeature => CliError::Rejected(Box::new(
            Diagnostic::new(format!(
                "`{}` needs a lossy WebP; this input is lossless or animated",
                layout.flag(),
            ))
            .with_cause(
                "YUV output is the lossy (VP8) decoder's native reconstruction; a lossless \
                 or animated file has no YUV form, and converting its RGBA would be a lossy \
                 RGB→YUV step this decoder does not perform.",
            )
            .with_help(["decode it to RGBA instead:", "  -png | -ppm | -pam"]),
        )),
        other => CliError::Codec(other),
    })
}

/// Replace each pixel's RGB with its alpha value (opaque), visualizing the
/// alpha plane as grayscale — mirroring `dwebp -alpha`.
fn alpha_as_gray(image: &Image) -> Result<Image, CliError> {
    let mut pixels = image.as_bytes().to_vec();
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3];
        px[0] = a;
        px[1] = a;
        px[2] = a;
        px[3] = 0xff;
    }
    Ok(Image::new(image.dimensions(), PixelLayout::Rgba8, pixels)?
        .with_metadata(image.metadata().clone()))
}

/// Flip an RGBA8 image vertically (top-to-bottom row reversal).
fn flip_vertically(image: &Image) -> Result<Image, CliError> {
    let width = image.width() as usize;
    let row = width * 4;
    let mut pixels = Vec::with_capacity(image.as_bytes().len());
    for chunk in image.as_bytes().chunks_exact(row).rev() {
        pixels.extend_from_slice(chunk);
    }
    Ok(Image::new(image.dimensions(), PixelLayout::Rgba8, pixels)?
        .with_metadata(image.metadata().clone()))
}

/// The BMP/TIFF output line, present only when the `formats` feature is compiled.
#[cfg(feature = "formats")]
const BMP_TIFF_HELP: &str = "\x20 -bmp / -tiff   BMP / TIFF output\n";
#[cfg(not(feature = "formats"))]
const BMP_TIFF_HELP: &str = "";

fn print_help() {
    crate::report::out(&format!(
        "dwebp (webpkit) — decode WebP (lossless, lossy, or animated) to PNG/PPM/PAM\n\n\
         Usage: dwebp [options] <input.webp> -o <output>\n\n\
         Options:\n\
         \x20 -o <file>      output file (`-` for stdout)\n\
         \x20 -png           PNG output (default)\n\
         \x20 -ppm / -pam    netpbm output\n\
         {BMP_TIFF_HELP}\
         \x20 -yuv / -pgm    raw YUV 4:2:0 planes / their PGM arrangement (lossy input)\n\
         \x20 -flip          flip vertically\n\
         \x20 -alpha         output the alpha plane as grayscale\n\
         \x20 -quiet / -v    quieter / more verbose\n\
         \x20 -color <when>  auto (default), always, or never\n\
         \x20 -version       print version\n",
    ));
}

fn print_version() {
    crate::report::out(&format!("dwebp (webpkit) {}", env!("CARGO_PKG_VERSION")));
}

/// The value following `flag`, advancing `index` past it.
fn value<'a>(
    args: &'a [OsString],
    index: &mut usize,
    flag: &str,
) -> Result<&'a OsString, CliError> {
    *index += 1;
    args.get(*index)
        .ok_or_else(|| CliError::Usage(format!("`{flag}` needs a value")))
}
