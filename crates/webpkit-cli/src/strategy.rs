//! Encoding as a *strategy*, not a single call.
//!
//! One image can be encoded three ways: once, as a lossless effort sweep that
//! keeps the smallest output (`--optimize`), or as a bisection over lossy quality
//! that hunts a byte / PSNR target (`-size` / `--target-size`, `-psnr`). Modeling
//! the choice as a [`Strategy`] gives two things a bare `encode()` call cannot.
//!
//! It makes a multi-attempt encode describable: [`EncodeReport`] carries every
//! attempt so verbose output can narrate the search (`q=75 -> 412KB; ...`) rather
//! than pretend one number appeared from nowhere.
//!
//! And it makes the old `--optimize --lossy` silent drop *unrepresentable*:
//! [`Strategy::resolve`] is the one place flags become a strategy, and it turns the
//! contradiction into a usage error instead of quietly ignoring `--optimize`.

use std::collections::BTreeMap;

use webpkit::{Effort, Image, Metadata, PixelLayout};

use crate::{
    codec::{self, EncodeMode},
    diff,
    error::CliError,
    format,
};

/// A target for the lossy quality search: a byte budget, a PSNR floor, or both.
///
/// At least one field is set — a target with neither is not a target — and
/// [`from_flags`](Self::from_flags) enforces that.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target {
    /// Largest acceptable output size in bytes.
    max_bytes: Option<u64>,
    /// Smallest acceptable reconstruction PSNR in dB (vs the source).
    min_psnr: Option<f64>,
}

impl Target {
    /// A target from the `-size` / `-psnr` flags, or `None` when neither is set.
    #[must_use]
    pub(crate) fn from_flags(max_bytes: Option<u64>, min_psnr: Option<f64>) -> Option<Self> {
        (max_bytes.is_some() || min_psnr.is_some()).then_some(Self {
            max_bytes,
            min_psnr,
        })
    }
}

/// How to turn one image into WebP bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Strategy {
    /// A single encode with a fixed mode.
    Once(EncodeMode),
    /// Sweep the three lossless efforts and keep the smallest output.
    OptimizeLossless,
    /// Bisect lossy quality to meet a [`Target`], at a fixed effort.
    Search {
        /// Encoder effort held constant across the quality search.
        effort: Effort,
        /// The byte / PSNR target being hunted.
        target: Target,
    },
}

impl Strategy {
    /// Resolve flags into a strategy, rejecting the contradictions a single
    /// `encode()` used to swallow.
    ///
    /// `mode` is the resolved codec, `derived` whether it came from the source
    /// format (as opposed to an explicit `--lossy`/`--quality`). `--optimize`
    /// sweeps lossless effort, so it cannot combine with an explicit lossy request
    /// or a size target; a size/PSNR target searches lossy quality, so it cannot
    /// apply to lossless output.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] for a contradictory combination.
    pub(crate) fn resolve(
        mode: EncodeMode,
        derived: bool,
        optimize: bool,
        target: Option<Target>,
    ) -> Result<Self, CliError> {
        let explicit_lossy = matches!(mode, EncodeMode::Lossy { .. }) && !derived;
        match (optimize, target) {
            (true, Some(_)) => Err(CliError::Usage(
                "`--optimize` sweeps lossless effort and a size/quality target searches lossy \
                 quality; they cannot combine — choose one"
                    .to_owned(),
            )),
            (true, None) => match mode {
                EncodeMode::Lossless(_) => Ok(Self::OptimizeLossless),
                EncodeMode::Lossy { .. } if explicit_lossy => Err(CliError::Usage(
                    "`--optimize` sweeps lossless effort; drop `--lossy`/`--quality`, or drop \
                     `--optimize`"
                        .to_owned(),
                )),
                // A source-derived lossy input (a JPEG) has nothing to sweep losslessly;
                // it encodes once. The user did not ask for lossy, so this is no drop.
                EncodeMode::Lossy { .. } => Ok(Self::Once(mode)),
            },
            (false, Some(target)) => match mode {
                EncodeMode::Lossy { method, .. } => Ok(Self::Search {
                    effort: method,
                    target,
                }),
                EncodeMode::Lossless(_) => Err(CliError::Usage(
                    "a size/quality target searches lossy quality; lossless output has no quality \
                     dial — remove the lossless request"
                        .to_owned(),
                )),
            },
            (false, None) => Ok(Self::Once(mode)),
        }
    }

    /// Run the strategy over a decoded image, returning the bytes and a report of
    /// every attempt.
    ///
    /// # Errors
    ///
    /// [`CliError::Codec`] if the encoder rejects the image.
    pub(crate) fn run(self, image: &Image, metadata: &Metadata) -> Result<EncodeReport, CliError> {
        match self {
            Self::Once(mode) => {
                let bytes = codec::encode(image, mode, metadata.clone())?;
                Ok(EncodeReport::single(mode, bytes))
            },
            Self::OptimizeLossless => optimize_lossless(image, metadata),
            Self::Search { effort, target } => Searcher::new(image, metadata, effort).run(target),
        }
    }
}

