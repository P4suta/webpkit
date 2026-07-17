//! The `webp` brand tool: a bare direction-detected form plus `encode` /
//! `decode` / `convert` / `info` / `diff` / `doctor`, `config` / `explain`, and
//! `completions` / `man`.
//!
//! Called bare, it sniffs each input and picks a direction (WebP → PNG, anything
//! else → WebP) with an implicit, overwrite-guarded output name. It reads
//! PNG/JPEG/GIF/TIFF/BMP/netpbm/raw, turns a GIF into an animated WebP, keeps
//! metadata by default, decodes animations frame-by-frame, and shares the codec,
//! format, I/O, and reporting layers with the `cwebp` / `dwebp` drop-ins.
//!
//! `completions` and `man` generate from this module's own [`Cli`], so the help,
//! the completion scripts, and the man pages cannot describe different flags.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{CommandFactory as _, Parser, Subcommand};
use webpkit::{DEFAULT_MAX_PIXELS, DecodeOptions};

use crate::{
    bulk,
    cli::{Layout, Method},
    codec::{self, EncodeMode},
    config,
    error::CliError,
    format::{self, InputFormat, OutputFormat, raw::RawParams},
    inspect,
    io::{self, Sink, Source},
    metadata::{MetadataField, Selection},
    preprocess::{Crop, Pipeline, Resize},
    report::{self, Reporter},
    strategy::{Strategy, Target},
    term::{self, ColorChoice},
};

/// Encode, decode, and inspect WebP images.
///
/// Called bare, `webp` picks a direction from the input's content: a WebP is
/// decoded to PNG, anything else is encoded to WebP, and the output name is
/// derived (`photo.png` → `photo.webp`, `photo.webp` → `photo.png`). Use `encode`
/// / `decode` to force a direction.
#[derive(Debug, Parser)]
#[command(
    name = "webp",
    version,
    about,
    long_about = None,
    args_conflicts_with_subcommands = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(flatten)]
    auto: AutoArgs,
    #[command(subcommand)]
    command: Option<Command>,
}

/// The subcommand form: `webp <command> ...`, with a required command.
///
/// A bare top-level positional (the [`AutoArgs`] inputs) would swallow a
/// subcommand name that follows a global flag (`webp --quiet encode …`), so
/// subcommand parsing uses this positional-free struct. Both forms are generated
/// from the same [`Command`] table, so nothing can drift; only the completions and
/// man pages, which describe the bare form too, are built from [`Cli`].
#[derive(Debug, Parser)]
#[command(name = "webp", version, about, long_about = None)]
struct SubCli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

/// The subcommands whose presence as the first positional selects [`SubCli`].
const SUBCOMMANDS: &[&str] = &[
    "decode",
    "encode",
    "convert",
    "info",
    "diff",
    "doctor",
    "config",
    "explain",
    "completions",
    "man",
    "help",
];

/// Whether the first positional argument names a subcommand.
///
/// Only global flags may precede a subcommand, and of those `--color` and
/// `--threads` take a value, so the scan skips their argument. Anything else at the
/// first positional (a file, a quality value) means the bare form.
fn first_positional_is_subcommand(argv: &[std::ffi::OsString]) -> bool {
    let mut i = 1;
    while let Some(arg) = argv.get(i) {
        let token = arg.to_string_lossy();
        if token == "--" {
            return false;
        }
        if token == "-" || !token.starts_with('-') {
            return SUBCOMMANDS.contains(&token.as_ref());
        }
        // `--color WHEN` / `--threads N` consume the next token (unless given as
        // `--flag=value`); every other global flag does not.
        let takes_value = token == "--color" || token == "--threads";
        i += usize::from(takes_value) + 1;
    }
    false
}

