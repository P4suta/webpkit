//! The `webp` brand tool: `encode` / `decode` / `convert` / `info`.
//!
//! It works over PNG, netpbm, and raw pixels with smart format detection and
//! metadata-preserving defaults, decodes animations frame-by-frame, and shares
//! the codec, format, I/O, and reporting layers with the `cwebp` / `dwebp`
//! drop-ins.

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};

use crate::{
    bulk,
    cli::{Layout, Method},
    codec::{self, EncodeMode},
    error::CliError,
    format::{self, InputFormat, OutputFormat, raw::RawParams},
    io::{Sink, Source},
    metadata::{MetadataField, Selection},
    report::{self, Reporter},
};

/// Encode, decode, and inspect WebP VP8L (lossless) images.
#[derive(Debug, Parser)]
#[command(name = "webp", version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

/// Flags available to every subcommand.
#[derive(Debug, clap::Args)]
struct GlobalArgs {
    /// Print per-stage detail on stderr.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Suppress all non-error output.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a WebP VP8L file to PNG (default), PPM/PAM, or raw pixels.
    Decode(DecodeArgs),
    /// Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy).
    Encode(EncodeArgs),
    /// Batch-convert many images (or directories) to WebP, in parallel.
    Convert(ConvertArgs),
    /// Print a summary of a WebP file (size, alpha, metadata, animation).
    Info(InfoArgs),
}

/// Arguments for `webp convert`.
#[derive(Debug, clap::Args)]
struct ConvertArgs {
    /// Input images and/or directories (PNG/PPM/PAM).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output directory (created outputs are `<stem>.webp`); default: beside input.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Encoder effort (ignored with --optimize).
    #[arg(short, long, value_enum, default_value_t)]
    method: Method,
    /// Encode lossily (VP8) instead of losslessly (VP8L).
    #[arg(long)]
    lossy: bool,
    /// Lossy quality 0-100 (higher = larger, closer to source); selects --lossy.
    #[arg(long)]
    quality: Option<u8>,
    /// Try every lossless effort level and keep the smallest output.
    #[arg(long)]
    optimize: bool,
    /// Recurse into subdirectories.
    #[arg(short, long)]
    recursive: bool,
    /// Metadata to embed: all,none,icc,exif,xmp (default: all).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Vec<MetadataField>,
}

/// Arguments for `webp info`.
#[derive(Debug, clap::Args)]
struct InfoArgs {
    /// Input `.webp` file; `-` (the default) reads stdin.
    #[arg(default_value = "-")]
    input: PathBuf,
}

/// Which frames of an animation to emit.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
enum FrameSelection {
    /// Only the first composited frame (the default).
    #[default]
    First,
    /// Every composited frame, numbered `<stem>-000.<ext>`, ...
    All,
}

/// Arguments for `webp decode`.
#[derive(Debug, clap::Args)]
struct DecodeArgs {
    /// Input `.webp` file; `-` (the default) reads stdin.
    #[arg(default_value = "-")]
    input: PathBuf,
    /// Output path; `-` writes stdout.
    #[arg(short, long)]
    output: PathBuf,
    /// Output format; defaults to the `-o` extension, else PNG.
    #[arg(long, value_enum)]
    format: Option<OutputFormat>,
    /// Byte order for raw output only.
    #[arg(long, value_enum, default_value_t)]
    layout: Layout,
    /// For animations: which frames to emit.
    #[arg(long, value_enum)]
    frames: Option<FrameSelection>,
    /// For animations: emit only this 0-based frame.
    #[arg(long, conflicts_with = "frames")]
    frame: Option<u32>,
    /// Metadata to carry into the output: all,none,icc,exif,xmp (default: all).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Vec<MetadataField>,
}

/// Arguments for `webp encode`.
#[derive(Debug, clap::Args)]
struct EncodeArgs {
    /// Input image (PNG/PPM/PAM/raw); `-` (the default) reads stdin.
    #[arg(default_value = "-")]
    input: PathBuf,
    /// Output `.webp` file; `-` writes stdout.
    #[arg(short, long)]
    output: PathBuf,
    /// Input format; defaults to the extension, else the magic bytes, else raw.
    #[arg(long, value_enum)]
    input_format: Option<InputFormat>,
    /// Raw-input width in pixels (required for raw input).
    #[arg(long)]
    width: Option<u32>,
    /// Raw-input height in pixels (required for raw input).
    #[arg(long)]
    height: Option<u32>,
    /// Byte order for raw input only.
    #[arg(long, value_enum, default_value_t)]
    layout: Layout,
    /// Encoder effort.
    #[arg(short, long, value_enum, default_value_t)]
    method: Method,
    /// Encode lossily (VP8) instead of losslessly (VP8L).
    #[arg(long)]
    lossy: bool,
    /// Lossy quality 0-100 (higher = larger, closer to source); selects --lossy.
    #[arg(long)]
    quality: Option<u8>,
    /// Metadata to embed: all,none,icc,exif,xmp (default: all — kinder than cwebp).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Vec<MetadataField>,
}