/// The outcome of running a [`Strategy`]: the bytes, the effective mode for the
/// status line, and the attempts a search made.
pub(crate) struct EncodeReport {
    /// The chosen WebP file.
    pub(crate) bytes: Vec<u8>,
    /// The mode that produced [`Self::bytes`] — a search reports its chosen quality.
    pub(crate) mode: EncodeMode,
    /// Each quality the search tried, in probe order (empty for a single encode).
    pub(crate) attempts: Vec<Attempt>,
    /// The quality a search settled on, or `None` for a non-search.
    pub(crate) chosen_quality: Option<u8>,
    /// Whether a search actually met its target (`true` for a non-search).
    pub(crate) met: bool,
}

impl EncodeReport {
    /// A report for a single, unconditional encode.
    const fn single(mode: EncodeMode, bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            mode,
            attempts: Vec::new(),
            chosen_quality: None,
            met: true,
        }
    }

    /// A one-line narration of a search's attempts, e.g.
    /// `q=75 -> 412KB; q=52 -> 231KB; q=44 -> 198KB * (3 attempts)`, or `None`
    /// when there was no search.
    #[must_use]
    pub(crate) fn search_line(&self) -> Option<String> {
        if self.attempts.is_empty() {
            return None;
        }
        let steps = self
            .attempts
            .iter()
            .map(|a| {
                let mark = if Some(a.quality) == self.chosen_quality {
                    " *"
                } else {
                    ""
                };
                format!("q={} -> {}{mark}", a.quality, human_size(a.bytes))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let n = self.attempts.len();
        let met = if self.met { "" } else { " (target not met)" };
        Some(format!(
            "{steps} ({n} attempt{}){met}",
            if n == 1 { "" } else { "s" }
        ))
    }
}

/// One encode the search performed.
pub(crate) struct Attempt {
    /// The lossy quality tried.
    pub(crate) quality: u8,
    /// The resulting file size in bytes.
    pub(crate) bytes: usize,
}

/// Sweep the three lossless efforts, keeping the smallest output.
fn optimize_lossless(image: &Image, metadata: &Metadata) -> Result<EncodeReport, CliError> {
    let mut best_effort = Effort::Fast;
    let mut bytes = codec::encode(image, EncodeMode::Lossless(Effort::Fast), metadata.clone())?;
    for effort in [Effort::Balanced, Effort::Best] {
        let candidate = codec::encode(image, EncodeMode::Lossless(effort), metadata.clone())?;
        if candidate.len() < bytes.len() {
            bytes = candidate;
            best_effort = effort;
        }
    }
    Ok(EncodeReport::single(
        EncodeMode::Lossless(best_effort),
        bytes,
    ))
}

/// State for a lossy quality bisection: memoized encodes and recorded attempts.
struct Searcher<'a> {
    image: &'a Image,
    metadata: &'a Metadata,
    effort: Effort,
    /// The source pixels, computed lazily only for a PSNR target.
    source_rgba: Option<Vec<u8>>,
    /// Encoded bytes per quality, so a bisection never re-encodes a quality.
    cache: BTreeMap<u8, Vec<u8>>,
    attempts: Vec<Attempt>,
}

impl<'a> Searcher<'a> {
    const fn new(image: &'a Image, metadata: &'a Metadata, effort: Effort) -> Self {
        Self {
            image,
            metadata,
            effort,
            source_rgba: None,
            cache: BTreeMap::new(),
            attempts: Vec::new(),
        }
    }

    /// Encoded size at `quality`, encoding and recording it on first request.
    fn size_at(&mut self, quality: u8) -> Result<u64, CliError> {
        if let Some(bytes) = self.cache.get(&quality) {
            return Ok(bytes.len() as u64);
        }
        let bytes = codec::encode(
            self.image,
            EncodeMode::Lossy {
                quality,
                method: self.effort,
            },
            self.metadata.clone(),
        )?;
        let len = bytes.len();
        self.attempts.push(Attempt {
            quality,
            bytes: len,
        });
        self.cache.insert(quality, bytes);
        Ok(len as u64)
    }

    /// Reconstruction PSNR at `quality` (vs the source); `f64::INFINITY` when the
    /// re-decode is byte-identical to the source.
    fn psnr_at(&mut self, quality: u8) -> Result<f64, CliError> {
        self.size_at(quality)?;
        let bytes = self
            .cache
            .get(&quality)
            .cloned()
            .ok_or_else(|| CliError::Format("target search lost an encode".to_owned()))?;
        if self.source_rgba.is_none() {
            self.source_rgba = Some(format::to_rgba8(self.image));
        }
        let decoded = codec::decode(&bytes, PixelLayout::Rgba8, None)?;
        let candidate = format::to_rgba8(&decoded);
        let source = self.source_rgba.as_deref().unwrap_or(&candidate);
        Ok(diff::psnr_rgb(source, &candidate).unwrap_or(f64::INFINITY))
    }