/// The bare, direction-detected form: `webp <inputs...>`.
///
/// These flags are only read when no subcommand is given. `-q`/`--quality`
/// selects lossy; `--lossless`/`--lossy` force a codec; otherwise the codec is
/// derived from the source (JPEG → lossy, everything else → lossless).
#[derive(Debug, clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "clap flag struct: each bool is an independent switch, not modeled state"
)]
struct AutoArgs {
    /// Images or directories. A WebP is decoded to PNG; anything else is encoded.
    inputs: Vec<PathBuf>,
    /// Output file, or a directory for many inputs; default: beside each input.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Lossy quality 0-100 (higher = larger, closer to source); selects lossy.
    #[arg(short = 'q', long)]
    quality: Option<u8>,
    /// Force lossless (VP8L) encoding.
    #[arg(long, conflicts_with = "lossy")]
    lossless: bool,
    /// Force lossy (VP8) encoding.
    #[arg(long)]
    lossy: bool,
    /// Encoder effort [default: balanced, or from env/config].
    #[arg(short, long, value_enum)]
    method: Option<Method>,
    /// Metadata to embed: all,none,icc,exif,xmp (default: all).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Vec<MetadataField>,
    /// Crop before encoding: `x,y,width,height` in pixels (applied before --resize).
    #[arg(long, value_name = "X,Y,W,H")]
    crop: Option<String>,
    /// Resize before encoding: `WxH` (use 0 on one axis to keep aspect).
    #[arg(long, value_name = "WxH")]
    resize: Option<String>,
    /// Recurse into subdirectories.
    #[arg(short, long)]
    recursive: bool,
    /// Overwrite an existing derived output.
    #[arg(long)]
    force: bool,
    /// Skip an existing derived output instead of failing (still exits 0).
    #[arg(long, conflicts_with = "force")]
    no_clobber: bool,
}

/// Flags available to every subcommand.
#[derive(Debug, clap::Args)]
struct GlobalArgs {
    /// Print per-stage detail on stderr.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Suppress all non-error output.
    ///
    /// Long-only: `-q` is `--quality`. A bare `webp -q photo.png` therefore reads
    /// `photo.png` as a quality value and fails; use `--quiet` to silence output.
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// auto, always, or never [default: auto, or from env/config].
    ///
    /// `CLICOLOR_FORCE` and `NO_COLOR` are honored; an explicit `--color` outranks
    /// them and `WEBP_COLOR`/`webp.toml`. Left alone, messages are colored only when
    /// stderr is a terminal — so a pipe or a log file never receives escape codes.
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "WHEN",
        hide_possible_values = true
    )]
    color: Option<ColorChoice>,
    /// Worker threads for parallel work; 0 (the default) uses one per core.
    ///
    /// One global thread pool is built, which the encoder's parallel `best`
    /// search draws from too — so this single number bounds every layer of
    /// parallelism, batch conversion and per-image search alike.
    #[arg(long, global = true, value_name = "N")]
    threads: Option<u16>,
    /// Report what would be written, without encoding or writing anything.
    #[arg(long, global = true)]
    dry_run: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a WebP file to PNG (default), PPM/PAM, or raw pixels.
    Decode(DecodeArgs),
    /// Encode an image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw) into a WebP file.
    Encode(EncodeArgs),
    /// Batch-convert many images (or directories) to WebP, in parallel.
    Convert(ConvertArgs),
    /// Print a summary of a WebP file (size, alpha, metadata, animation).
    Info(InfoArgs),
    /// Compare two images: report PSNR and the largest per-channel difference.
    ///
    /// Both inputs are decoded to RGBA (a WebP, or any readable image), so a source
    /// and its WebP, or two WebP files, can be compared. With `--min-psnr`, exit 1 when
    /// the PSNR is below the threshold — the grep/diff convention, for CI gates.
    Diff(DiffArgs),
    /// Diagnose the environment: PATH drop-in shadows, config, terminal, threads.
    ///
    /// Exits 0 even with warnings; 1 only on a real error (an invalid config file).
    Doctor,
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
    /// An exit code (`0`..`9`) or its short name (`usage`, `limit`, ...).
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "clap flag struct: each bool is an independent switch, not modeled state"
)]
struct ConvertArgs {
    /// Input images and/or directories (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output directory (created outputs are `<stem>.webp`); default: beside input.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Encoder effort (ignored with --optimize) [default: balanced, or from env/config].
    #[arg(short, long, value_enum)]
    method: Option<Method>,
    /// Force lossless (VP8L). The default is source-derived: JPEG → lossy, else lossless.
    #[arg(long, conflicts_with = "lossy")]
    lossless: bool,
    /// Encode lossily (VP8) instead of losslessly (VP8L).
    #[arg(long)]
    lossy: bool,
    /// Lossy quality 0-100 (higher = larger, closer to source); selects --lossy.
    #[arg(short = 'q', long)]
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
    /// Overwrite an existing output.
    #[arg(long)]
    force: bool,
    /// Skip an input whose `.webp` output exists (still exits 0).
    #[arg(long, conflicts_with = "force")]
    no_clobber: bool,
}

