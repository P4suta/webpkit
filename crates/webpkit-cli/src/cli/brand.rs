//! The `webp` brand tool: `encode` / `decode` / `convert` / `info`, plus
//! `completions` and `man`.
//!
//! It works over PNG, netpbm, and raw pixels with smart format detection and
//! metadata-preserving defaults, decodes animations frame-by-frame, and shares
//! the codec, format, I/O, and reporting layers with the `cwebp` / `dwebp`
//! drop-ins.
//!
//! `completions` and `man` generate from this module's own [`Cli`], so the help,
//! the completion scripts, and the man pages cannot describe different flags.

use std::{path::PathBuf, process::ExitCode};

use clap::{CommandFactory as _, Parser, Subcommand};
use webpkit::DecodeOptions;

use crate::{
    bulk,
    cli::{Layout, Method},
    codec::{self, EncodeMode},
    config,
    error::CliError,
    format::{self, InputFormat, OutputFormat, raw::RawParams},
    inspect,
    io::{Sink, Source},
    metadata::{MetadataField, Selection},
    report::{self, Reporter},
    term::{self, ColorChoice},
};

/// Encode, decode, and inspect WebP images.
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
    /// auto, always, or never
    ///
    /// `CLICOLOR_FORCE` and `NO_COLOR` are honored, and an explicit `--color`
    /// outranks both. Left alone, messages are colored only when stderr is a
    /// terminal — so a pipe or a log file never receives escape codes.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t,
        value_name = "WHEN",
        hide_possible_values = true
    )]
    color: ColorChoice,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a WebP file to PNG (default), PPM/PAM, or raw pixels.
    Decode(DecodeArgs),
    /// Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy).
    Encode(EncodeArgs),
    /// Batch-convert many images (or directories) to WebP, in parallel.
    Convert(ConvertArgs),
    /// Print a summary of a WebP file (size, alpha, metadata, animation).
    Info(InfoArgs),
    /// Show resolved settings and where each came from (args, env, file, default).
    ///
    /// Settings resolve highest-priority-first: command-line arguments, then
    /// `WEBP_*` environment variables, then a `webp.toml` (found by walking up
    /// from the working directory, then in your user config directory), then the
    /// built-in defaults. Each value is printed with its origin.
    Config(ConfigArgs),
    /// Explain an exit code: what a failing run's status number means.
    Explain(ExplainArgs),
    /// Print a shell completion script.
    ///
    /// The script is generated from the same command tree that parses your
    /// arguments, so it cannot describe a flag that does not exist.
    ///
    /// Load it for the current shell, or install it where your shell looks:
    ///
    ///   bash:        eval "$(webp completions bash)"
    ///   zsh:         webp completions zsh > ~/.zfunc/_webp
    ///   fish:        webp completions fish > ~/.config/fish/completions/webp.fish
    ///   powershell:  webp completions powershell | Out-String | Invoke-Expression
    Completions(CompletionsArgs),
    /// Print a man page in roff, for `man -l -` or a package's man directory.
    Man(ManArgs),
}

/// Arguments for `webp completions`.
#[derive(Debug, clap::Args)]
struct CompletionsArgs {
    /// The shell to generate for.
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

/// Arguments for `webp config`.
///
/// The override flags feed the args layer, so `webp config --quality 90` shows
/// that setting resolving to `argument`. They are plain `Option`s with no clap
/// default: a default would make every field claim the args layer and shadow the
/// file, so the defaults live only in the config table's lowest layer.
#[derive(Debug, clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
    /// Print the resolved settings as JSON (stable key order).
    #[arg(long)]
    json: bool,
    /// Print a commented `webp.toml` template to stdout.
    #[arg(long, conflicts_with = "json")]
    template: bool,
    /// Override: lossy quality 0-100.
    #[arg(long, value_name = "0-100")]
    quality: Option<u8>,
    /// Override: encoder effort.
    #[arg(long, value_enum)]
    effort: Option<Method>,
    /// Override: lossless or lossy.
    #[arg(long, value_enum)]
    codec: Option<config::Codec>,
    /// Override: metadata to carry (all,none,icc,exif,xmp).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Option<Vec<MetadataField>>,
    /// Override: worker threads (0 = one per core).
    #[arg(long)]
    threads: Option<u16>,
    /// Override: decode pixel cap (N, 300M, 2G, or none).
    #[arg(long, value_name = "N|none")]
    max_pixels: Option<String>,
}

