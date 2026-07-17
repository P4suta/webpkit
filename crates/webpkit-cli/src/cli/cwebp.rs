//! The `cwebp` drop-in: libwebp's `cwebp` command grammar, lossy by default.
//!
//! Accepts the familiar single-dash flags (`-o`, `-q`, `-m`, `-z`, `-lossless`,
//! `-metadata`, `-quiet`, `-v`, ...). Like libwebp's `cwebp`, output is **lossy
//! (`VP8 `) by default**; `-lossless` (or `-z`) switches to lossless (`VP8L`).
//! `-q` is quality in lossy mode and effort in lossless mode. Unsupported lossy /
//! preprocessing knobs are rejected with a clear message rather than silently
//! ignored.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use webpkit::{AlphaFilterMode, AlphaMethod, Image, LossyTuning, Preset};

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

/// The `-h` help line for an [`Disposition::Active`] flag: a left-column usage and a
/// right-column blurb, rendered by [`print_help`] from the one flag table.
struct HelpEntry {
    /// The flag and its value placeholder, e.g. `-q <float>`.
    usage: &'static str,
    /// A one-line description.
    blurb: &'static str,
}

/// What the drop-in does with a recognized flag — the single source of truth that
/// drives the parser's classification, the rejection diagnostics, the did-you-mean
/// candidate set, and the generated `-h` help. Adding, moving, or retiring a flag is a
/// one-line [`FLAGS`] edit that can never drift across those four concerns.
enum Disposition {
    /// Recognized and acted on by a parser arm below. `Some(help)` renders its `-h`
    /// line; aliases and the specially-handled `-h`/`--help` carry `None`.
    Active(Option<HelpEntry>),
    /// Accepted for libwebp compatibility and ignored. `takes_value` skips its argument
    /// so the following token is not mistaken for input.
    CompatNoop {
        /// Whether the flag consumes the next argument.
        takes_value: bool,
    },
    /// Not supported; rejected with a tailored `cause` and `help` under a caret.
    Rejected {
        /// The one-line reason shown after the title.
        cause: &'static str,
        /// The `help:` lines pointing at a supported path.
        help: &'static [&'static str],
    },
}

/// One flag and its [`Disposition`].
struct FlagSpec {
    /// The flag spelling, including the leading dash.
    name: &'static str,
    /// What the parser does with it.
    disposition: Disposition,
}

/// A helpful line, factored out so the two generic-tuning rejections share one text.
const TUNE_HELP: &[&str] = &[
    "the encoder is tuned through effort and quality only:",
    "  cwebp -m <0-6> -q <0-100> <in> -o <out.webp>",
];

/// Shorthand for an [`Disposition::Active`] flag with a rendered help line.
const fn active(usage: &'static str, blurb: &'static str) -> Disposition {
    Disposition::Active(Some(HelpEntry { usage, blurb }))
}

