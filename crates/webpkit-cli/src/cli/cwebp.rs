//! The `cwebp` drop-in: libwebp's `cwebp` command grammar, lossy by default.
//!
//! Accepts the familiar single-dash flags (`-o`, `-q`, `-m`, `-z`, `-lossless`,
//! `-metadata`, `-quiet`, `-v`, ...). Like libwebp's `cwebp`, output is **lossy
//! (`VP8 `) by default**; `-lossless` (or `-z`) switches to lossless (`VP8L`).
//! `-q` is quality in lossy mode and effort in lossless mode. Unsupported lossy /
//! preprocessing knobs are rejected with a clear message rather than silently
//! ignored.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use webpkit::Image;

use crate::{
    codec::EncodeMode,
    diag::{self, ArgvSpan, Diagnostic},
    effort,
    error::CliError,
    format::{self, InputFormat},
    io::{Sink, Source},
    metadata::{MetadataField, Selection},
    preprocess::{Crop, Pipeline, Resize},
    report::Reporter,
    strategy::{Strategy, Target},
    term,
};

/// Every flag `cwebp` recognizes, accepted or rejected — the search space for a
/// did-you-mean suggestion, so a typo of a *rejected* flag still points home.
const KNOWN_FLAGS: &[&str] = &[
    "-o",
    "-q",
    "-m",
    "-z",
    "-lossless",
    "-metadata",
    "-quiet",
    "-v",
    "-color",
    "-short",
    "-progress",
    "-exact",
    "-noalpha",
    "-low_memory",
    "-noasm",
    "-mt",
    "-alpha_q",
    "-alpha_method",
    "-alpha_filter",
    "-blend_alpha",
    "-version",
    "-near_lossless",
    "-size",
    "-psnr",
    "-pass",
    "-sns",
    "-f",
    "-sharpness",
    "-segments",
    "-partition_limit",
    "-jpeg_like",
    "-sharp_yuv",
    "-hint",
    "-af",
    "-pre",
    "-map",
    "-crop",
    "-resize",
    "-preset",
];

/// Parse `cwebp`-style arguments, encode, and return a process exit code.
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

/// A parsed `cwebp` invocation.
#[derive(Default)]
struct Config {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    method: Option<i64>,
    level: Option<i64>,
    quality: Option<f64>,
    /// Lossless (`VP8L`) output; set by `-lossless`, `-z`, or `-near_lossless`.
    /// Default is lossy.
    lossless: bool,
    /// `-near_lossless N` (`0..=100`, lower = stronger); implies lossless.
    near_lossless: Option<u8>,
    metadata: Vec<MetadataField>,
    /// `-crop x y w h` pixel preprocessing (applied before `-resize`).
    crop: Option<Crop>,
    /// `-resize w h` pixel preprocessing (a `0` axis keeps aspect).
    resize: Option<Resize>,
    /// `-size N`: target output bytes, searched over lossy quality.
    target_size: Option<u64>,
    /// `-psnr N`: target reconstruction PSNR floor in dB.
    target_psnr: Option<f64>,
    /// `-noalpha`: drop the alpha channel (encode the image opaque).
    noalpha: bool,
    verbose: u8,
    quiet: bool,
}

impl Config {
    /// The codec and knobs this invocation selects.
    ///
    /// Lossless maps `-m`/`-z`/`-q` onto the effort method; lossy takes its
    /// quality from `-q` (default 75) and effort from `-m` (default Balanced).
    fn encode_mode(&self) -> EncodeMode {
        if self.lossless {
            EncodeMode::Lossless {
                effort: effort::resolve(self.method, self.level, self.quality),
                near_lossless: self.near_lossless,
            }
        } else {
            EncodeMode::Lossy {
                quality: effort::lossy_quality(self.quality.unwrap_or(75.0)),
                method: effort::lossy_method(self.method),
            }
        }
    }
}