/// Arguments for `webp diff`.
#[derive(Debug, clap::Args)]
struct DiffArgs {
    /// The first image (a WebP, or any readable format).
    a: PathBuf,
    /// The second image, compared against the first (same dimensions required).
    b: PathBuf,
    /// Fail (exit 1) if the RGB PSNR is below this many decibels.
    #[arg(long, value_name = "DB")]
    min_psnr: Option<f64>,
    /// Print the comparison as JSON instead of text.
    #[arg(long)]
    json: bool,
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
    /// Input image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw); `-` (default) reads stdin.
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
    /// Encoder effort [default: balanced, or from env/config].
    #[arg(short, long, value_enum)]
    method: Option<Method>,
    /// Force lossless (VP8L). The default is source-derived: JPEG → lossy, else lossless.
    #[arg(long, conflicts_with = "lossy")]
    lossless: bool,
    /// Encode lossily (VP8) instead of losslessly (VP8L).
    #[arg(long)]
    lossy: bool,
    /// Lossy quality 0-100 (higher = larger, closer to source); selects --lossy.
    #[arg(short = 'q', long)]
    quality: Option<u8>,
    /// Crop before encoding: `x,y,width,height` in pixels (applied before --resize).
    #[arg(long, value_name = "X,Y,W,H")]
    crop: Option<String>,
    /// Resize before encoding: `WxH` (use 0 on one axis to keep aspect).
    #[arg(long, value_name = "WxH")]
    resize: Option<String>,
    /// Target output size, e.g. `200k` or `2M`, found by searching lossy quality.
    ///
    /// Lossy only: it bisects quality until the encoded file fits. With `-v`, the
    /// search is printed. Cannot combine with `--lossless`.
    #[arg(long, value_name = "SIZE")]
    target_size: Option<String>,
    /// Target reconstruction PSNR floor in dB (lossy only; pairs with --target-size).
    #[arg(long, value_name = "DB")]
    min_psnr: Option<f64>,
    /// Metadata to embed: all,none,icc,exif,xmp (default: all — kinder than cwebp).
    #[arg(long, value_enum, value_delimiter = ',')]
    metadata: Vec<MetadataField>,
}

/// The codec flags this invocation passed, as [`codec::CodecFlags`].
/// Fold the encode CLI flags over env (`WEBP_*`) and `webp.toml`, so a setting a
/// flag does not carry still honors the environment and the config file — the same
/// resolution `webp config` reports.
///
/// This is what stops `webp config` from lying: without it, `WEBP_QUALITY` /
/// `WEBP_CODEC` / `WEBP_EFFORT` / `WEBP_METADATA` were shown by `webp config` as
/// applied, yet ignored by every actual encode. A flag the user passed lands in the
/// args layer, so it still wins.
fn encode_settings(
    lossless: bool,
    lossy: bool,
    quality: Option<u8>,
    method: Option<Method>,
    metadata: &[MetadataField],
) -> Result<config::Settings, CliError> {
    let mut layer = config::Partial::default();
    if let Some(quality) = quality {
        layer.quality = Some(from_args(
            config::Quality::new(quality).map_err(CliError::Usage)?,
        ));
    }
    if let Some(method) = method {
        layer.effort = Some(from_args(method.into()));
    }
    if lossless {
        layer.codec = Some(from_args(config::Codec::Lossless));
    } else if lossy {
        layer.codec = Some(from_args(config::Codec::Lossy));
    }
    if !metadata.is_empty() {
        layer.metadata = Some(from_args(Selection::from_fields(metadata)));
    }
    Ok(config::resolve(layer)?.settings)
}

/// The thread count after folding `--threads` over `WEBP_THREADS` and `webp.toml`.
///
/// Without this, `webp config` reported `WEBP_THREADS` as applied while only the
/// `--threads` flag ever bounded the pool — the same lie the encode settings had.
/// `0` means one worker per core; a resolution error (a malformed env/file value)
/// falls back to the flag alone rather than failing before any work starts.
fn resolved_threads(flag: Option<u16>) -> Option<u16> {
    let mut layer = config::Partial::default();
    if let Some(threads) = flag {
        layer.threads = Some(from_args(threads));
    }
    config::resolve(layer).map_or(flag, |resolution| Some(resolution.settings.threads.value))
}

/// The decode pixel cap after folding `WEBP_MAX_PIXELS` and `webp.toml`. No CLI
/// flag sets it outside `webp config`, so a real decode honors it only through the
/// environment or a config file — exactly what `webp config` reports. `None` is
/// unbounded; a resolution error keeps the built-in default cap.
fn resolved_max_pixels() -> Option<u64> {
    config::resolve(config::Partial::default()).map_or(Some(DEFAULT_MAX_PIXELS), |resolution| {
        match resolution.settings.max_pixels.value {
            config::MaxPixels::Limited(limit) => Some(limit),
            config::MaxPixels::Unbounded => None,
        }
    })
}