/// The single flag-disposition table. Order here is the order of the generated `-h`
/// help; [`known_flags`], the rejection reasons, and the did-you-mean candidates all
/// derive from it.
const FLAGS: &[FlagSpec] = &[
    FlagSpec {
        name: "-o",
        disposition: active("-o <file>", "output file (`-` for stdout)"),
    },
    FlagSpec {
        name: "-q",
        disposition: active(
            "-q <float>",
            "lossy quality 0-100 (default 75); effort in -lossless mode",
        ),
    },
    FlagSpec {
        name: "-m",
        disposition: active("-m <int>", "method 0-6 (effort)"),
    },
    FlagSpec {
        name: "-lossless",
        disposition: active(
            "-lossless",
            "encode losslessly (VP8L) instead of lossy (VP8)",
        ),
    },
    FlagSpec {
        name: "-z",
        disposition: active("-z <int>", "lossless level 0-9 (implies -lossless)"),
    },
    FlagSpec {
        name: "-near_lossless",
        disposition: active(
            "-near_lossless <int>",
            "near-lossless preprocessing 0-100, lower = stronger (implies -lossless)",
        ),
    },
    FlagSpec {
        name: "-metadata",
        disposition: active("-metadata <list>", "all,none,icc,exif,xmp (default: all)"),
    },
    FlagSpec {
        name: "-preset",
        disposition: active(
            "-preset <name>",
            "content preset: default, photo, picture, drawing, icon, text (a tuning base)",
        ),
    },
    FlagSpec {
        name: "-crop",
        disposition: active(
            "-crop x y w h",
            "crop before encoding (dimensions match libwebp; pixels differ)",
        ),
    },
    FlagSpec {
        name: "-resize",
        disposition: active(
            "-resize w h",
            "resize before encoding (0 on one axis keeps aspect)",
        ),
    },
    FlagSpec {
        name: "-size",
        disposition: active(
            "-size <int>",
            "target output size in bytes (searches lossy quality)",
        ),
    },
    FlagSpec {
        name: "-psnr",
        disposition: active(
            "-psnr <float>",
            "target reconstruction PSNR floor in dB (lossy)",
        ),
    },
    FlagSpec {
        name: "-pass",
        disposition: active(
            "-pass <int>",
            "number of entropy-refinement passes 1-10 (1 = single pass)",
        ),
    },
    FlagSpec {
        name: "-sns",
        disposition: active("-sns <int>", "spatial noise shaping 0-100"),
    },
    FlagSpec {
        name: "-f",
        disposition: active("-f <int>", "in-loop filter strength 0-100"),
    },
    FlagSpec {
        name: "-sharpness",
        disposition: active("-sharpness <int>", "in-loop filter sharpness 0-7"),
    },
    FlagSpec {
        name: "-segments",
        disposition: active("-segments <int>", "number of quantizer segments 1-4"),
    },
    FlagSpec {
        name: "-partition_limit",
        disposition: active(
            "-partition_limit <int>",
            "first-partition rate cap 0-100 (0 = no limit)",
        ),
    },
    FlagSpec {
        name: "-jpeg_like",
        disposition: active(
            "-jpeg_like",
            "bias quantization toward a JPEG-like size curve",
        ),
    },
    FlagSpec {
        name: "-sharp_yuv",
        disposition: active("-sharp_yuv", "luminance-guided (sharp) chroma subsampling"),
    },
    FlagSpec {
        name: "-exact",
        disposition: active(
            "-exact",
            "preserve the RGB under fully-transparent pixels (the default)",
        ),
    },
    FlagSpec {
        name: "-alpha_q",
        disposition: active(
            "-alpha_q <int>",
            "alpha quality 0-100 (100 = lossless, the default)",
        ),
    },
    FlagSpec {
        name: "-alpha_method",
        disposition: active(
            "-alpha_method <int>",
            "alpha compression: 0 raw, 1 lossless (default)",
        ),
    },
    FlagSpec {
        name: "-alpha_filter",
        disposition: active(
            "-alpha_filter <str>",
            "alpha filter: none, fast, or best (default)",
        ),
    },
    FlagSpec {
        name: "-noalpha",
        disposition: active("-noalpha", "drop the alpha channel (encode opaque)"),
    },
    FlagSpec {
        name: "-quiet",
        disposition: active("-quiet", "suppress the status line"),
    },
    FlagSpec {
        name: "-short",
        disposition: active("-short", "concise output: print only the result size"),
    },
    FlagSpec {
        name: "-progress",
        disposition: active("-progress", "report encoding progress by stage"),
    },
    FlagSpec {
        name: "-v",
        disposition: active("-v", "verbose output (repeatable)"),
    },
    FlagSpec {
        name: "-color",
        disposition: active("-color <when>", "auto (default), always, or never"),
    },
    FlagSpec {
        name: "-version",
        disposition: active("-version", "print version"),
    },
    // Accepted for libwebp compatibility, ignored here.
    FlagSpec {
        name: "-low_memory",
        disposition: Disposition::CompatNoop { takes_value: false },
    },
    FlagSpec {
        name: "-noasm",
        disposition: Disposition::CompatNoop { takes_value: false },
    },
    FlagSpec {
        name: "-mt",
        disposition: Disposition::CompatNoop { takes_value: false },
    },
    // The true residue of genuinely-unsupported flags.
    FlagSpec {
        name: "-blend_alpha",
        disposition: Disposition::Rejected {
            cause: "compositing the image onto a background color is a preprocessing step this \
                    encoder does not model.",
            help: &[
                "flatten first with an image tool, or drop alpha entirely:",
                "  cwebp -noalpha <in> -o <out.webp>",
            ],
        },
    },
    FlagSpec {
        name: "-hint",
        disposition: Disposition::Rejected {
            cause: GENERIC_TUNE_CAUSE,
            help: TUNE_HELP,
        },
    },
    FlagSpec {
        name: "-af",
        disposition: Disposition::Rejected {
            cause: GENERIC_TUNE_CAUSE,
            help: TUNE_HELP,
        },
    },
    FlagSpec {
        name: "-pre",
        disposition: Disposition::Rejected {
            cause: GENERIC_TUNE_CAUSE,
            help: TUNE_HELP,
        },
    },
    FlagSpec {
        name: "-map",
        disposition: Disposition::Rejected {
            cause: GENERIC_TUNE_CAUSE,
            help: TUNE_HELP,
        },
    },
];