/// A `webp config` sub-action; absent means "show the resolved table".
#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Print a single setting's resolved value, with nothing else.
    Get(ConfigGetArgs),
}

/// Arguments for `webp config get`.
#[derive(Debug, clap::Args)]
struct ConfigGetArgs {
    /// The setting to print, e.g. `quality`.
    key: String,
}

/// Arguments for `webp explain`.
#[derive(Debug, clap::Args)]
struct ExplainArgs {
    /// An exit code (`0`, `2`..`9`) or its short name (`usage`, `limit`, ...).
    #[arg(value_name = "CODE")]
    code: String,
}

/// Arguments for `webp man`.
#[derive(Debug, clap::Args)]
struct ManArgs {
    /// Document this subcommand instead of the tool itself.
    #[arg(value_name = "COMMAND")]
    command: Option<String>,
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
    /// Print the report as JSON instead of text.
    ///
    /// One object, with a `schema` field to pin. Nothing is decoded either way,
    /// so this is cheap on any size of file.
    #[arg(long)]
    json: bool,
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
pub(crate) fn main() -> ExitCode {
    let cli = Cli::parse();
    term::install(cli.global.color);
    let reporter = Reporter::new(cli.global.verbose, cli.global.quiet);
    match run(&cli.command, &reporter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            crate::report::error(&err.to_diagnostic());
            err.exit_code()
        },
    }
}

fn run(command: &Command, reporter: &Reporter) -> Result<(), CliError> {
    match command {
        Command::Decode(args) => decode(args, reporter),
        Command::Encode(args) => encode(args, reporter),
        Command::Convert(args) => convert(args, reporter),
        Command::Info(args) => info(args, reporter),
        Command::Config(args) => config_cmd(args),
        Command::Explain(args) => explain(&args.code),
        Command::Completions(args) => {
            completions(args);
            Ok(())
        },
        Command::Man(args) => man(args.command.as_deref()),
    }
}

/// Print a completion script for `shell`, generated from this tool's own
/// `clap::Command`.
fn completions(args: &CompletionsArgs) {
    let mut command = Cli::command();
    let name = command.get_name().to_owned();
    let mut script = Vec::new();
    clap_complete::generate(args.shell, &mut command, name, &mut script);
    emit(&script);
}

/// Print the man page for the tool, or for one of its subcommands.
fn man(subcommand: Option<&str>) -> Result<(), CliError> {
    let root = Cli::command();
    let page = match subcommand {
        None => clap_mangen::Man::new(root),
        Some(name) => {
            let found = root
                .find_subcommand(name)
                .ok_or_else(|| CliError::Usage(format!("`{name}` is not a webp command")))?
                .clone();
            // The title has to be set explicitly: left alone the page is `ENCODE(1)`,
            // naming a program nobody can run. `webp-encode` is what it installs as.
            clap_mangen::Man::new(found).title(format!("webp-{name}"))
        },
    };
    let mut roff = Vec::new();
    page.render(&mut roff)
        .map_err(|err| CliError::write_output("<stdout>".to_owned(), err))?;
    emit(&roff);
    Ok(())
}

