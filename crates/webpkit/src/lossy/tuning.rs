//! [`LossyTuning`] — the psychovisual rate-distortion knobs for lossy encoding.
//!
//! These mirror libwebp's `cwebp` shaping controls. The four *active* knobs —
//! [`sns_strength`](LossyTuning::sns_strength), [`segments`](LossyTuning::segments),
//! [`filter_strength`](LossyTuning::filter_strength) and
//! [`filter_sharpness`](LossyTuning::filter_sharpness) — feed the shared perceptual
//! model (spatial-noise-shaping quantization, segment count, and the in-loop
//! deblocking filter). Every knob is validated at its setter into the range the
//! encoder relies on, so a frame encode never re-checks them.
//!
//! [`LossyTuning::default`] is the **auto / near-best** baseline, chosen to match
//! `cwebp`'s default shaping (`sns 50`, `filter 60`, `segments 4`) so a caller who
//! sets no knob gets libwebp-parity output rather than a weak default.
//!
//! The lossy-alpha knobs ([`alpha_q`](LossyTuning::alpha_q),
//! [`alpha_method`](LossyTuning::alpha_method) and
//! [`alpha_filter`](LossyTuning::alpha_filter)) are **active**: `alpha_q` drives the
//! level-quantization pre-pass (`100` = lossless, the default), while `alpha_method`
//! and `alpha_filter` bound the stored-plane search. Their defaults reproduce the
//! prior always-lossless, exhaustive-search behavior byte-for-byte.
//!
//! [`sharp_yuv`](LossyTuning::sharp_yuv) is **active**: when set it replaces the plain
//! 4:2:0 box chroma subsampling with libwebp `libsharpyuv`'s luminance-guided refinement
//! (see [`crate::lossy::sharp_yuv`]). It is **off by default**, so the default encode's
//! chroma — and every output byte — is unchanged; only `-sharp_yuv` opts in.
//!
//! The rate/RD knobs ([`exact`](LossyTuning::exact),
//! [`jpeg_like`](LossyTuning::jpeg_like) and
//! [`partition_limit`](LossyTuning::partition_limit)) are **active**, each with a neutral
//! default that leaves every output byte unchanged: `exact` defaults to `true`
//! (preserve the RGB under fully-transparent pixels, kinder than `cwebp`'s clearing);
//! `jpeg_like` defaults `false` and `partition_limit` defaults `0` (no rate cap). Turning
//! any of them off / neutral reproduces the default encode byte-for-byte; only an
//! explicit non-neutral value biases the base quantizer (a smaller, coarser file).
//!
//! A `cwebp`-style content [`Preset`] is **not** a stored knob but a bundle: [`Preset::tuning`]
//! expands it into a base [`LossyTuning`] (the libwebp `WebPConfigPreset` shaping values)
//! that a caller then overrides with explicit `with_*` setters.
//!
//! [`pass`](LossyTuning::pass) is **active**: it sets the number of entropy-refinement
//! passes (`1..=10`, libwebp's `StatLoop`). `1` (the default) is the single-pass,
//! byte-identical encode; a higher count re-plans the frame against the previous pass's
//! optimized coefficient probabilities so the trellis rate model and the encoded size
//! converge. It only bites on the proba-optimizing effort tiers (`Full`/`Best`); at the
//! `Fast` tier there is no probability stage, so any pass count is inert.

/// The largest psychovisual-strength percentage (`sns_strength`, `filter_strength`,
/// `alpha_q`, `partition_limit`).
const MAX_PERCENT: u8 = 100;
/// The number of macroblock segments VP8 can code (`1..=4`).
const MIN_SEGMENTS: u8 = 1;
/// The number of macroblock segments VP8 can code (`1..=4`).
const MAX_SEGMENTS: u8 = 4;
/// The largest in-loop filter sharpness level (`0..=7`).
const MAX_SHARPNESS: u8 = 7;
/// The fewest analysis passes (`1..=10`).
const MIN_PASS: u8 = 1;
/// The most analysis passes (`1..=10`).
const MAX_PASS: u8 = 10;

/// A `cwebp`-style content preset (libwebp `WebPPreset`). Not a stored knob — call
/// [`Preset::tuning`] to expand it into a base [`LossyTuning`] bundle that explicit
/// `with_*` setters then override.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Preset {
    /// The default preset — no content-specific shaping.
    #[default]
    Default,
    /// Digital picture, like portrait or inner shot.
    Picture,
    /// Outdoor photograph with natural lighting.
    Photo,
    /// Hand or line drawing with high-contrast details.
    Drawing,
    /// Small-sized colorful image.
    Icon,
    /// Text-like content.
    Text,
}