/// The shared cause for the internal-tuning-knob rejections.
const GENERIC_TUNE_CAUSE: &str = "this is an internal encoder-tuning knob webpkit does not expose.";

/// The [`FlagSpec`] for `name`, or `None` when the flag is unknown.
fn spec(name: &str) -> Option<&'static FlagSpec> {
    FLAGS.iter().find(|f| f.name == name)
}

/// Every flag the drop-in recognizes — the did-you-mean search space, so a typo of a
/// *rejected* flag still points home. Derived from the one table.
fn known_flags() -> Vec<&'static str> {
    FLAGS.iter().map(|f| f.name).collect()
}

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
#[allow(
    clippy::struct_excessive_bools,
    reason = "a flat accumulator of independent cwebp boolean flags (lossless, noalpha, \
              sharp_yuv, quiet); a state machine would obscure the one-flag-per-field map"
)]
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
    /// `-sns N`: spatial-noise-shaping strength (`0..=100`).
    sns: Option<u8>,
    /// `-f N`: in-loop deblocking-filter strength (`0..=100`).
    filter_strength: Option<u8>,
    /// `-sharpness N`: in-loop deblocking-filter sharpness (`0..=7`).
    filter_sharpness: Option<u8>,
    /// `-segments N`: number of macroblock quantizer segments (`1..=4`).
    segments: Option<u8>,
    /// `-alpha_q N`: alpha-plane quality (`0..=100`; `100` = lossless).
    alpha_q: Option<u8>,
    /// `-alpha_method N`: alpha compression (`0` raw / `1` lossless).
    alpha_method: Option<AlphaMethod>,
    /// `-alpha_filter <none|fast|best>`: alpha spatial-filter search.
    alpha_filter: Option<AlphaFilterMode>,
    /// `-sharp_yuv`: use luminance-guided (sharp) chroma subsampling instead of the box
    /// filter. Off by default, matching the byte-identical plain path.
    sharp_yuv: bool,
    /// `-preset <name>`: a content preset expanded into a base [`LossyTuning`] that the
    /// explicit knobs above override.
    preset: Option<Preset>,
    /// `-exact`: preserve the RGB under fully-transparent pixels (webpkit's default).
    exact: bool,
    /// `-jpeg_like`: bias quantization toward a JPEG-like size curve.
    jpeg_like: bool,
    /// `-partition_limit N`: first-partition rate cap (`0..=100`; `0` = no limit).
    partition_limit: Option<u8>,
    /// `-pass N`: entropy-refinement pass count (`1..=10`; `1` = single pass).
    pass: Option<u8>,
    /// `-short`: collapse the status line to the essential result (output size).
    short: bool,
    /// `-progress`: report encoding progress by stage.
    progress: bool,
    verbose: u8,
    quiet: bool,
}