/// Resolve settings and print them, `config get <key>`, or the template.
fn config_cmd(args: &ConfigArgs) -> Result<(), CliError> {
    if args.template {
        report::out(config::template().trim_end());
        return Ok(());
    }
    let resolution = config::resolve(config_args_layer(args)?)?;
    if let Some(ConfigAction::Get(get)) = &args.action {
        let value = resolution.settings.get(&get.key).ok_or_else(|| {
            CliError::Usage(format!(
                "`{}` is not a setting; known settings are {}",
                get.key,
                config::KEYS.join(", "),
            ))
        })?;
        report::out(&value);
        return Ok(());
    }
    if args.json {
        let json = serde_json::to_string_pretty(&resolution.settings.to_json())
            .map_err(|err| CliError::Format(format!("serializing config: {err}")))?;
        report::out(&json);
        return Ok(());
    }
    for line in config::render_report(&resolution) {
        report::out(&line);
    }
    Ok(())
}

/// Pair an args-layer value with [`config::Origin::Args`].
const fn from_args<T>(value: T) -> config::Sourced<T> {
    config::Sourced::new(value, config::Origin::Args)
}

/// The args layer: the config-override flags the user actually passed, each tagged
/// [`config::Origin::Args`]. An unset flag stays absent so it cannot shadow a
/// lower layer.
fn config_args_layer(args: &ConfigArgs) -> Result<config::Partial, CliError> {
    let mut partial = config::Partial::default();
    if let Some(quality) = args.quality {
        partial.quality = Some(from_args(
            config::Quality::new(quality).map_err(CliError::Usage)?,
        ));
    }
    if let Some(effort) = args.effort {
        partial.effort = Some(from_args(effort.into()));
    }
    if let Some(codec) = args.codec {
        partial.codec = Some(from_args(codec));
    }
    if let Some(fields) = &args.metadata {
        partial.metadata = Some(from_args(Selection::from_fields(fields)));
    }
    if let Some(threads) = args.threads {
        partial.threads = Some(from_args(threads));
    }
    if let Some(spec) = &args.max_pixels {
        partial.max_pixels = Some(from_args(
            config::MaxPixels::parse(spec).map_err(CliError::Usage)?,
        ));
    }
    Ok(partial)
}

/// Print the meaning of an exit code, an offline reference for the contract that
/// `webp`'s exit status encodes.
fn explain(code: &str) -> Result<(), CliError> {
    for line in crate::error::explain(code)? {
        report::out(&line);
    }
    Ok(())
}