    /// Bisect quality for the target and produce the report.
    fn run(mut self, target: Target) -> Result<EncodeReport, CliError> {
        // Largest quality within the byte budget (size rises with quality).
        let q_size = match target.max_bytes {
            Some(max) => Some(last_true(0, 100, |q| Ok(self.size_at(q)? <= max))?),
            None => None,
        };
        // Smallest quality meeting the PSNR floor (PSNR rises with quality).
        let q_psnr = match target.min_psnr {
            Some(floor) => Some(first_true(0, 100, |q| Ok(self.psnr_at(q)? >= floor))?),
            None => None,
        };
        // The floor wins over the budget: at least `q_psnr`, at most `q_size` when
        // compatible. `max` yields exactly that.
        let chosen = [q_size, q_psnr].into_iter().flatten().max().unwrap_or(75);

        let met = target
            .max_bytes
            .is_none_or(|max| self.size_at(chosen).is_ok_and(|s| s <= max))
            && target
                .min_psnr
                .is_none_or(|floor| self.psnr_at(chosen).is_ok_and(|p| p >= floor));

        let bytes = self
            .cache
            .get(&chosen)
            .cloned()
            .ok_or_else(|| CliError::Format("target search produced no encode".to_owned()))?;
        Ok(EncodeReport {
            bytes,
            mode: EncodeMode::Lossy {
                quality: chosen,
                method: self.effort,
            },
            attempts: self.attempts,
            chosen_quality: Some(chosen),
            met,
        })
    }
}

/// Largest `q` in `lo..=hi` for which `pred(q)` holds, assuming `pred` is true on
/// an initial run of low values and false thereafter. Returns `lo` if none hold.
fn last_true(
    lo: u8,
    hi: u8,
    mut pred: impl FnMut(u8) -> Result<bool, CliError>,
) -> Result<u8, CliError> {
    let mut best: Option<u8> = None;
    let (mut a, mut b) = (i16::from(lo), i16::from(hi));
    while a <= b {
        let mid = u8::try_from(i16::midpoint(a, b)).unwrap_or(lo);
        if pred(mid)? {
            best = Some(mid);
            a = i16::from(mid) + 1;
        } else {
            b = i16::from(mid) - 1;
        }
    }
    Ok(best.unwrap_or(lo))
}

/// Smallest `q` in `lo..=hi` for which `pred(q)` holds, assuming `pred` is false on
/// an initial run of low values and true thereafter. Returns `hi` if none hold.
fn first_true(
    lo: u8,
    hi: u8,
    mut pred: impl FnMut(u8) -> Result<bool, CliError>,
) -> Result<u8, CliError> {
    let mut best: Option<u8> = None;
    let (mut a, mut b) = (i16::from(lo), i16::from(hi));
    while a <= b {
        let mid = u8::try_from(i16::midpoint(a, b)).unwrap_or(lo);
        if pred(mid)? {
            best = Some(mid);
            b = i16::from(mid) - 1;
        } else {
            a = i16::from(mid) + 1;
        }
    }
    Ok(best.unwrap_or(hi))
}

/// A byte count as a short human string: `412KB`, `1.5MB`, `900B`. Integer math so
/// no lossy float cast is needed.
fn human_size(bytes: usize) -> String {
    let b = u64::try_from(bytes).unwrap_or(u64::MAX);
    if b >= 1 << 20 {
        // Tenths of a MB, rounded.
        let tenths = (b * 10 + (1 << 19)) >> 20;
        format!("{}.{}MB", tenths / 10, tenths % 10)
    } else if b >= 1 << 10 {
        format!("{}KB", (b + (1 << 9)) >> 10)
    } else {
        format!("{b}B")
    }
}

#[cfg(test)]
mod tests {
    use super::{first_true, last_true};

    /// A synthetic monotone size(q) = q*q so the bisection is checked without any
    /// encoder in the loop — the search logic bites on its own.
    #[test]
    fn last_true_finds_the_largest_passing_quality() {
        // size(q) = q; budget 50 -> largest q with q <= 50 is 50.
        let q = last_true(0, 100, |q| Ok(u64::from(q) <= 50)).unwrap();
        assert_eq!(q, 50);
    }

    #[test]
    fn last_true_falls_back_to_lo_when_none_pass() {
        // Nothing is under budget: return the smallest quality (best effort).
        let q = last_true(0, 100, |_| Ok(false)).unwrap();
        assert_eq!(q, 0);
    }

    #[test]
    fn first_true_finds_the_smallest_passing_quality() {
        // psnr(q) >= floor first holds at q = 30.
        let q = first_true(0, 100, |q| Ok(q >= 30)).unwrap();
        assert_eq!(q, 30);
    }

    #[test]
    fn first_true_falls_back_to_hi_when_none_pass() {
        let q = first_true(0, 100, |_| Ok(false)).unwrap();
        assert_eq!(q, 100);
    }

    #[test]
    fn bisection_probes_a_logarithmic_number_of_qualities() {
        let mut count = 0;
        let _ = last_true(0, 100, |q| {
            count += 1;
            Ok(u64::from(q) <= 42)
        });
        // log2(101) ~ 7; certainly far fewer than a linear scan of 101.
        assert!(count <= 8, "bisection probed {count} qualities");
    }
}