impl Preset {
    /// Expand this preset into a base [`LossyTuning`] — the libwebp `WebPConfigPreset`
    /// shaping values (spatial-noise-shaping strength, in-loop filter strength/sharpness,
    /// and segment count). [`Preset::Default`] returns [`LossyTuning::default`], so a
    /// default preset is byte-identical to no preset at all. A caller applies explicit
    /// knobs *after* this to override the bundle.
    #[must_use]
    pub const fn tuning(self) -> LossyTuning {
        let base = LossyTuning::DEFAULT;
        // libwebp `WebPConfigInit` preset table (`sns`, `filter_strength`,
        // `filter_sharpness`, `segments`); only the perceptual-shaping knobs differ.
        let (sns, strength, sharpness, segments) = match self {
            Self::Default => return base,
            Self::Picture => (80, 35, 4, 4),
            Self::Photo => (80, 30, 3, 4),
            Self::Drawing => (25, 10, 6, 4),
            Self::Icon => (25, 10, 0, 4),
            Self::Text => (0, 0, 0, 2),
        };
        LossyTuning {
            sns_strength: sns,
            filter_strength: strength,
            filter_sharpness: sharpness,
            segments,
            ..base
        }
    }
}

/// How a lossy frame's alpha channel is compressed, mapping libwebp `cwebp`'s
/// `-alpha_method` (`0` raw / `1` lossless).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum AlphaMethod {
    /// Store the alpha plane uncompressed (`-alpha_method 0`).
    None,
    /// Compress the alpha plane losslessly (the default, `-alpha_method 1`).
    #[default]
    Compressed,
}

/// The spatial-filter search for a lossy frame's alpha plane, mapping libwebp
/// `cwebp`'s `-alpha_filter` (`none` / `fast` / `best`).
///
/// The alpha plane is always stored losslessly, so this only bounds *how many*
/// spatial predictors the encoder trials before keeping the smallest. The default
/// is [`AlphaFilterMode::Best`] — the exhaustive search over all four predictors,
/// the near-best behavior a caller gets when no knob is set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum AlphaFilterMode {
    /// Do not spatially filter the plane (`none`): store it as-is.
    None,
    /// Trial a single predictor picked by a cheap residual estimate (`fast`).
    Fast,
    /// Trial every predictor and keep the smallest (`best`, the default).
    #[default]
    Best,
}

/// The psychovisual rate-distortion tuning knobs for a lossy encode.
///
/// Build from [`LossyTuning::default`] (the near-best auto baseline) and override with
/// the `with_*` setters, each of which validates its input into the encoder's range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a flat surface of independent cwebp-mirror boolean knobs (sharp_yuv, \
              smooth_segments, exact, jpeg_like); a state machine would obscure the \
              one-knob-per-field map callers rely on"
)]
pub struct LossyTuning {
    sns_strength: u8,
    segments: u8,
    filter_strength: u8,
    filter_sharpness: u8,
    // The active lossy-alpha knobs (level-quantization pre-pass + stored-plane search).
    alpha_q: u8,
    alpha_method: AlphaMethod,
    alpha_filter: AlphaFilterMode,
    // The active sharp-YUV (luminance-guided chroma) strategy; default off = box chroma.
    sharp_yuv: bool,
    // 3×3 majority-vote smoothing of the per-macroblock segment map (libwebp
    // `SmoothSegmentMap`); default off = the raw k-means map, byte-identical.
    smooth_segments: bool,
    // Per-frequency luma quant sharpening (libwebp `kFreqSharpening`); default off =
    // no bias, byte-identical. Preserves high-freq detail at a larger-file / lower-PSNR
    // cost, so it is off by default.
    freq_sharpen: bool,
    // Preserve the RGB under fully-transparent pixels (default `true`); `false` clears it.
    exact: bool,
    // Active RD/rate knobs, neutral by default (bias the base quantizer when set).
    jpeg_like: bool,
    partition_limit: u8,
    // Entropy-refinement pass count (`1..=10`, libwebp's `StatLoop`); `1` is byte-identical.
    pass: u8,
}