/// Write generated bytes to stdout as a report.
///
/// Both generators emit ASCII, so the lossy conversion cannot alter them; going
/// through `report` keeps every write to a standard stream in one module.
fn emit(bytes: &[u8]) {
    report::out(String::from_utf8_lossy(bytes).trim_end_matches('\n'));
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
        webpkit::PixelLayout::Rgba8
    };
    // One resolved `DecodeOptions` for both paths. Passing the loose flags is how
    // `--layout` came to be honored for stills and silently dropped for
    // animations: `decode_animation` took `format` but not `layout`, so nothing
    // could notice the omission.
    let options = DecodeOptions::new().layout(layout);
    if is_animated(&bytes)? {
        return decode_animation(args, &bytes, format, &options, &sink, reporter);
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
///
/// Compositing is stateful — a frame is painted onto the canvas its predecessors
/// left — so reaching frame N genuinely costs frames `0..=N`. It costs no more
/// than that: the walk is driven per selection rather than collected up front, so
/// the default (`--frames first`) decodes one frame of a hundred-frame animation
/// instead of all hundred, and only `--frames all` holds more than one canvas.
fn decode_animation(
    args: &DecodeArgs,
    bytes: &[u8],
    format: OutputFormat,
    options: &DecodeOptions,
    sink: &Sink,
    reporter: &Reporter,
) -> Result<(), CliError> {
    let no_meta = webpkit::Metadata::none();
    let composited = |take: Option<u32>| -> Result<Vec<webpkit::Image>, CliError> {
        let frames = webpkit::decode_frames_with(bytes, options)?;
        let walk = frames
            .composited()
            .map(|frame| frame.map(webpkit::CompositedFrame::into_image));
        match take {
            Some(n) => walk.take(n as usize + 1).collect::<Result<_, _>>(),
            None => walk.collect::<Result<_, _>>(),
        }
        .map_err(CliError::from)
    };

    if let Some(index) = args.frame {
        let images = composited(Some(index))?;
        let image = images
            .get(index as usize)
            .ok_or_else(|| CliError::Usage(format!("frame {index} is out of range")))?;
        sink.write(&format::write_image(image, format, &no_meta)?)?;
        reporter.status(&format!("decoded frame {index} -> {}", sink.label()));
        return Ok(());
    }

    match args.frames.unwrap_or_default() {
        FrameSelection::First => {
            let images = composited(Some(0))?;
            let image = images
                .first()
                .ok_or_else(|| CliError::Format("animation has no frames".to_owned()))?;
            sink.write(&format::write_image(image, format, &no_meta)?)?;
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
            let images = composited(None)?;
            if images.is_empty() {
                return Err(CliError::Format("animation has no frames".to_owned()));
            }
            for (index, image) in images.iter().enumerate() {
                let out = format::write_image(image, format, &no_meta)?;
                Sink::File(numbered_path(&base, index)).write(&out)?;
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

fn info(args: &InfoArgs, reporter: &Reporter) -> Result<(), CliError> {
    let source = Source::from_arg(&args.input);
    let bytes = source.read()?;
    let report = inspect::report(&bytes, source.label())?;

    if args.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| CliError::Format(format!("serializing the report: {err}")))?;
        report::out(&json);
        return Ok(());
    }
    for line in info_lines(&report) {
        report::out(&line);
    }
    // The chunk table is the `webpinfo` half: useful when you are debugging a
    // container, noise when you are not.
    for line in chunk_lines(&report) {
        reporter.detail(&line);
    }
    Ok(())
}

/// The text report: one `Label: value` per line.
fn info_lines(report: &inspect::Report) -> Vec<String> {
    let mut lines = vec![
        format!("File:       {}", report.path),
        format!("Format:     {}", format_line(report)),
        format!("Size:       {} bytes", report.bytes),
    ];
    if let Some(anim) = report.animation {
        lines.push(format!("Canvas:     {}x{}", report.width, report.height));
        lines.push(format!("Loop:       {}", loop_line(anim.loop_count)));
        lines.push(format!("Frames:     {}", anim.frames));
        lines.push(format!("Duration:   {} ms", anim.duration_ms));
    } else {
        lines.push(format!("Dimensions: {}x{}", report.width, report.height));
    }
    lines.push(format!("Alpha:      {}", yes_no(report.alpha)));
    lines.push(format!("Metadata:   {}", metadata_line(report.metadata)));
    lines
}

/// `webpinfo`-style chunk dump, shown under `-v`.
fn chunk_lines(report: &inspect::Report) -> Vec<String> {
    let mut lines = vec!["Chunks:".to_owned()];
    for chunk in &report.chunks {
        lines.push(format!("  {:<6} {:>9} bytes", chunk.fourcc, chunk.bytes));
    }
    lines
}

fn format_line(report: &inspect::Report) -> String {
    match report.container {
        inspect::Container::Animation => format!("WebP animation ({})", report.codec),
        inspect::Container::Extended => format!("WebP {} (extended)", report.codec),
        inspect::Container::Simple => format!("WebP {}", report.codec),
    }
}

fn loop_line(count: u16) -> String {
    if count == 0 {
        "forever".to_owned()
    } else {
        format!("{count} time(s)")
    }
}

/// Comma-list the metadata kinds present, with sizes, or `none`.
fn metadata_line(metadata: inspect::MetadataInfo) -> String {
    if !metadata.any() {
        return "none".to_owned();
    }
    [
        ("ICC", metadata.icc),
        ("Exif", metadata.exif),
        ("XMP", metadata.xmp),
    ]
    .iter()
    .filter(|(_, field)| field.present)
    .map(|(name, field)| format!("{name} {} bytes", field.bytes))
    .collect::<Vec<_>>()
    .join(", ")
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