fn run(args: &[OsString]) -> Result<(), CliError> {
    let config = match parse(args)? {
        Parsed::Run(config) => config,
        Parsed::Handled => return Ok(()),
    };
    let reporter = Reporter::new(config.verbose, config.quiet);
    let mode = config.encode_mode();
    let input = config
        .input
        .ok_or_else(|| CliError::Usage("no input file (use `-` for stdin)".to_owned()))?;
    let output = config
        .output
        .ok_or_else(|| CliError::Usage("no output file (use `-o <file>`, or `-o -`)".to_owned()))?;

    let source = Source::from_arg(&input);
    let sink = Sink::from_arg(&output);
    let bytes = source.read()?;
    let format = InputFormat::resolve(None, source.extension().as_deref(), &bytes);
    reject_raw_source(format, &source.label())?;
    // Re-encoding an already-lossy JPEG as lossy WebP compounds loss; unlike
    // libwebp's cwebp (which rejects nothing), point at the exact-preserving path.
    if format == InputFormat::Jpeg && matches!(mode, EncodeMode::Lossy { .. }) {
        crate::report::warn(
            "re-encoding a lossy JPEG as lossy WebP compounds loss; pass -lossless to keep it exact",
        );
    }

    // Crop-then-resize preprocessing. Project from the header first so an
    // out-of-bounds crop fails before the image is even decoded.
    let pipeline = Pipeline::new(config.crop, config.resize);
    if !pipeline.is_empty()
        && let Some(dims) = format::dimensions_of(&bytes, format)
    {
        pipeline.project(dims)?;
    }
    let image = pipeline.apply(format::read_image(&bytes, format, None)?)?;
    let image = if config.noalpha {
        strip_alpha(&image)?
    } else {
        image
    };
    let metadata = Selection::from_fields(&config.metadata).apply(image.metadata());

    // A `-size`/`-psnr` target makes this a quality search; otherwise a single encode.
    let target = Target::from_flags(config.target_size, config.target_psnr);
    let strategy = Strategy::resolve(mode, false, false, target)?;
    let encoded = strategy.run(&image, &metadata)?;
    sink.write(&encoded.bytes)?;
    if let Some(search) = encoded.search_line() {
        reporter.detail(&format!("search: {search}"));
    }
    reporter.status(&format!(
        "{} -> {} ({}x{}, {} bytes)",
        source.label(),
        sink.label(),
        image.width(),
        image.height(),
        encoded.bytes.len(),
    ));
    Ok(())
}

enum Parsed {
    Run(Box<Config>),
    Handled,
}

