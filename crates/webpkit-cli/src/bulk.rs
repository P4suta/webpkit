//! Parallel bulk / directory conversion for `webp convert`.
//!
//! Files (and directories, optionally recursive) are encoded to WebP in
//! parallel with rayon. `--optimize` tries every effort level per file and
//! keeps the smallest output.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use webpkit::{Effort, Image, Metadata};

use crate::{
    codec::{self, CodecFlags, EncodeMode},
    error::CliError,
    format::{self, InputFormat},
    io::{self, Sink, Source},
    metadata::Selection,
};

/// Extensions treated as encodable image inputs when scanning directories.
const IMAGE_EXTENSIONS: [&str; 11] = [
    "png", "ppm", "pam", "jpg", "jpeg", "gif", "tif", "tiff", "bmp", "raw", "rgba",
];

/// Options for a bulk conversion run.
#[derive(Debug, Clone)]
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
    /// Per-file report lines paired with whether that file succeeded.
    pub(crate) lines: Vec<(bool, String)>,
}

/// Convert every input file (expanding directories) to WebP.
///
/// # Errors
///
/// [`CliError`] only for a failure that prevents the whole run (e.g. a
/// directory that cannot be read); per-file failures are recorded in the
/// returned [`Outcome`].
pub(crate) fn convert(inputs: &[PathBuf], options: &Options) -> Result<Outcome, CliError> {
    let files = io::collect_files(inputs, options.recursive, &is_image)?;
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
    let bytes = Source::File(path.to_path_buf()).read()?;
    let format = InputFormat::resolve(None, io::extension_of(path).as_deref(), &bytes);
    if format == InputFormat::Raw {
        return Err(CliError::Format(
            "raw input needs explicit dimensions; use `webp encode`".to_owned(),
        ));
    }
    let (mode, _derived) = codec::resolve_mode(format, options.flags)?;
    // `--optimize` searches the three lossless effort levels; a lossy request keeps
    // its own single (quality, effort) rather than an effort sweep. A GIF becomes a
    // lossless animation and is not part of the still effort search.
    let webp = if options.optimize
        && matches!(mode, EncodeMode::Lossless(_))
        && format != InputFormat::Gif
    {
        let image = format::read_image(&bytes, format, None)?;
        let metadata = options.metadata.apply(image.metadata());
        encode_smallest(&image, &metadata)?
    } else {
        codec::encode_input(&bytes, format, mode, options.metadata, true)?.bytes
    };
    let output = output_path(path, options.output_dir.as_deref());
    Sink::File(output.clone()).write(&webp)?;
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