impl Config {
    /// The codec and knobs this invocation selects.
    ///
    /// Lossless maps `-m`/`-z`/`-q` onto the effort method; lossy takes its
    /// quality from `-q` (default 75) and effort from `-m` (default auto).
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
                tuning: self.tuning(),
            }
        }
    }

    /// The psychovisual [`LossyTuning`] this invocation selects: a `-preset` bundle (or
    /// the near-best default) as the base, with each explicit `-sns`/`-f`/`-sharpness`/
    /// `-segments`/alpha/RD knob applied on top (every setter validates its range). A
    /// preset is only a base, so an explicit knob always wins over it.
    fn tuning(&self) -> LossyTuning {
        let mut tuning = self
            .preset
            .map_or_else(LossyTuning::default, Preset::tuning);
        if let Some(sns) = self.sns {
            tuning = tuning.with_sns_strength(sns);
        }
        if let Some(f) = self.filter_strength {
            tuning = tuning.with_filter_strength(f);
        }
        if let Some(sharpness) = self.filter_sharpness {
            tuning = tuning.with_filter_sharpness(sharpness);
        }
        if let Some(segments) = self.segments {
            tuning = tuning.with_segments(segments);
        }
        if let Some(alpha_q) = self.alpha_q {
            tuning = tuning.with_alpha_q(alpha_q);
        }
        if let Some(method) = self.alpha_method {
            tuning = tuning.with_alpha_method(method);
        }
        if let Some(filter) = self.alpha_filter {
            tuning = tuning.with_alpha_filter(filter);
        }
        if self.sharp_yuv {
            tuning = tuning.with_sharp_yuv(true);
        }
        if self.exact {
            tuning = tuning.with_exact(true);
        }
        if self.jpeg_like {
            tuning = tuning.with_jpeg_like(true);
        }
        if let Some(limit) = self.partition_limit {
            tuning = tuning.with_partition_limit(limit);
        }
        if let Some(pass) = self.pass {
            tuning = tuning.with_pass(pass);
        }
        tuning
    }
}

fn run(args: &[OsString]) -> Result<(), CliError> {
    let config = match parse(args)? {
        Parsed::Run(config) => config,
        Parsed::Handled => return Ok(()),
    };
    let reporter = Reporter::new(config.verbose, config.quiet)
        .with_short_and_progress(config.short, config.progress);
    let mode = config.encode_mode();
    let input = config
        .input
        .ok_or_else(|| CliError::Usage("no input file (use `-` for stdin)".to_owned()))?;
    let output = config
        .output
        .ok_or_else(|| CliError::Usage("no output file (use `-o <file>`, or `-o -`)".to_owned()))?;

    let source = Source::from_arg(&input);
    let sink = Sink::from_arg(&output);
    reporter.progress("reading");
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
    reporter.progress("encoding");
    let encoded = strategy.run(&image, &metadata)?;
    reporter.progress("writing");
    sink.write(&encoded.bytes)?;
    if let Some(search) = encoded.search_line() {
        reporter.detail(&format!("search: {search}"));
    }
    // `-short`: just the essential result (the output size); otherwise the full line.
    if reporter.is_short() {
        reporter.status(&format!("{} bytes", encoded.bytes.len()));
    } else {
        reporter.status(&format!(
            "{} -> {} ({}x{}, {} bytes)",
            source.label(),
            sink.label(),
            image.width(),
            image.height(),
            encoded.bytes.len(),
        ));
    }
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
                config.near_lossless = Some(parse_near_lossless(&value(
                    args,
                    &mut index,
                    "-near_lossless",
                )?)?);
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
            // Rate control by a codec-native quality search (lossy only).
            "-size" => config.target_size = Some(value_u64(args, &mut index, "-size")?),
            "-psnr" => config.target_psnr = Some(parse_f64(&value(args, &mut index, "-psnr")?)?),
            // `-pass N`: entropy-refinement pass count, mapped onto `LossyTuning::with_pass`
            // (each pass sharpens the size estimate). `1` (the default) is byte-identical.
            "-pass" => config.pass = Some(value_knob(args, &mut index, "-pass")?),
            // Psychovisual tuning knobs, mapped onto the encoder's `LossyTuning`
            // (each setter validates its own range).
            "-sns" => config.sns = Some(value_knob(args, &mut index, "-sns")?),
            "-f" => config.filter_strength = Some(value_knob(args, &mut index, "-f")?),
            "-sharpness" => {
                config.filter_sharpness = Some(value_knob(args, &mut index, "-sharpness")?);
            },
            "-segments" => config.segments = Some(value_knob(args, &mut index, "-segments")?),
            // A content preset: expanded into a base `LossyTuning`, overridden by any
            // explicit knob above. `default` (or no preset) is byte-identical.
            "-preset" => config.preset = Some(parse_preset(&value(args, &mut index, "-preset")?)?),
            // Luminance-guided chroma subsampling (a boolean flag), mapped onto
            // `LossyTuning::with_sharp_yuv`. Off by default, so omitting it is byte-identical.
            "-sharp_yuv" => config.sharp_yuv = true,
            // `-exact` preserves the RGB under fully-transparent pixels — webpkit's
            // default, so this states the guarantee. `-jpeg_like`/`-partition_limit`
            // bias the base quantizer; both neutral by default (byte-identical).
            "-exact" => config.exact = true,
            "-jpeg_like" => config.jpeg_like = true,
            "-partition_limit" => {
                config.partition_limit = Some(value_knob(args, &mut index, "-partition_limit")?);
            },
            // Lossy-alpha knobs: `-alpha_q` drives the level-quantization pre-pass,
            // `-alpha_method`/`-alpha_filter` bound the stored-plane search.
            "-alpha_q" => config.alpha_q = Some(value_knob(args, &mut index, "-alpha_q")?),
            "-alpha_method" => {
                config.alpha_method = Some(parse_alpha_method(&value(
                    args,
                    &mut index,
                    "-alpha_method",
                )?)?);
            },
            "-alpha_filter" => {
                config.alpha_filter = Some(parse_alpha_filter(&value(
                    args,
                    &mut index,
                    "-alpha_filter",
                )?)?);
            },
            // Applied from a prescan in `main`, before parsing can fail; parsed again
            // here to consume the value and to reject a bad one by name.
            "-color" | "--color" => {
                term::parse_choice(&value(args, &mut index, &token)?)?;
            },
            "-v" => config.verbose = config.verbose.saturating_add(1),
            // Concise output / per-stage progress, both via the Reporter (stderr only).
            "-short" => config.short = true,
            "-progress" => config.progress = true,
            // Drop the alpha channel: make the image opaque before encoding.
            "-noalpha" => config.noalpha = true,
            "--" => {
                index += 1;
                if index < args.len() {
                    config.input = Some(PathBuf::from(&args[index]));
                }
            },
            // Everything else is classified by the one flag table: a compat no-op is
            // skipped (consuming its value if any), a rejected flag draws its caret and
            // cause, and an unrecognized dash-flag draws a did-you-mean.
            other => match spec(other).map(|f| &f.disposition) {
                Some(Disposition::CompatNoop { takes_value }) => {
                    if *takes_value {
                        let _ = value_os(args, &mut index, other)?;
                    }
                },
                Some(Disposition::Rejected { cause, help }) => {
                    return Err(reject(&rendered, index, other, cause, help));
                },
                // An Active flag reaches here only if its parser arm above is missing —
                // a drift the `active_flags_are_dispatched` test forbids; fall through to
                // the did-you-mean so a caller still gets a caret, never a silent accept.
                Some(Disposition::Active(_)) | None
                    if other.starts_with('-') && other.chars().count() > 1 =>
                {
                    return Err(CliError::Rejected(Box::new(diag::unknown_flag(
                        "cwebp",
                        &rendered,
                        index,
                        other,
                        &known_flags(),
                    ))));
                },
                _ => config.input = Some(PathBuf::from(&args[index])),
            },
        }
        index += 1;
    }
    Ok(Parsed::Run(Box::new(config)))
}