#[allow(
    clippy::too_many_lines,
    reason = "a flat cwebp flag table reads more clearly than fragmenting it"
)]
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
            "-h" | "-H" | "-help" | "--help" => {
                print_help();
                return Ok(Parsed::Handled);
            },
            "-version" | "--version" => {
                print_version();
                return Ok(Parsed::Handled);
            },
            "-o" => config.output = Some(PathBuf::from(value_os(args, &mut index, "-o")?)),
            "-q" => config.quality = Some(parse_f64(&value(args, &mut index, "-q")?)?),
            "-m" => config.method = Some(parse_i64(&value(args, &mut index, "-m")?)?),
            // `-z` is a lossless preset, so it also switches to lossless output.
            "-z" => {
                config.level = Some(parse_i64(&value(args, &mut index, "-z")?)?);
                config.lossless = true;
            },
            "-lossless" => config.lossless = true,
            // Near-lossless preprocessing implies lossless output, as in libwebp.
            "-near_lossless" => {
                config.near_lossless =
                    Some(parse_near_lossless(&value(args, &mut index, "-near_lossless")?)?);
                config.lossless = true;
            },
            "-metadata" => {
                config
                    .metadata
                    .extend(parse_metadata(&value(args, &mut index, "-metadata")?)?);
            },
            "-quiet" => config.quiet = true,
            // Pixel preprocessing, applied tool-side before the encoder — the same
            // place libwebp's cwebp does it. `-crop x y w h`, `-resize w h`.
            "-crop" => {
                let x = value_u32(args, &mut index, "-crop")?;
                let y = value_u32(args, &mut index, "-crop")?;
                let width = value_u32(args, &mut index, "-crop")?;
                let height = value_u32(args, &mut index, "-crop")?;
                config.crop = Some(Crop {
                    x,
                    y,
                    width,
                    height,
                });
            },
            "-resize" => {
                let width = value_u32(args, &mut index, "-resize")?;
                let height = value_u32(args, &mut index, "-resize")?;
                config.resize = Some(Resize::new(width, height)?);
            },
            // Rate control by CLI-side search over quality (lossy only).
            "-size" => config.target_size = Some(value_u64(args, &mut index, "-size")?),
            "-psnr" => config.target_psnr = Some(parse_f64(&value(args, &mut index, "-psnr")?)?),
            // Applied from a prescan in `main`, before parsing can fail; parsed again
            // here to consume the value and to reject a bad one by name.
            "-color" | "--color" => {
                term::parse_choice(&value(args, &mut index, &token)?)?;
            },
            "-v" => config.verbose = config.verbose.saturating_add(1),
            // Accepted for compatibility, a no-op here. `-exact` preserves the RGB of
            // fully-transparent pixels — already this encoder's behavior.
            "-short" | "-progress" | "-exact" | "-low_memory" | "-noasm" | "-mt" => {},
            // Drop the alpha channel: make the image opaque before encoding.
            "-noalpha" => config.noalpha = true,
            "--" => {
                index += 1;
                if index < args.len() {
                    config.input = Some(PathBuf::from(&args[index]));
                }
            },
            other if is_rejected(other) => return Err(reject(&rendered, index, other)),
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(CliError::Rejected(Box::new(diag::unknown_flag(
                    "cwebp",
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

/// Internal encoder-tuning knobs this encoder does not implement. They are
/// rejected (rather than silently ignored) so a caller is never misled into
/// thinking an internal tuning parameter took effect. `-crop`/`-resize`/`-size`/
/// `-psnr` are **not** here — they are now live (preprocessing and a quality
/// search) rather than internal tuning.
fn is_rejected(flag: &str) -> bool {
    matches!(
        flag,
        "-pass"
            | "-sns"
            | "-f"
            | "-sharpness"
            | "-segments"
            | "-partition_limit"
            | "-jpeg_like"
            | "-sharp_yuv"
            | "-hint"
            | "-af"
            | "-pre"
            | "-map"
            | "-preset"
            | "-alpha_q"
            | "-alpha_method"
            | "-alpha_filter"
            | "-blend_alpha"
    )
}

/// The tailored cause and help for one rejected flag. Different knobs want
/// different answers, so the reason is per-flag rather than one flat sentence.
struct Rejection {
    cause: &'static str,
    help: &'static [&'static str],
}

fn rejection_of(flag: &str) -> Rejection {
    match flag {
        "-preset" => Rejection {
            cause: "a preset bundles the internal tuning knobs below, none of which webpkit \
                    exposes.",
            help: &[
                "choose effort and quality directly:",
                "  cwebp -m <0-6> -q <0-100> <in> -o <out.webp>",
            ],
        },
        "-alpha_q" | "-alpha_method" | "-alpha_filter" => Rejection {
            cause: "webpkit always stores alpha losslessly; there is no lossy-alpha \
                    compression to tune, so this knob has nothing to change.",
            help: &[
                "alpha is preserved exactly; drop the flag.",
                "to discard alpha entirely, use -noalpha.",
            ],
        },
        "-blend_alpha" => Rejection {
            cause: "compositing the image onto a background color is a preprocessing \
                    step this encoder does not model.",
            help: &[
                "flatten first with an image tool, or drop alpha entirely:",
                "  cwebp -noalpha <in> -o <out.webp>",
            ],
        },
        _ => Rejection {
            cause: "this is an internal encoder-tuning knob webpkit does not expose.",
            help: &[
                "the encoder is tuned through effort and quality only:",
                "  cwebp -m <0-6> -q <0-100> <in> -o <out.webp>",
            ],
        },
    }
}

/// Build the rejection diagnostic for `flag` at `index`, with a caret and its own
/// cause and help.
fn reject(args: &[String], index: usize, flag: &str) -> CliError {
    let rejection = rejection_of(flag);
    let mut diag = Diagnostic::new(format!("`{flag}` is not supported by this encoder"))
        .with_cause(rejection.cause)
        .with_help(rejection.help.iter().copied())
        .with_note("other libwebp rate-control and preprocessing flags are rejected the same way");
    if let Some(span) = ArgvSpan::at_token("cwebp", args, index) {
        diag = diag.with_span(span);
    }
    CliError::Rejected(Box::new(diag))
}

/// Drop the alpha channel: force every pixel opaque so the encoder omits alpha
/// (`argb_has_alpha` then sees no transparency), mirroring `cwebp -noalpha`.
fn strip_alpha(image: &Image) -> Result<Image, CliError> {
    let off = image.layout().alpha_byte_offset();
    let mut pixels = image.as_bytes().to_vec();
    for px in pixels.chunks_exact_mut(4) {
        px[off] = 0xff;
    }
    Ok(Image::new(image.dimensions(), image.layout(), pixels)?
        .with_metadata(image.metadata().clone()))
}

/// Reject raw pixel input, which `cwebp` has no way to give dimensions to.
fn reject_raw_source(format: InputFormat, label: &str) -> Result<(), CliError> {
    if format == InputFormat::Raw {
        return Err(CliError::Format(format!(
            "{label}: unsupported input; encode from PNG/JPEG/GIF/TIFF/BMP/PPM/PAM"
        )));
    }
    Ok(())
}

fn value(args: &[OsString], index: &mut usize, flag: &str) -> Result<String, CliError> {
    Ok(value_os(args, index, flag)?.to_string_lossy().into_owned())
}

fn value_os(args: &[OsString], index: &mut usize, flag: &str) -> Result<OsString, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("`{flag}` needs a value")))
}