/// Build the [`EncodeMode`] from the shared effort/quality flags: `--lossy`, or an
/// explicit `--quality`, selects lossy (VP8) output; otherwise lossless (VP8L).
fn encode_mode(lossy: bool, quality: Option<u8>, method: Method) -> EncodeMode {
    if lossy || quality.is_some() {
        EncodeMode::Lossy {
            quality: quality.unwrap_or(75),
            method: method.into(),
        }
    } else {
        EncodeMode::Lossless(method.into())
    }
}

/// Parse arguments, run the requested command, and return a process exit code.
#[must_use]
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let reporter = Reporter::new(cli.global.verbose, cli.global.quiet);
    match run(&cli.command, &reporter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            crate::report::error(&err);
            err.exit_code()
        },
    }
}

fn run(command: &Command, reporter: &Reporter) -> Result<(), CliError> {
    match command {
        Command::Decode(args) => decode(args, reporter),
        Command::Encode(args) => encode(args, reporter),
        Command::Convert(args) => convert(args, reporter),
        Command::Info(args) => info(args),
    }
}

fn convert(args: &ConvertArgs, reporter: &Reporter) -> Result<(), CliError> {
    let options = bulk::Options {
        mode: encode_mode(args.lossy, args.quality, args.method),
        metadata: Selection::from_fields(&args.metadata),
        optimize: args.optimize,
        recursive: args.recursive,
        output_dir: args.output.clone(),
    };
    let outcome = bulk::convert(&args.inputs, &options)?;
    for (ok, text) in &outcome.lines {
        if *ok {
            reporter.detail(text);
        } else {
            report::warn(text);
        }
    }
    reporter.status(&format!(
        "converted {} file(s), {} -> {} bytes{}",
        outcome.converted,
        outcome.total_in,
        outcome.total_out,
        if outcome.failed > 0 {
            format!(" ({} failed)", outcome.failed)
        } else {
            String::new()
        },
    ));
    if outcome.failed > 0 {
        return Err(CliError::Format(format!(
            "{} file(s) failed to convert",
            outcome.failed
        )));
    }
    Ok(())
}

fn decode(args: &DecodeArgs, reporter: &Reporter) -> Result<(), CliError> {
    let source = Source::from_arg(&args.input);
    let sink = Sink::from_arg(&args.output);
    let bytes = source.read()?;
    let format = OutputFormat::resolve(args.format, sink.extension().as_deref());
    reporter.detail(&format!("decoding {} -> {format:?}", source.label()));

    // Image formats are always RGBA8; only raw honors the requested layout.
    let layout = if format == OutputFormat::Raw {
        args.layout.into()
    } else {
        webpkit::lossless::PixelLayout::Rgba8
    };
    if is_animated(&bytes)? {
        return decode_animation(args, &bytes, format, &sink, reporter);
    }
    let image = codec::decode(&bytes, layout)?;
    let metadata = Selection::from_fields(&args.metadata).apply(image.metadata());
    let out = format::write_image(&image, format, &metadata)?;
    sink.write(&out)?;
    reporter.status(&format!(
        "decoded {} -> {} ({}x{}, {} bytes)",
        source.label(),
        sink.label(),
        image.width(),
        image.height(),
        out.len(),
    ));
    Ok(())
}

/// Whether the WebP file is animated (an `ANIM` container). Codec-agnostic — works
/// for a lossy file the same as a lossless one, so `webp decode`/`info` handle both.
fn is_animated(bytes: &[u8]) -> Result<bool, CliError> {
    Ok(webpkit::is_animated(bytes)?)
}

/// Decode an animation, honoring `--frame` / `--frames`.
fn decode_animation(
    args: &DecodeArgs,
    bytes: &[u8],
    format: OutputFormat,
    sink: &Sink,
    reporter: &Reporter,
) -> Result<(), CliError> {
    let frames = webpkit::decode_frames(bytes)?;
    let images: Vec<webpkit::lossless::Image> = frames
        .composited()
        .map(|frame| frame.map(webpkit::lossless::CompositedFrame::into_image))
        .collect::<Result<_, _>>()?;
    if images.is_empty() {
        return Err(CliError::Format("animation has no frames".to_owned()));
    }
    let no_meta = webpkit::lossless::Metadata::none();

    if let Some(index) = args.frame {
        let image = images
            .get(index as usize)
            .ok_or_else(|| CliError::Usage(format!("frame {index} is out of range")))?;
        let out = format::write_image(image, format, &no_meta)?;
        sink.write(&out)?;
        reporter.status(&format!("decoded frame {index} -> {}", sink.label()));
        return Ok(());
    }

    match args.frames.unwrap_or_default() {
        FrameSelection::First => {
            let out = format::write_image(&images[0], format, &no_meta)?;
            sink.write(&out)?;
            reporter.status(&format!("decoded first frame -> {}", sink.label()));
        },
        FrameSelection::All => {
            let base = match sink {
                Sink::File(path) => path.clone(),
                Sink::Stdout => {
                    return Err(CliError::Usage(
                        "`--frames all` needs a file output, not stdout".to_owned(),
                    ));
                },
            };
            for (index, image) in images.iter().enumerate() {
                let path = numbered_path(&base, index);
                let out = format::write_image(image, format, &no_meta)?;
                Sink::File(path).write(&out)?;
            }
            reporter.status(&format!(
                "decoded {} frames -> {}",
                images.len(),
                base.display()
            ));
        },
    }
    Ok(())
}