impl LossyTuning {
    /// The auto / near-best baseline, matching `cwebp`'s default shaping. Every knob at
    /// its neutral value, so an encode with this tuning is byte-identical to the
    /// pre-tuning encoder.
    pub(crate) const DEFAULT: Self = Self {
        sns_strength: 50,
        segments: MAX_SEGMENTS,
        filter_strength: 60,
        filter_sharpness: 0,
        alpha_q: MAX_PERCENT,
        alpha_method: AlphaMethod::Compressed,
        alpha_filter: AlphaFilterMode::Best,
        sharp_yuv: false,
        smooth_segments: false,
        freq_sharpen: false,
        exact: true,
        jpeg_like: false,
        partition_limit: 0,
        pass: MIN_PASS,
    };
}

impl Default for LossyTuning {
    /// The auto / near-best baseline, matching `cwebp`'s default shaping.
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl LossyTuning {
    /// The auto / near-best tuning baseline (same as [`LossyTuning::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the spatial-noise-shaping strength (`0..=100`, clamped): higher values move
    /// more bits toward visually sensitive (flat) regions. `0` disables shaping.
    #[must_use]
    pub const fn with_sns_strength(mut self, sns: u8) -> Self {
        self.sns_strength = clamp(sns, 0, MAX_PERCENT);
        self
    }

    /// Set the number of macroblock quantizer segments (`1..=4`, clamped). `1` codes a
    /// single segment (no segmentation header).
    #[must_use]
    pub const fn with_segments(mut self, segments: u8) -> Self {
        self.segments = clamp(segments, MIN_SEGMENTS, MAX_SEGMENTS);
        self
    }

    /// Set the in-loop deblocking-filter strength (`0..=100`, clamped). `0` disables
    /// the filter.
    #[must_use]
    pub const fn with_filter_strength(mut self, strength: u8) -> Self {
        self.filter_strength = clamp(strength, 0, MAX_PERCENT);
        self
    }

    /// Set the in-loop deblocking-filter sharpness (`0..=7`, clamped): higher values
    /// filter less near edges.
    #[must_use]
    pub const fn with_filter_sharpness(mut self, sharpness: u8) -> Self {
        self.filter_sharpness = clamp(sharpness, 0, MAX_SHARPNESS);
        self
    }

    /// Set the sharp-YUV (luminance-guided chroma) flag. `false` (the default) keeps the
    /// plain 4:2:0 box downsampling and byte-identical output; `true` refines the U/V planes
    /// with the gamma-correct, decoder-aware `sharp_yuv` subsampling.
    #[must_use]
    pub const fn with_sharp_yuv(mut self, sharp_yuv: bool) -> Self {
        self.sharp_yuv = sharp_yuv;
        self
    }

    /// Set the segment-map smoothing flag. `false` (the default) keeps the raw k-means
    /// per-macroblock segment map and byte-identical output; `true` applies a 3×3
    /// majority-vote smoothing pass (libwebp `SmoothSegmentMap`) that cleans isolated
    /// segment assignments before the ids are emitted. Only bites on the effort tiers
    /// that segment (`Full`/`Best`) and only when the content forms `>= 2` segments.
    #[must_use]
    pub const fn with_smooth_segments(mut self, smooth_segments: bool) -> Self {
        self.smooth_segments = smooth_segments;
        self
    }

    /// Set the alpha-plane quality (`0..=100`, clamped): the number of distinct alpha
    /// levels the level-quantization pre-pass keeps. `100` (the default) is lossless;
    /// lower values coarsen soft transparency for a smaller `ALPH` chunk.
    #[must_use]
    pub const fn with_alpha_q(mut self, alpha_q: u8) -> Self {
        self.alpha_q = clamp(alpha_q, 0, MAX_PERCENT);
        self
    }

    /// Set the alpha compression method: [`AlphaMethod::Compressed`] (the default)
    /// keeps the lossless VP8L candidate in the search; [`AlphaMethod::None`] stores
    /// the plane raw.
    #[must_use]
    pub const fn with_alpha_method(mut self, method: AlphaMethod) -> Self {
        self.alpha_method = method;
        self
    }

    /// Set the alpha spatial-filter search: [`AlphaFilterMode::Best`] (the default)
    /// trials every predictor, [`AlphaFilterMode::Fast`] a single estimated one, and
    /// [`AlphaFilterMode::None`] stores the plane unfiltered.
    #[must_use]
    pub const fn with_alpha_filter(mut self, filter: AlphaFilterMode) -> Self {
        self.alpha_filter = filter;
        self
    }

    /// Set the exact-transparency flag. `true` (the default) preserves the RGB stored
    /// under fully-transparent pixels; `false` clears it (RGB → 0) before the RGB→YUV
    /// conversion, letting those macroblocks flatten for a smaller file. The default
    /// (`true`) is byte-identical to the pre-`exact` encoder.
    #[must_use]
    pub const fn with_exact(mut self, exact: bool) -> Self {
        self.exact = exact;
        self
    }

    /// Set the number of entropy-refinement passes (`1..=10`, clamped; libwebp's
    /// `StatLoop`). `1` (the default) is the single-pass, byte-identical encode; a higher
    /// count re-plans the frame against each pass's optimized coefficient probabilities so
    /// the size converges. Only the proba-optimizing effort tiers act on it.
    #[must_use]
    pub const fn with_pass(mut self, pass: u8) -> Self {
        self.pass = clamp(pass, MIN_PASS, MAX_PASS);
        self
    }

    /// Set the JPEG-like rate-distortion bias flag. `false` (the default) is
    /// byte-identical; `true` steepens the quality falloff by coarsening the base
    /// quantizer, biasing bit allocation toward a JPEG-like size curve.
    #[must_use]
    pub const fn with_jpeg_like(mut self, jpeg_like: bool) -> Self {
        self.jpeg_like = jpeg_like;
        self
    }

    /// Set the first-partition rate cap (`0..=100`, clamped). `0` (the default) is no
    /// limit and byte-identical; a higher value coarsens the base quantizer to drop
    /// high-frequency coefficients and shrink the first (coefficient) partition.
    #[must_use]
    pub const fn with_partition_limit(mut self, limit: u8) -> Self {
        self.partition_limit = clamp(limit, 0, MAX_PERCENT);
        self
    }

    /// The spatial-noise-shaping strength (`0..=100`).
    #[must_use]
    pub const fn sns_strength(self) -> u8 {
        self.sns_strength
    }

    /// The number of macroblock quantizer segments (`1..=4`).
    #[must_use]
    pub const fn segments(self) -> u8 {
        self.segments
    }

    /// The in-loop deblocking-filter strength (`0..=100`).
    #[must_use]
    pub const fn filter_strength(self) -> u8 {
        self.filter_strength
    }

    /// The in-loop deblocking-filter sharpness (`0..=7`).
    #[must_use]
    pub const fn filter_sharpness(self) -> u8 {
        self.filter_sharpness
    }

    /// The sharp-YUV (luminance-guided chroma) flag; `false` keeps plain box chroma.
    #[must_use]
    pub const fn sharp_yuv(self) -> bool {
        self.sharp_yuv
    }

    /// The segment-map smoothing flag; `false` keeps the raw k-means map.
    #[must_use]
    pub const fn smooth_segments(self) -> bool {
        self.smooth_segments
    }

    /// Set the per-frequency luma sharpening flag (libwebp `kFreqSharpening`). `false`
    /// (the default) applies no bias and is byte-identical; `true` adds a per-frequency
    /// bias to luma AC coefficients before quantization, so high-frequency detail
    /// survives coarser quantization — a larger file and (usually) lower PSNR, so it is a
    /// detail-preserving opt-in, not a size/quality win.
    #[must_use]
    pub const fn with_freq_sharpen(mut self, freq_sharpen: bool) -> Self {
        self.freq_sharpen = freq_sharpen;
        self
    }

    /// The per-frequency luma sharpening flag; `false` applies no bias.
    #[must_use]
    pub const fn freq_sharpen(self) -> bool {
        self.freq_sharpen
    }

    /// The alpha-plane quality (`0..=100`); `100` keeps alpha lossless.
    #[must_use]
    pub const fn alpha_q(self) -> u8 {
        self.alpha_q
    }

    /// The alpha compression method.
    #[must_use]
    pub const fn alpha_method(self) -> AlphaMethod {
        self.alpha_method
    }

    /// The alpha spatial-filter search.
    #[must_use]
    pub const fn alpha_filter(self) -> AlphaFilterMode {
        self.alpha_filter
    }

    /// The exact-transparency flag; `true` preserves the RGB under transparent pixels.
    #[must_use]
    pub const fn exact(self) -> bool {
        self.exact
    }

    /// The number of entropy-refinement passes (`1..=10`; libwebp's `StatLoop`).
    #[must_use]
    pub const fn pass(self) -> u8 {
        self.pass
    }

    /// The JPEG-like rate-distortion bias flag.
    #[must_use]
    pub const fn jpeg_like(self) -> bool {
        self.jpeg_like
    }

    /// The first-partition rate cap (`0..=100`; `0` = no limit).
    #[must_use]
    pub const fn partition_limit(self) -> u8 {
        self.partition_limit
    }
}

/// Clamp `v` into `lo..=hi` in a `const` context (the standard [`Ord::clamp`] is not
/// `const`), so every setter can stay `const`.
const fn clamp(v: u8, lo: u8, hi: u8) -> u8 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::{AlphaFilterMode, AlphaMethod, LossyTuning, Preset};

