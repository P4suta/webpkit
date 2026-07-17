//! Parallel bulk / directory conversion for `webp convert`.
//!
//! Files (and directories, optionally recursive) are encoded to WebP in
//! parallel with rayon. `--optimize` tries every effort level per file and
//! keeps the smallest output.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::{
    codec::{self, CodecFlags},
    error::CliError,
    format::{self, InputFormat},
    io::{self, Sink, Source},
    metadata::Selection,
    strategy::Strategy,
};

/// Extensions treated as encodable image inputs when scanning directories.
const IMAGE_EXTENSIONS: [&str; 11] = [
    "png", "ppm", "pam", "jpg", "jpeg", "gif", "tif", "tiff", "bmp", "raw", "rgba",
];

/// Options for a bulk conversion run.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "conversion options: each bool is an independent switch, not modeled state"
)]
pub(crate) struct Options {
    /// The user's codec choice, resolved per file against its source format.
    pub(crate) flags: CodecFlags,
    /// Which metadata to carry through.
    pub(crate) metadata: Selection,
    /// Try every lossless effort level and keep the smallest output.
    pub(crate) optimize: bool,
    /// Recurse into subdirectories.
    pub(crate) recursive: bool,
    /// Output directory; when absent, each output sits beside its input.
    pub(crate) output_dir: Option<PathBuf>,
    /// Overwrite an existing output instead of refusing.
    pub(crate) force: bool,
    /// Skip an input whose output exists (rather than refusing).
    pub(crate) no_clobber: bool,
}

/// One result line and the aggregate totals from a bulk run.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Number of files converted successfully.
    pub(crate) converted: usize,
    /// Number of files that failed.
    pub(crate) failed: usize,
    /// Total input bytes across successful files.
    pub(crate) total_in: u64,
    /// Total output bytes across successful files.
    pub(crate) total_out: u64,
    /// Files skipped because their output already existed (`--no-clobber`).
    pub(crate) skipped: usize,
    /// Per-file report lines paired with whether that file succeeded.
    pub(crate) lines: Vec<(bool, String)>,
}

/// Convert every input file (expanding directories) to WebP, calling `on_progress`
/// with `(completed, total)` as each file finishes (from the rayon workers, so it
/// must be `Sync`) so the caller can render a live counter.
///
/// # Errors
///
/// [`CliError`] only for a failure that prevents the whole run (e.g. a
/// directory that cannot be read); per-file failures are recorded in the
/// returned [`Outcome`].
pub(crate) fn convert(
    inputs: &[PathBuf],
    options: &Options,
    on_progress: impl Fn(usize, usize) + Sync,
) -> Result<Outcome, CliError> {
    let files = io::collect_files(inputs, options.recursive, &is_image)?;
    let total = files.len();
    let done = AtomicUsize::new(0);
    let results: Vec<Result<Conversion, String>> = files
        .par_iter()
        .map(|path| {
            let result =
                convert_one(path, options).map_err(|err| format!("{}: {err}", path.display()));
            // Report completion order, not file order — the counter tracks progress,
            // not which file; `fetch_add` returns the prior value, so `+ 1` is this
            // file's 1-based position.
            on_progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
            result
        })
        .collect();

    let mut outcome = Outcome::default();
    for result in results {
        match result {
            Ok(Conversion::Written(stat)) => {
                outcome.converted += 1;
                outcome.total_in += stat.in_len;
                outcome.total_out += stat.out_len;
                outcome.lines.push((
                    true,
                    format!("{} ({} bytes)", stat.output.display(), stat.out_len),
                ));
            },
            Ok(Conversion::Skipped(output)) => {
                outcome.skipped += 1;
                outcome
                    .lines
                    .push((true, format!("skipping {} (exists)", output.display())));
            },
            Err(message) => {
                outcome.failed += 1;
                outcome.lines.push((false, message));
            },
        }
    }
    Ok(outcome)
}

struct Stat {
    output: PathBuf,
    in_len: u64,
    out_len: u64,
}

/// One input's disposition: written, or skipped because its output exists.
enum Conversion {
    Written(Stat),
    Skipped(PathBuf),
}

fn convert_one(path: &Path, options: &Options) -> Result<Conversion, CliError> {
    let output = output_path(path, options.output_dir.as_deref());
    if io::exists(&output) && !options.force {
        if options.no_clobber {
            return Ok(Conversion::Skipped(output));
        }
        return Err(CliError::Clobber(output.display().to_string()));
    }
    let bytes = Source::File(path.to_path_buf()).read()?;
    let format = InputFormat::resolve(None, io::extension_of(path).as_deref(), &bytes);
    if format == InputFormat::Raw {
        return Err(CliError::Format(
            "raw input needs explicit dimensions; use `webp encode`".to_owned(),
        ));
    }
    let (mode, derived) = codec::resolve_mode(format, options.flags)?;
    // A GIF becomes an animation honoring its resolved codec, outside any still
    // effort search. Every other input becomes a strategy: `--optimize` a lossless
    // effort sweep, else a single encode. Resolving through `Strategy` is what makes
    // `--optimize --lossy` a usage error rather than a silently dropped flag.
    let webp = if format == InputFormat::Gif {
        codec::encode_input(
            &bytes,
            format,
            mode,
            options.metadata,
            true,
            codec::AnimOptimize::default(),
        )?
        .bytes
    } else {
        let strategy = Strategy::resolve(mode, derived, options.optimize, None)?;
        let image = format::read_image(&bytes, format, None)?;
        let metadata = options.metadata.apply(image.metadata());
        strategy.run(&image, &metadata)?.bytes
    };
    Sink::File(output.clone()).write(&webp)?;
    Ok(Conversion::Written(Stat {
        output,
        in_len: bytes.len() as u64,
        out_len: webp.len() as u64,
    }))
}

fn is_image(path: &Path) -> bool {
    io::extension_of(path).is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

/// The `.webp` output path: `<stem>.webp` in the output dir, else beside input.
fn output_path(input: &Path, output_dir: Option<&Path>) -> PathBuf {
    let stem = input.file_stem().unwrap_or(input.as_os_str());
    let dir = output_dir
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    dir.join(stem).with_extension("webp")
}
