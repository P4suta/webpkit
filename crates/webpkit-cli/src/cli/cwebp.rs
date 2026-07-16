//! The `cwebp` drop-in: libwebp's `cwebp` command grammar, lossy by default.
//!
//! Accepts the familiar single-dash flags (`-o`, `-q`, `-m`, `-z`, `-lossless`,
//! `-metadata`, `-quiet`, `-v`, ...). Like libwebp's `cwebp`, output is **lossy
//! (`VP8 `) by default**; `-lossless` (or `-z`) switches to lossless (`VP8L`).
//! `-q` is quality in lossy mode and effort in lossless mode. Unsupported lossy /
//! preprocessing knobs are rejected with a clear message rather than silently
//! ignored.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use crate::{
    codec::{self, EncodeMode},
    effort,
    error::CliError,
    format::{self, InputFormat},
    io::{Sink, Source},
    metadata::{MetadataField, Selection},
    report::Reporter,
    term,
};

/// Parse `cwebp`-style arguments, encode, and return a process exit code.
#[must_use]
pub(crate) fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    term::install(term::prescan(&args));
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            crate::report::error(&err);
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
    /// Lossless (`VP8L`) output; set by `-lossless` or `-z`. Default is lossy.
    lossless: bool,
    metadata: Vec<MetadataField>,
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
            EncodeMode::Lossless(effort::resolve(self.method, self.level, self.quality))
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
    reject_lossy_source(&bytes, format, &source.label())?;

    let image = format::read_image(&bytes, format, None)?;
    let metadata = Selection::from_fields(&config.metadata).apply(image.metadata());
    let webp = codec::encode(&image, mode, metadata)?;
    sink.write(&webp)?;
    reporter.status(&format!(
        "{} -> {} ({}x{}, {} bytes)",
        source.label(),
        sink.label(),
        image.width(),
        image.height(),
        webp.len(),
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
            "-metadata" => {
                config
                    .metadata
                    .extend(parse_metadata(&value(args, &mut index, "-metadata")?)?);
            },
            "-quiet" => config.quiet = true,
            // Applied from a prescan in `main`, before parsing can fail; parsed again
            // here to consume the value and to reject a bad one by name.
            "-color" | "--color" => {
                term::parse_choice(&value(args, &mut index, &token)?)?;
            },
            "-v" => config.verbose = config.verbose.saturating_add(1),
            // Accepted for compatibility, but a no-op here. `-exact` preserves the
            // RGB of fully-transparent pixels — already this encoder's behavior, as
            // it never rewrites hidden RGB — so it is accepted rather than rejected.
            "-short" | "-progress" | "-exact" | "-noalpha" | "-low_memory" | "-noasm" | "-mt" => {},
            // Accepted-and-ignored options that consume one value. Alpha is always
            // lossless here, so its tuning knobs have no effect.
            "-alpha_q" | "-alpha_method" | "-alpha_filter" | "-blend_alpha" => {
                let _ = value(args, &mut index, &token)?;
            },
            "--" => {
                index += 1;
                if index < args.len() {
                    config.input = Some(PathBuf::from(&args[index]));
                }
            },
            other if is_rejected(other) => return Err(reject(other)),
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(CliError::Usage(format!("unknown option `{other}`")));
            },
            _ => config.input = Some(PathBuf::from(&args[index])),
        }
        index += 1;
    }
    Ok(Parsed::Run(Box::new(config)))
}

/// Rate-control and preprocessing knobs this encoder does not implement. They are
/// rejected (rather than silently ignored) so a caller is never misled into
/// thinking, e.g., a `-size` target or `-crop` took effect.
fn is_rejected(flag: &str) -> bool {
    matches!(
        flag,
        "-near_lossless"
            | "-size"
            | "-psnr"
            | "-pass"
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
            | "-crop"
            | "-resize"
            | "-preset"
    )
}

fn reject(flag: &str) -> CliError {
    CliError::Usage(format!(
        "`{flag}` is a rate-control or preprocessing option unsupported by this \
         encoder"
    ))
}

/// Reject clearly-lossy source images with an actionable message.
fn reject_lossy_source(bytes: &[u8], format: InputFormat, label: &str) -> Result<(), CliError> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err(CliError::Format(format!(
            "{label} is a JPEG (a lossy source); convert it to PNG first"
        )));
    }
    if format == InputFormat::Raw {
        return Err(CliError::Format(format!(
            "{label}: unsupported input; encode from PNG, PPM, or PAM"
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
         \x20 -metadata <list> all,none,icc,exif,xmp (default: all)\n\
         \x20 -quiet / -v      quieter / more verbose\n\
         \x20 -color <when>    auto (default), always, or never\n\
         \x20 -version         print version\n",
    );
}

fn print_version() {
    crate::report::out(&format!("cwebp (webpkit) {}", env!("CARGO_PKG_VERSION")));
}