/// The color choice after folding `--color` over `WEBP_COLOR` and `webp.toml`.
///
/// Without this, only the `--color` flag reached `term::install`, while `webp
/// config` reported `WEBP_COLOR`/a `color = ...` file value as applied — the same
/// lie the encode settings and thread count carried. Precedence: flag, then env,
/// then file, then the `auto` default; a resolution error falls back to the flag.
fn resolved_color(flag: Option<ColorChoice>) -> ColorChoice {
    let mut layer = config::Partial::default();
    if let Some(choice) = flag {
        layer.color = Some(from_args(choice));
    }
    config::resolve(layer).map_or_else(
        |_| flag.unwrap_or_default(),
        |resolution| resolution.settings.color.value,
    )
}

/// Translate resolved settings into codec flags, honoring provenance.
///
/// A codec or quality with a non-default origin (CLI, env, or file) selects the
/// codec; when both are defaults, the flags stay empty so [`codec::resolve_mode`]
/// applies the source-derived default (JPEG → lossy, else lossless). `force_lossy`
/// covers a size/PSNR target, which selects lossy without being a config setting.
fn flags_of(settings: &config::Settings, force_lossy: bool) -> codec::CodecFlags {
    let codec_set = !matches!(settings.codec.origin, config::Origin::Default);
    let quality_set = !matches!(settings.quality.origin, config::Origin::Default);
    let lossless = codec_set && settings.codec.value == config::Codec::Lossless;
    let lossy = force_lossy
        || (codec_set && settings.codec.value == config::Codec::Lossy)
        || (quality_set && !lossless);
    // Quality is only meaningful for lossy; a lossless codec drops it (rather than
    // tripping `resolve_mode`'s lossless-plus-quality conflict on an env value).
    let quality = (quality_set && !lossless).then_some(settings.quality.value.0);
    codec::CodecFlags {
        lossless,
        lossy,
        quality,
        effort: settings.effort.value,
    }
}

/// A one-phrase description of the chosen codec for the status line, e.g.
/// `lossless · from PNG source` or `lossy q75 · from JPEG source`.
fn codec_note(mode: EncodeMode, format: InputFormat, animation: bool) -> String {
    let codec = if animation {
        "animation (lossless)".to_owned()
    } else {
        match mode {
            EncodeMode::Lossless(_) => "lossless".to_owned(),
            EncodeMode::Lossy { quality, .. } => format!("lossy q{quality}"),
        }
    };
    format!("{codec} · from {format:?} source")
}

/// Build a crop-then-resize [`Pipeline`] from the optional `--crop`/`--resize` specs.
fn pipeline_of(crop: Option<&str>, resize: Option<&str>) -> Result<Pipeline, CliError> {
    let crop = crop.map(Crop::parse).transpose()?;
    let resize = resize.map(Resize::parse).transpose()?;
    Ok(Pipeline::new(crop, resize))
}

/// Parse a `--target-size` spec: a byte count with an optional `k`/`m`/`g` suffix
/// (binary, `1k = 1024`), e.g. `200k`, `1024`, `2M`.
fn parse_target_size(spec: &str) -> Result<u64, CliError> {
    let spec = spec.trim();
    let (digits, scale) = match spec.chars().last() {
        Some('k' | 'K') => (&spec[..spec.len() - 1], 1u64 << 10),
        Some('m' | 'M') => (&spec[..spec.len() - 1], 1u64 << 20),
        Some('g' | 'G') => (&spec[..spec.len() - 1], 1u64 << 30),
        _ => (spec, 1),
    };
    let number: u64 = digits.trim().parse().map_err(|_| {
        CliError::Usage(format!(
            "`--target-size` expected a byte count (optionally suffixed k/m/g), got `{spec}`"
        ))
    })?;
    number
        .checked_mul(scale)
        .ok_or_else(|| CliError::Usage(format!("`--target-size` is impossibly large: `{spec}`")))
}

/// The encode target from `--target-size` / `--min-psnr`, if either was given.
fn target_of(target_size: Option<&str>, min_psnr: Option<f64>) -> Result<Option<Target>, CliError> {
    let bytes = target_size.map(parse_target_size).transpose()?;
    Ok(Target::from_flags(bytes, min_psnr))
}