fn parse_i64(text: &str) -> Result<i64, CliError> {
    text.parse()
        .map_err(|_| CliError::Usage(format!("expected an integer, got `{text}`")))
}

/// Parse a `-near_lossless` level: an integer in `0..=100` (libwebp's range; lower
/// = stronger quantization, `100` disables the pass).
fn parse_near_lossless(text: &str) -> Result<u8, CliError> {
    u8::try_from(parse_i64(text)?)
        .ok()
        .filter(|&level| level <= 100)
        .ok_or_else(|| CliError::Usage(format!("`-near_lossless` expects 0-100, got `{text}`")))
}

/// The next argument parsed as a `u32` (a crop/resize coordinate).
fn value_u32(args: &[OsString], index: &mut usize, flag: &str) -> Result<u32, CliError> {
    let text = value(args, index, flag)?;
    text.parse().map_err(|_| {
        CliError::Usage(format!(
            "`{flag}` expected a non-negative integer, got `{text}`"
        ))
    })
}

/// The next argument parsed as a `u64` (a byte target).
fn value_u64(args: &[OsString], index: &mut usize, flag: &str) -> Result<u64, CliError> {
    let text = value(args, index, flag)?;
    text.parse()
        .map_err(|_| CliError::Usage(format!("`{flag}` expected a byte count, got `{text}`")))
}

fn parse_f64(text: &str) -> Result<f64, CliError> {
    text.parse()
        .map_err(|_| CliError::Usage(format!("expected a number, got `{text}`")))
}

fn parse_metadata(list: &str) -> Result<Vec<MetadataField>, CliError> {
    list.split(',')
        .map(|item| match item.trim() {
            "all" => Ok(MetadataField::All),
            "none" => Ok(MetadataField::None),
            "icc" => Ok(MetadataField::Icc),
            "exif" => Ok(MetadataField::Exif),
            "xmp" => Ok(MetadataField::Xmp),
            other => Err(CliError::Usage(format!(
                "unknown -metadata value `{other}`"
            ))),
        })
        .collect()
}

fn print_help() {
    crate::report::out(
        "cwebp (webpkit) — encode PNG/PPM/PAM to WebP (lossy by default)\n\n\
         Usage: cwebp [options] <input> -o <output.webp>\n\n\
         Options:\n\
         \x20 -o <file>        output file (`-` for stdout)\n\
         \x20 -q <float>       lossy quality 0-100 (default 75); effort in -lossless mode\n\
         \x20 -m <int>         method 0-6 (effort)\n\
         \x20 -lossless        encode losslessly (VP8L) instead of lossy (VP8)\n\
         \x20 -z <int>         lossless level 0-9 (implies -lossless)\n\
         \x20 -near_lossless <int>  near-lossless preprocessing 0-100, lower = stronger (implies -lossless)\n\
         \x20 -metadata <list> all,none,icc,exif,xmp (default: all)\n\
         \x20 -crop x y w h    crop before encoding (dimensions match libwebp; pixels differ)\n\
         \x20 -resize w h      resize before encoding (0 on one axis keeps aspect)\n\
         \x20 -size <int>      target output size in bytes (searches lossy quality)\n\
         \x20 -psnr <float>    target reconstruction PSNR floor in dB (lossy)\n\
         \x20 -quiet / -v      quieter / more verbose\n\
         \x20 -color <when>    auto (default), always, or never\n\
         \x20 -version         print version\n",
    );
}

fn print_version() {
    crate::report::out(&format!("cwebp (webpkit) {}", env!("CARGO_PKG_VERSION")));
}
