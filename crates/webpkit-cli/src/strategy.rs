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

use webpkit::{Effort, Image, LossyTuning, Metadata, RateTarget};

use crate::{
    codec::{self, EncodeMode},
    error::CliError,
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

    /// Project this CLI target onto the library's integer [`RateTarget`]: the byte
    /// budget narrows to `usize`, and the dB PSNR floor becomes fixed-point
    /// centidecibels (dB × 100, rounded) — the codec forbids floating point, so the
    /// conversion happens here, CLI-side.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "centidb is rounded and clamped into 0..=u32::MAX before the cast, so \
                  the narrowing to u32 is lossless"
    )]
    fn to_rate_target(self) -> RateTarget {
        let mut target = RateTarget::default();
        if let Some(max) = self.max_bytes {
            target = target.with_size(usize::try_from(max).unwrap_or(usize::MAX));
        }
        if let Some(db) = self.min_psnr {
            let centidb = (db * 100.0).round().clamp(0.0, f64::from(u32::MAX)) as u32;
            target = target.with_psnr(centidb);
        }
        target
    }
}

/// How to turn one image into WebP bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Strategy {
    /// A single encode with a fixed mode.
    Once(EncodeMode),
    /// Encode losslessly at the deepest effort level — provably the smallest output
    /// — carrying any near-lossless level through.
    OptimizeLossless {
        /// Near-lossless level applied to the encode, or `None` for plain lossless.
        near_lossless: Option<u8>,
    },
    /// Bisect lossy quality to meet a [`Target`], at a fixed effort and tuning.
    Search {
        /// Encoder effort held constant across the quality search.
        effort: Effort,
        /// Psychovisual tuning held constant across the quality search.
        tuning: LossyTuning,
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
                EncodeMode::Lossless { near_lossless, .. } => {
                    Ok(Self::OptimizeLossless { near_lossless })
                },
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
                EncodeMode::Lossy { method, tuning, .. } => Ok(Self::Search {
                    effort: method,
                    tuning,
                    target,
                }),
                EncodeMode::Lossless { .. } => Err(CliError::Usage(
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
            Self::OptimizeLossless { near_lossless } => {
                optimize_lossless(image, metadata, near_lossless)
            },
            Self::Search {
                effort,
                tuning,
                target,
            } => search(image, metadata, effort, tuning, target),
        }
    }
}

/// Delegate a `-size`/`-psnr` quality search to the codec-native rate control
/// ([`webpkit::Encoder::<Lossy>::rate_control`]), then map its result onto the CLI's
/// [`EncodeReport`]. The bisection lives in the library; this only carries the
/// resolved metadata onto the encode and narrates the probes.
///
/// The metadata is folded onto a copy of the image (the encoder then embeds exactly
/// it, no inheritance beyond it), so the searched encode matches the single-encode
/// path's `-metadata` handling byte-for-byte.
fn search(
    image: &Image,
    metadata: &Metadata,
    effort: Effort,
    tuning: LossyTuning,
    target: Target,
) -> Result<EncodeReport, CliError> {
    let image = image.clone().with_metadata(metadata.clone());
    let result = webpkit::Encoder::lossy()
        .effort(effort)
        .tuning(tuning)
        .rate_control(&image, target.to_rate_target())?;
    let chosen = result.quality();
    let met = result.met();
    let attempts = result
        .attempts()
        .iter()
        .map(|a| Attempt {
            quality: a.quality,
            bytes: a.bytes,
        })
        .collect();
    Ok(EncodeReport {
        bytes: result.into_bytes(),
        mode: EncodeMode::Lossy {
            quality: chosen,
            method: effort,
            tuning,
        },
        attempts,
        chosen_quality: Some(chosen),
        met,
    })
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

/// Encode at the deepest effort level, whose candidate set is a superset of every
/// lower level's and is ranked by real emitted bytes, so its output is provably the
/// smallest the lossless encoder can reach. Any `near_lossless` level is applied.
fn optimize_lossless(
    image: &Image,
    metadata: &Metadata,
    near_lossless: Option<u8>,
) -> Result<EncodeReport, CliError> {
    let mode = EncodeMode::Lossless {
        effort: Effort::level(9),
        near_lossless,
    };
    let bytes = codec::encode(image, mode, metadata.clone())?;
    Ok(EncodeReport::single(mode, bytes))
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