/// Insert a zero-padded frame index before the extension: `out.png` → `out-000.png`.
fn numbered_path(base: &std::path::Path, index: usize) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let numbered = format!("{stem}-{index:03}");
    match base.extension() {
        Some(ext) => base.with_file_name(numbered).with_extension(ext),
        None => base.with_file_name(numbered),
    }
}

fn encode(args: &EncodeArgs, reporter: &Reporter) -> Result<(), CliError> {
    let source = Source::from_arg(&args.input);
    let sink = Sink::from_arg(&args.output);
    let bytes = source.read()?;
    let format = InputFormat::resolve(args.input_format, source.extension().as_deref(), &bytes);
    reporter.detail(&format!("encoding {format:?} {}", source.label()));

    let raw = match (args.width, args.height) {
        (Some(width), Some(height)) => Some(RawParams {
            width,
            height,
            layout: args.layout.into(),
        }),
        _ => None,
    };
    let image = format::read_image(&bytes, format, raw)?;
    let metadata = Selection::from_fields(&args.metadata).apply(image.metadata());
    let mode = encode_mode(args.lossy, args.quality, args.method);
    let webp = codec::encode(&image, mode, metadata)?;
    sink.write(&webp)?;
    reporter.status(&format!(
        "encoded {} -> {} ({}x{}, {} bytes, {})",
        source.label(),
        sink.label(),
        image.width(),
        image.height(),
        webp.len(),
        ratio(bytes.len(), webp.len()),
    ));
    Ok(())
}

/// Format the output-to-input size ratio as a percentage string, e.g. `41.2%`.
fn ratio(input_len: usize, output_len: usize) -> String {
    if input_len == 0 {
        return "n/a".to_owned();
    }
    let permille = output_len as u128 * 1000 / input_len as u128;
    format!("{}.{}%", permille / 10, permille % 10)
}

#[allow(
    clippy::print_stdout,
    reason = "info prints its report to stdout by design"
)]
fn info(args: &InfoArgs) -> Result<(), CliError> {
    let source = Source::from_arg(&args.input);
    let bytes = source.read()?;
    let mut lines = vec![format!("File:       {}", source.label())];
    if is_animated(&bytes)? {
        lines.extend(animation_lines(&bytes)?);
    } else {
        let image = webpkit::decode(&bytes)?;
        lines.extend(still_lines(&bytes, &image));
    }
    for line in &lines {
        println!("{line}");
    }
    Ok(())
}

/// Summary lines for a still image, labeling the codec from the container's
/// image chunk (`VP8L` lossless vs `VP8 ` lossy).
fn still_lines(bytes: &[u8], image: &webpkit::lossless::Image) -> Vec<String> {
    let format = if bytes.windows(4).any(|w| w == b"VP8L") {
        "WebP VP8L (lossless)"
    } else if bytes.windows(4).any(|w| w == b"VP8 ") {
        "WebP VP8 (lossy)"
    } else {
        "WebP"
    };
    vec![
        format!("Format:     {format}"),
        format!("Dimensions: {}x{}", image.width(), image.height()),
        format!("Alpha:      {}", yes_no(image.has_alpha())),
        format!("Metadata:   {}", metadata_summary(image.metadata())),
    ]
}

/// Summary lines for an animation.
fn animation_lines(bytes: &[u8]) -> Result<Vec<String>, CliError> {
    let frames = webpkit::decode_frames(bytes)?;
    let anim = frames.anim_info();
    let loops = if anim.loop_count == 0 {
        "forever".to_owned()
    } else {
        format!("{} time(s)", anim.loop_count)
    };
    let count = frames.composited().count();
    Ok(vec![
        "Format:     WebP animation".to_owned(),
        format!(
            "Canvas:     {}x{}",
            anim.canvas.width(),
            anim.canvas.height()
        ),
        format!("Loop:       {loops}"),
        format!("Frames:     {count}"),
    ])
}

/// Comma-list the metadata kinds present, or `none`.
fn metadata_summary(metadata: &webpkit::lossless::Metadata) -> String {
    let mut parts = Vec::new();
    if metadata.icc_profile.is_some() {
        parts.push("ICC");
    }
    if metadata.exif.is_some() {
        parts.push("Exif");
    }
    if metadata.xmp.is_some() {
        parts.push("XMP");
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
