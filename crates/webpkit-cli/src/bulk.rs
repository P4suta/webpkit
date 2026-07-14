//! Parallel bulk / directory conversion for `webp convert`.
//!
//! Files (and directories, optionally recursive) are encoded to WebP in
//! parallel with rayon. `--optimize` tries every effort level per file and
//! keeps the smallest output.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use webpkit::lossless::{Effort, Image, Metadata};

use crate::{
    codec::{self, EncodeMode},
    error::CliError,
    format::{self, InputFormat},
    metadata::Selection,
};

/// Extensions treated as encodable image inputs when scanning directories.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "ppm", "pam", "raw", "rgba"];

/// Options for a bulk conversion run.
#[derive(Debug, Clone)]
pub struct Options {
    /// The codec and knobs (ignored for the lossless effort search under `optimize`).
    pub mode: EncodeMode,
    /// Which metadata to carry through.
    pub metadata: Selection,
    /// Try every lossless effort level and keep the smallest output.
    pub optimize: bool,
    /// Recurse into subdirectories.
    pub recursive: bool,
    /// Output directory; when absent, each output sits beside its input.
    pub output_dir: Option<PathBuf>,
}

/// One result line and the aggregate totals from a bulk run.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Number of files converted successfully.
    pub converted: usize,
    /// Number of files that failed.
    pub failed: usize,
    /// Total input bytes across successful files.
    pub total_in: u64,
    /// Total output bytes across successful files.
    pub total_out: u64,
    /// Per-file report lines paired with whether that file succeeded.
    pub lines: Vec<(bool, String)>,
}

/// Convert every input file (expanding directories) to WebP.
///
/// # Errors
///
/// [`CliError`] only for a failure that prevents the whole run (e.g. a
/// directory that cannot be read); per-file failures are recorded in the
/// returned [`Outcome`].
pub fn convert(inputs: &[PathBuf], options: &Options) -> Result<Outcome, CliError> {
    let files = collect_files(inputs, options.recursive)?;
    let results: Vec<Result<Stat, String>> = files
        .par_iter()
        .map(|path| convert_one(path, options).map_err(|err| format!("{}: {err}", path.display())))
        .collect();

    let mut outcome = Outcome::default();
    for result in results {
        match result {
            Ok(stat) => {
                outcome.converted += 1;
                outcome.total_in += stat.in_len;
                outcome.total_out += stat.out_len;
                outcome.lines.push((
                    true,
                    format!("{} ({} bytes)", stat.output.display(), stat.out_len),
                ));
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

fn convert_one(path: &Path, options: &Options) -> Result<Stat, CliError> {
    let bytes = std::fs::read(path)
        .map_err(|err| CliError::read_input(path.display().to_string(), &err))?;
    let format = InputFormat::resolve(None, extension_of(path).as_deref(), &bytes);
    if format == InputFormat::Raw {
        return Err(CliError::Format(
            "raw input needs explicit dimensions; use `webp encode`".to_owned(),
        ));
    }
    let image = format::read_image(&bytes, format, None)?;
    let metadata = options.metadata.apply(image.metadata());
    // `--optimize` searches the three lossless effort levels; a lossy request keeps
    // its own single (quality, effort) rather than an effort sweep.
    let webp = match (options.optimize, options.mode) {
        (true, EncodeMode::Lossless(_)) => encode_smallest(&image, &metadata)?,
        (_, mode) => codec::encode(&image, mode, metadata)?,
    };
    let output = output_path(path, options.output_dir.as_deref());
    std::fs::write(&output, &webp)
        .map_err(|err| CliError::write_output(output.display().to_string(), &err))?;
    Ok(Stat {
        output,
        in_len: bytes.len() as u64,
        out_len: webp.len() as u64,
    })
}

/// Encode with every lossless effort level and return the smallest output.
fn encode_smallest(image: &Image, metadata: &Metadata) -> Result<Vec<u8>, CliError> {
    let mut smallest: Option<Vec<u8>> = None;
    for method in [Effort::Fast, Effort::Balanced, Effort::Best] {
        let candidate = codec::encode(image, EncodeMode::Lossless(method), metadata.clone())?;
        if smallest
            .as_ref()
            .is_none_or(|best| candidate.len() < best.len())
        {
            smallest = Some(candidate);
        }
    }
    smallest.ok_or_else(|| CliError::Format("no encoder produced output".to_owned()))
}

/// Expand `inputs` (files and directories) into a flat list of files.
fn collect_files(inputs: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for input in inputs {
        if input.is_dir() {
            walk(input, recursive, &mut files)?;
        } else {
            files.push(input.clone());
        }
    }
    Ok(files)
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<(), CliError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|err| CliError::read_input(dir.display().to_string(), &err))?;
    for entry in entries {
        let entry = entry.map_err(|err| CliError::read_input(dir.display().to_string(), &err))?;
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                walk(&path, recursive, out)?;
            }
        } else if is_image(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_image(path: &Path) -> bool {
    extension_of(path).is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.as_str()))
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
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