    #[test]
    fn default_is_the_cwebp_parity_baseline() {
        let t = LossyTuning::default();
        assert_eq!(t.sns_strength(), 50);
        assert_eq!(t.filter_strength(), 60);
        assert_eq!(t.segments(), 4);
        assert_eq!(t.filter_sharpness(), 0);
        // The alpha defaults reproduce the prior always-lossless exhaustive search.
        assert_eq!(t.alpha_q(), 100);
        assert_eq!(t.alpha_method(), AlphaMethod::Compressed);
        assert_eq!(t.alpha_filter(), AlphaFilterMode::Best);
        // Neutral RD-knob defaults leave every output byte unchanged.
        assert!(!t.sharp_yuv());
        assert!(!t.smooth_segments());
        assert!(!t.freq_sharpen());
        assert!(t.exact(), "exact preserves hidden RGB by default");
        assert_eq!(t.pass(), 1);
        assert!(!t.jpeg_like());
        assert_eq!(t.partition_limit(), 0);
        assert_eq!(LossyTuning::new(), t, "new() equals default()");
    }

    #[test]
    fn preset_default_is_the_baseline_and_others_shape_it() {
        // The default preset expands to exactly the default tuning (byte-identical);
        // every other preset moves the perceptual-shaping knobs off the baseline.
        assert_eq!(Preset::Default.tuning(), LossyTuning::default());
        let photo = Preset::Photo.tuning();
        assert_eq!(photo.sns_strength(), 80);
        assert_eq!(photo.filter_strength(), 30);
        assert_eq!(photo.filter_sharpness(), 3);
        let text = Preset::Text.tuning();
        assert_eq!(text.sns_strength(), 0);
        assert_eq!(text.filter_strength(), 0);
        assert_eq!(text.segments(), 2);
        // A preset is a base: an explicit setter applied afterward wins.
        assert_eq!(
            Preset::Photo.tuning().with_sns_strength(10).sns_strength(),
            10
        );
    }