/// Parse arguments, run the requested command, and return a process exit code.
///
/// A subcommand invocation and the bare direction-detected form parse through
/// different structs (see [`SubCli`]); the first positional decides which.
#[must_use]
pub(crate) fn main() -> ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if first_positional_is_subcommand(&argv) {
        let cli = match SubCli::try_parse_from(&argv) {
            Ok(cli) => cli,
            Err(err) => return parse_error(&err),
        };
        term::install(resolved_color(cli.global.color));
        codec::configure_threads(resolved_threads(cli.global.threads));
        let reporter = Reporter::new(cli.global.verbose, cli.global.quiet).dry(cli.global.dry_run);
        finish(run(&cli.command, &reporter))
    } else {
        let cli = match Cli::try_parse_from(&argv) {
            Ok(cli) => cli,
            Err(err) => return parse_error(&err),
        };
        term::install(resolved_color(cli.global.color));
        codec::configure_threads(resolved_threads(cli.global.threads));
        let reporter = Reporter::new(cli.global.verbose, cli.global.quiet).dry(cli.global.dry_run);
        let result = match &cli.command {
            Some(command) => run(command, &reporter),
            None => auto(&cli.auto, &reporter).map(|()| ExitCode::SUCCESS),
        };
        finish(result)
    }
}

/// Turn a command result into a process exit code, rendering any error.
///
/// The `Ok` value is the exit code the command chose: `SUCCESS` for most, but
/// `diff`/`doctor` return `1` when their predicate is false (the grep/diff
/// convention) rather than raising an error.
fn finish(result: Result<ExitCode, CliError>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(err) => {
            report::error(&err.to_diagnostic());
            err.exit_code()
        },
    }
}

/// Render a clap parse failure, adding a `-q` → `--quiet` hint when the user
/// clearly meant the old quiet flag: `-q` now takes a quality value, so a
/// non-numeric argument to it is the tell.
fn parse_error(err: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind;

    let _ = err.print();
    let bad_value = matches!(
        err.kind(),
        ErrorKind::ValueValidation | ErrorKind::InvalidValue
    );
    if bad_value && err.render().to_string().contains("--quality") {
        report::warn(
            "`-q` is now --quality (a number). To silence output, use `--quiet` (long form).",
        );
    }
    // clap uses exit code 2 for both usage errors and --help/--version; mirror it.
    ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2))
}

fn run(command: &Command, reporter: &Reporter) -> Result<ExitCode, CliError> {
    let ok = ExitCode::SUCCESS;
    match command {
        Command::Decode(args) => decode(args, reporter).map(|()| ok),
        Command::Encode(args) => encode(args, reporter).map(|()| ok),
        Command::Convert(args) => convert(args, reporter).map(|()| ok),
        Command::Info(args) => info(args, reporter).map(|()| ok),
        Command::Diff(args) => diff_cmd(args, reporter),
        Command::Doctor => Ok(crate::doctor::run()),
        Command::Config(args) => config_cmd(args).map(|()| ok),
        Command::Explain(args) => explain(&args.code).map(|()| ok),
        Command::Completions(args) => {
            completions(args);
            Ok(ok)
        },
        Command::Man(args) => man(args.command.as_deref()).map(|()| ok),
    }
}