/// Build the rejection diagnostic for `flag` at `index`, with a caret and the
/// `cause`/`help` the flag table carries for it.
fn reject(args: &[String], index: usize, flag: &str, cause: &str, help: &[&str]) -> CliError {
    let mut diag = Diagnostic::new(format!("`{flag}` is not supported by this encoder"))
        .with_cause(cause)
        .with_help(help.iter().copied())
        .with_note("other libwebp rate-control and preprocessing flags are rejected the same way");
    if let Some(span) = ArgvSpan::at_token("cwebp", args, index) {
        diag = diag.with_span(span);
    }
    CliError::Rejected(Box::new(diag))
}

/// Parse a `-preset` name into a [`Preset`] (libwebp's `WebPPreset` set).
fn parse_preset(text: &str) -> Result<Preset, CliError> {
    match text {
        "default" => Ok(Preset::Default),
        "photo" => Ok(Preset::Photo),
        "picture" => Ok(Preset::Picture),
        "drawing" => Ok(Preset::Drawing),
        "icon" => Ok(Preset::Icon),
        "text" => Ok(Preset::Text),
        _ => Err(CliError::Usage(format!(
            "`-preset` expects default, photo, picture, drawing, icon, or text, got `{text}`"
        ))),
    }
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

/// The next argument parsed as a psychovisual-tuning knob (`-sns`/`-f`/`-sharpness`/
/// `-segments`): a non-negative integer, saturating into `u8` (the [`LossyTuning`]
/// setter re-validates it into the knob's own range).
fn value_knob(args: &[OsString], index: &mut usize, flag: &str) -> Result<u8, CliError> {
    let text = value(args, index, flag)?;
    let n = parse_i64(&text)?;
    u8::try_from(n.clamp(0, i64::from(u8::MAX))).map_err(|_| {
        CliError::Usage(format!(
            "`{flag}` expected a non-negative integer, got `{text}`"
        ))
    })
}

/// Parse a `-alpha_method` value: `0` stores the alpha plane raw, `1` compresses it
/// losslessly (libwebp's two alpha methods).
fn parse_alpha_method(text: &str) -> Result<AlphaMethod, CliError> {
    match text {
        "0" => Ok(AlphaMethod::None),
        "1" => Ok(AlphaMethod::Compressed),
        _ => Err(CliError::Usage(format!(
            "`-alpha_method` expects 0 (raw) or 1 (lossless), got `{text}`"
        ))),
    }
}

/// Parse a `-alpha_filter` value: `none`, `fast`, or `best` (libwebp's alpha filter
/// choices), selecting how many spatial predictors the alpha search trials.
fn parse_alpha_filter(text: &str) -> Result<AlphaFilterMode, CliError> {
    match text {
        "none" => Ok(AlphaFilterMode::None),
        "fast" => Ok(AlphaFilterMode::Fast),
        "best" => Ok(AlphaFilterMode::Best),
        _ => Err(CliError::Usage(format!(
            "`-alpha_filter` expects none, fast, or best, got `{text}`"
        ))),
    }
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

/// The column width the flag usage is padded to before its blurb, sized to the
/// widest usage the table renders.
const HELP_USAGE_WIDTH: usize = 22;

/// Render the `-h` help from the one flag table: the header, then one line per
/// [`Disposition::Active`] flag that carries a [`HelpEntry`], in table order. Nothing
/// to hand-maintain — a table edit reflows the help.
fn print_help() {
    let mut text = String::from(
        "cwebp (webpkit) — encode PNG/PPM/PAM to WebP (lossy by default)\n\n\
         Usage: cwebp [options] <input> -o <output.webp>\n\n\
         Options:\n",
    );
    for flag in FLAGS {
        if let Disposition::Active(Some(entry)) = &flag.disposition {
            text.push_str("  ");
            text.push_str(entry.usage);
            for _ in entry.usage.len()..HELP_USAGE_WIDTH {
                text.push(' ');
            }
            text.push(' ');
            text.push_str(entry.blurb);
            text.push('\n');
        }
    }
    crate::report::out(&text);
}

fn print_version() {
    crate::report::out(&format!("cwebp (webpkit) {}", env!("CARGO_PKG_VERSION")));
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Disposition, FLAGS, parse};
    use crate::error::CliError;

    #[test]
    fn flag_table_has_no_duplicate_names() {
        let mut names: Vec<&str> = FLAGS.iter().map(|f| f.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a flag appears twice in the table");
    }

    #[test]
    fn active_flags_are_dispatched_not_reported_unknown() {
        // Every `Active` flag must have a real parser arm: feeding it alone may fail for
        // a missing value or input (`CliError::Usage`), but it must never be `Rejected`,
        // which is what an Active flag missing from the dispatch match would produce (it
        // would fall through to the did-you-mean). This ties the table to the arms so the
        // two cannot drift.
        for flag in FLAGS {
            if !matches!(flag.disposition, Disposition::Active(_)) {
                continue;
            }
            let args = [OsString::from(flag.name)];
            assert!(
                !matches!(parse(&args), Err(CliError::Rejected(_))),
                "`{}` is Active but the parser routed it to a rejection/unknown",
                flag.name
            );
        }
    }

    #[test]
    fn rejected_residue_is_exactly_the_true_unsupported_flags() {
        // The rejected set is the true residue only: the genuinely-unsupported knobs.
        // Everything the audit called out (`-preset`, `-jpeg_like`, `-partition_limit`,
        // `-exact`) and P6b's rate control (`-size`/`-psnr`/`-pass`) is now Active.
        let rejected: Vec<&str> = FLAGS
            .iter()
            .filter(|f| matches!(f.disposition, Disposition::Rejected { .. }))
            .map(|f| f.name)
            .collect();
        assert_eq!(rejected, ["-blend_alpha", "-hint", "-af", "-pre", "-map"]);
    }
}