    #[test]
    fn active_setters_validate_their_ranges() {
        let t = LossyTuning::new()
            .with_sns_strength(250)
            .with_segments(9)
            .with_filter_strength(255)
            .with_filter_sharpness(40);
        assert_eq!(t.sns_strength(), 100, "sns clamps to 100");
        assert_eq!(t.segments(), 4, "segments clamps to 4");
        assert_eq!(t.filter_strength(), 100, "filter clamps to 100");
        assert_eq!(t.filter_sharpness(), 7, "sharpness clamps to 7");

        let low = LossyTuning::new().with_segments(0);
        assert_eq!(low.segments(), 1, "segments clamps up to 1");
    }

    #[test]
    fn alpha_setters_validate_and_round_trip() {
        let t = LossyTuning::new()
            .with_alpha_q(200)
            .with_alpha_method(AlphaMethod::None)
            .with_alpha_filter(AlphaFilterMode::Fast);
        assert_eq!(t.alpha_q(), 100, "alpha_q clamps to 100");
        assert_eq!(t.alpha_method(), AlphaMethod::None);
        assert_eq!(t.alpha_filter(), AlphaFilterMode::Fast);
    }

    #[test]
    fn rd_and_placeholder_setters_validate_and_round_trip() {
        let t = LossyTuning::new()
            .with_sharp_yuv(true)
            .with_smooth_segments(true)
            .with_freq_sharpen(true)
            .with_exact(false)
            .with_pass(50)
            .with_jpeg_like(true)
            .with_partition_limit(250);
        assert!(t.sharp_yuv());
        assert!(t.smooth_segments());
        assert!(t.freq_sharpen());
        assert!(!t.exact());
        assert_eq!(t.pass(), 10, "pass clamps to 10");
        assert!(t.jpeg_like());
        assert_eq!(t.partition_limit(), 100, "partition_limit clamps to 100");

        assert_eq!(
            LossyTuning::new().with_pass(0).pass(),
            1,
            "pass clamps up to 1"
        );
    }
}