/// Compare two images, print the result, and reflect `--min-psnr` in the exit code.
///
/// The report goes to stdout (like `info`); a failed `--min-psnr` predicate exits
/// `1` with a note on stderr, leaving stdout's report intact.
fn diff_cmd(args: &DiffArgs, reporter: &Reporter) -> Result<ExitCode, CliError> {
    let comparison = crate::diff::compare(&args.a, &args.b)?;
    if args.json {
        let json = serde_json::to_string_pretty(&comparison)
            .map_err(|err| CliError::Format(format!("serializing the comparison: {err}")))?;
        report::out(&json);
    } else {
        for line in diff_lines(&comparison) {
            report::out(&line);
        }
    }
    if let Some(min) = args.min_psnr
        && !comparison.meets(min)
    {
        reporter.status(&format!("PSNR is below the --min-psnr {min} dB threshold"));
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

/// The text report for `webp diff`: dimensions, PSNR, and the max channel delta.
fn diff_lines(comparison: &crate::diff::Comparison) -> Vec<String> {
    let psnr = comparison.psnr.map_or_else(
        || "identical (no difference)".to_owned(),
        |value| format!("{value:.2} dB"),
    );
    vec![
        format!("Dimensions: {}x{}", comparison.width, comparison.height),
        format!("PSNR:       {psnr}"),
        format!("Max delta:  {} / 255", comparison.max_delta),
    ]
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
    // `--optimize` sweeps lossless effort, so an explicit lossy request contradicts
    // it. Caught here, once, rather than silently dropping `--optimize` per file.
    if args.optimize && (args.lossy || args.quality.is_some()) {
        return Err(CliError::Usage(
            "`--optimize` sweeps lossless effort; drop `--lossy`/`--quality`, or drop `--optimize`"
                .to_owned(),
        ));
    }
    let settings = encode_settings(
        args.lossless,
        args.lossy,
        args.quality,
        args.method,
        &args.metadata,
    )?;
    let options = bulk::Options {
        flags: flags_of(&settings, false),
        metadata: settings.metadata.value,
        optimize: args.optimize,
        recursive: args.recursive,
        output_dir: args.output.clone(),
        force: args.force,
        no_clobber: args.no_clobber,
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
        "converted {} file(s){}, {} -> {} bytes{}",
        outcome.converted,
        if outcome.skipped > 0 {
            format!(", {} skipped", outcome.skipped)
        } else {
            String::new()
        },
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

/// Input file extensions the bare form considers when walking a directory. Naming
/// a file directly bypasses this filter, so an odd extension still works.
const CONVERTIBLE_EXTENSIONS: &[&str] = &[
    "png", "ppm", "pam", "jpg", "jpeg", "gif", "tif", "tiff", "bmp", "raw", "rgba", "webp",
];

fn is_convertible(path: &Path) -> bool {
    io::extension_of(path).is_some_and(|ext| CONVERTIBLE_EXTENSIONS.contains(&ext.as_str()))
}

/// The bare, direction-detected form: `webp <inputs...>`.
///
/// Each input is sniffed independently — a WebP is decoded to PNG, anything else
/// is encoded to WebP — so a mixed batch is coherent. Derived outputs are guarded
/// against overwrite (§ overwrite policy); an explicitly named single `-o FILE`
/// overwrites, as naming it is intent.
fn auto(args: &AutoArgs, reporter: &Reporter) -> Result<(), CliError> {
    if args.inputs.is_empty() {
        return Err(CliError::Usage(
            "no input given (try `webp photo.png`, or `webp --help`)".to_owned(),
        ));
    }
    let files = io::collect_files(&args.inputs, args.recursive, &is_convertible)?;
    if files.is_empty() {
        return Err(CliError::Usage(
            "no convertible images found in the given inputs".to_owned(),
        ));
    }
    let out_is_dir = args.output.as_deref().is_some_and(io::is_dir);
    if files.len() > 1 && args.output.is_some() && !out_is_dir {
        return Err(CliError::Usage(
            "with more than one input, `-o` must be a directory".to_owned(),
        ));
    }
    // A single input with an explicit non-directory `-o` names its output exactly;
    // every other shape derives the name (and is overwrite-guarded).
    let explicit = (files.len() == 1 && args.output.is_some() && !out_is_dir)
        .then(|| args.output.clone())
        .flatten();
    for file in &files {
        auto_one(file, args, explicit.as_deref(), reporter)?;
    }
    Ok(())
}

/// Convert one file in the bare form, honoring the overwrite guard.
fn auto_one(
    input: &Path,
    args: &AutoArgs,
    explicit: Option<&Path>,
    reporter: &Reporter,
) -> Result<(), CliError> {
    let bytes = Source::File(input.to_path_buf()).read()?;
    let ext = if codec::is_webp(&bytes) {
        // Decode direction: honor an explicit output's extension, else PNG.
        explicit
            .and_then(io::extension_of)
            .unwrap_or_else(|| "png".to_owned())
    } else {
        "webp".to_owned()
    };
    let (output, derived) = explicit.map_or_else(
        || {
            let stem = input.file_stem().unwrap_or(input.as_os_str());
            let dir = args
                .output
                .clone()
                .or_else(|| input.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            (dir.join(stem).with_extension(&ext), true)
        },
        |path| (path.to_path_buf(), false),
    );
    if reporter.is_dry_run() {
        let direction = if codec::is_webp(&bytes) {
            "decode"
        } else {
            "encode"
        };
        let exists = if io::exists(&output) {
            " (overwrites existing)"
        } else {
            ""
        };
        report::plan(&format!(
            "{direction} {} -> {}{exists}",
            input.display(),
            output.display(),
        ));
        return Ok(());
    }
    if !clear_to_write(&output, derived, args, reporter)? {
        return Ok(());
    }

    if codec::is_webp(&bytes) {
        auto_decode(input, &bytes, &output, args, reporter)
    } else {
        auto_encode(input, &bytes, &output, args, reporter)
    }
}

/// Resolve the overwrite guard for one output. Returns `false` when the file
/// should be skipped (`--no-clobber`), `Err` when it is refused.
fn clear_to_write(
    output: &Path,
    derived: bool,
    args: &AutoArgs,
    reporter: &Reporter,
) -> Result<bool, CliError> {
    if !io::exists(output) || args.force {
        return Ok(true);
    }
    if args.no_clobber {
        reporter.detail(&format!("skipping {} (exists)", output.display()));
        return Ok(false);
    }
    if derived {
        return Err(CliError::Clobber(output.display().to_string()));
    }
    // An explicitly named output overwrites, as naming it is the intent.
    Ok(true)
}

/// Encode one non-WebP input to a WebP output, reporting the chosen codec.
///
/// A crop or resize forces the still path (a preprocessed single image); without
/// one, a GIF still becomes an animation.
fn auto_encode(
    input: &Path,
    bytes: &[u8],
    output: &Path,
    args: &AutoArgs,
    reporter: &Reporter,
) -> Result<(), CliError> {
    let format = InputFormat::resolve(None, io::extension_of(input).as_deref(), bytes);
    let settings = encode_settings(
        args.lossless,
        args.lossy,
        args.quality,
        args.method,
        &args.metadata,
    )?;
    let flags = flags_of(&settings, false);
    let (mode, derived) = codec::resolve_mode(format, flags)?;
    let selection = settings.metadata.value;
    let pipeline = pipeline_of(args.crop.as_deref(), args.resize.as_deref())?;

    let produced = if pipeline.is_empty() {
        let encoded = codec::encode_input(bytes, format, mode, selection, true)?;
        if encoded.animation && (flags.lossy || flags.quality.is_some()) {
            report::warn("a GIF becomes a lossless animation; --lossy/--quality do not apply");
        }
        Produced {
            bytes: encoded.bytes,
            width: encoded.width,
            height: encoded.height,
            note: codec_note(mode, format, encoded.animation),
            search: None,
        }
    } else {
        if let Some(dims) = format::dimensions_of(bytes, format) {
            pipeline.project(dims)?;
        }
        let image = pipeline.apply(format::read_image(bytes, format, None)?)?;
        let strategy = Strategy::resolve(mode, derived, false, None)?;
        run_still(strategy, &image, selection, format)?
    };
    Sink::File(output.to_path_buf()).write(&produced.bytes)?;
    reporter.status(&format!(
        "{} -> {} ({}x{}, {} bytes, {}, {})",
        input.display(),
        output.display(),
        produced.width,
        produced.height,
        produced.bytes.len(),
        ratio(bytes.len(), produced.bytes.len()),
        produced.note,
    ));
    Ok(())
}

/// Decode one WebP input to an image output (first frame if animated), applying
/// any `--crop`/`--resize` to the decoded pixels before writing.
fn auto_decode(
    input: &Path,
    bytes: &[u8],
    output: &Path,
    args: &AutoArgs,
    reporter: &Reporter,
) -> Result<(), CliError> {
    let format = OutputFormat::resolve(None, io::extension_of(output).as_deref());
    let pipeline = pipeline_of(args.crop.as_deref(), args.resize.as_deref())?;
    let image = pipeline.apply(codec::decode_still_or_first_frame(
        bytes,
        resolved_max_pixels(),
    )?)?;
    let metadata = Selection::from_fields(&args.metadata).apply(image.metadata());
    let out = format::write_image(&image, format, &metadata)?;
    Sink::File(output.to_path_buf()).write(&out)?;
    reporter.status(&format!(
        "{} -> {} ({}x{}, {} bytes, from WebP)",
        input.display(),
        output.display(),
        image.width(),
        image.height(),
        out.len(),
    ));
    Ok(())
}

fn decode(args: &DecodeArgs, reporter: &Reporter) -> Result<(), CliError> {
    let source = Source::from_arg(&args.input);
    let sink = Sink::from_arg(&args.output);
    let bytes = source.read()?;
    let format = OutputFormat::resolve(args.format, sink.extension().as_deref());
    if reporter.is_dry_run() {
        report::plan(&format!(
            "{} -> {} ({format:?})",
            source.label(),
            sink.label(),
        ));
        return Ok(());
    }
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
    let cap = resolved_max_pixels();
    let options = codec::decode_options(layout, cap);
    if is_animated(&bytes)? {
        return decode_animation(args, &bytes, format, &options, &sink, reporter);
    }
    let image = codec::decode(&bytes, layout, cap)?;
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

    let target = target_of(args.target_size.as_deref(), args.min_psnr)?;
    // A size/PSNR target searches lossy quality, so it selects lossy the same way
    // `--lossy` does — combined with `--lossless` it is a clean conflict, not a
    // silent no-op.
    let settings = encode_settings(
        args.lossless,
        args.lossy,
        args.quality,
        args.method,
        &args.metadata,
    )?;
    let flags = flags_of(&settings, target.is_some());
    let (mode, derived) = codec::resolve_mode(format, flags)?;
    if reporter.is_dry_run() {
        report::plan(&format!(
            "{} -> {} ({})",
            source.label(),
            sink.label(),
            codec_note(mode, format, false),
        ));
        return Ok(());
    }
    let selection = settings.metadata.value;
    let pipeline = pipeline_of(args.crop.as_deref(), args.resize.as_deref())?;
    let strategy = Strategy::resolve(mode, derived, false, target)?;

    let raw = match (args.width, args.height) {
        (Some(width), Some(height)) => Some(RawParams {
            width,
            height,
            layout: args.layout.into(),
        }),
        _ => None,
    };
    // A GIF becomes an animation only when nothing forces the still path — a crop,
    // a resize, or a size target all operate on a single still image. Raw pixels
    // carry no container and always take the still path with explicit dimensions.
    let still = raw.is_some() || !pipeline.is_empty() || target.is_some();
    let encoded = if let Some(params) = raw {
        let image = pipeline.apply(format::read_image(&bytes, format, Some(params))?)?;
        run_still(strategy, &image, selection, format)?
    } else if still {
        if !pipeline.is_empty()
            && let Some(dims) = format::dimensions_of(&bytes, format)
        {
            pipeline.project(dims)?;
        }
        let image = pipeline.apply(format::read_image(&bytes, format, None)?)?;
        run_still(strategy, &image, selection, format)?
    } else {
        let e = codec::encode_input(&bytes, format, mode, selection, true)?;
        Produced {
            bytes: e.bytes,
            width: e.width,
            height: e.height,
            note: codec_note(mode, format, e.animation),
            search: None,
        }
    };
    sink.write(&encoded.bytes)?;
    if let Some(search) = &encoded.search {
        reporter.detail(&format!("search: {search}"));
    }
    reporter.status(&format!(
        "encoded {} -> {} ({}x{}, {} bytes, {}, {})",
        source.label(),
        sink.label(),
        encoded.width,
        encoded.height,
        encoded.bytes.len(),
        ratio(bytes.len(), encoded.bytes.len()),
        encoded.note,
    ));
    Ok(())
}

/// One encoded still: the bytes, its dimensions, the status-line codec note, and a
/// search narration when a target was hunted.
struct Produced {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    note: String,
    search: Option<String>,
}

/// Encode a decoded still through a [`Strategy`], selecting `selection`'s metadata.
fn run_still(
    strategy: Strategy,
    image: &webpkit::Image,
    selection: Selection,
    format: InputFormat,
) -> Result<Produced, CliError> {
    let metadata = selection.apply(image.metadata());
    let report = strategy.run(image, &metadata)?;
    Ok(Produced {
        width: image.width(),
        height: image.height(),
        note: codec_note(report.mode, format, false),
        search: report.search_line(),
        bytes: report.bytes,
    })
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::{ArgAction, CommandFactory as _};

    use super::{Cli, SUBCOMMANDS};

    /// `SUBCOMMANDS` (the first-positional dispatch list) must name exactly the
    /// clap subcommands plus the built-in `help`. Add a `Command` variant without
    /// listing it and the new command would silently route through the bare form;
    /// this fails instead.
    #[test]
    fn subcommands_list_matches_the_command_enum() {
        let mut expected: BTreeSet<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect();
        expected.insert("help".to_owned());
        let listed: BTreeSet<String> = SUBCOMMANDS.iter().map(|&s| s.to_owned()).collect();
        assert_eq!(
            listed, expected,
            "SUBCOMMANDS drifted from the Command enum"
        );
    }

    /// `first_positional_is_subcommand` skips the value-taking global flags by name
    /// (`--color`, `--threads`). Add a value-taking global and forget the scan and a
    /// `webp --new V encode` misparses; this pins the set so the scan is updated too.
    #[test]
    fn value_taking_globals_are_exactly_color_and_threads() {
        let value_globals: BTreeSet<String> = Cli::command()
            .get_arguments()
            .filter(|a| a.is_global_set())
            .filter(|a| matches!(a.get_action(), ArgAction::Set | ArgAction::Append))
            .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
            .collect();
        let expected: BTreeSet<String> = ["--color", "--threads"]
            .iter()
            .map(|&s| s.to_owned())
            .collect();
        assert_eq!(
            value_globals, expected,
            "update the takes_value scan in first_positional_is_subcommand"
        );
    }
}
