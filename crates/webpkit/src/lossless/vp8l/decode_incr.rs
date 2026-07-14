//! Two-phase suspend/resume VP8L decode core.
//!
//! [`Vp8lStream`] decodes a VP8L payload from *append-only* input pushes, without
//! buffering the whole stream up front: each [`Vp8lStream::advance`] is handed a
//! growing byte prefix and reports whether it made progress, needs more input, or
//! finished.
//!
//! # Why this is bit-exact against the one-shot [`crate::lossless::vp8l::decode::decode`]
//!
//! The design rests on two facts about the VP8L pixel loop:
//!
//! 1. **Each pixel unit reads all of its bits before mutating any state.**
//!    [`decode_one`] gates the whole unit on a single `is_eos` check, so a unit
//!    that runs past the buffered input leaves *nothing* changed — there is no
//!    partial state to roll back.
//! 2. **`is_eos` plus zero-padding gives a free third state.** After an attempt,
//!    `is_eos() == false` means the bits came entirely from real (buffered)
//!    bytes; because the payload is append-only, those bytes are identical to the
//!    ones a one-shot decode would see, so the unit decodes bit-for-bit the same.
//!
//! So a committed streamed unit is decoded from the same absolute bit offset
//! (`resume_bits`, chained through [`BitReader::new_at`], which equals
//! `new(..)` + `consume`) over byte-identical bytes by the same [`decode_one`] —
//! hence identical to one-shot. Suspended attempts mutate nothing. No reader
//! snapshot, no partial-copy state (the LZ77 copy loop reads no bits and never
//! suspends mid-copy), and no self-reference: the [`PixelCore`] *owns* its parsed
//! groups/entropy, so [`StreamState`] can persist across pushes with no borrow.
//!
//! The transform inverse runs **incrementally**: as each coded row completes,
//! [`StreamState`] flows it through an [`InverseChain`] — the same computation
//! graph as the whole-buffer [`apply_inverse_transforms`], evaluated row-major
//! instead of stage-major — appending finalized output rows to `ready`. The
//! accumulated `ready` is byte-identical to the batch inverse (each per-row
//! inverse fn is proven equal to its whole-buffer inverse). The coded buffer
//! ([`PixelCore::argb`], the LZ77 back-reference window) is never mutated by the
//! inverse: the chain works on copies.

use crate::lossless::bit_io::reader::BitReader;
use crate::lossless::prelude::*;
use crate::lossless::transform::{cross_color, palette, predictor, subtract_green};
use crate::lossless::vp8l::decode::{
    ParsedStream, PixelCore, Transform, decode_one, parse_top_level,
};
use crate::lossless::{Error, Result};

/// A suspend/resume VP8L decoder driven by append-only input pushes.
///
/// Feed growing byte prefixes to [`Vp8lStream::advance`]; once it reports
/// [`Step::Done`], [`Vp8lStream::into_pixels`] yields the final post-inverse ARGB.
pub(crate) struct Vp8lStream {
    state: Phase,
}

/// The decoder's lifecycle: parse the header, stream the pixels, then hold the
/// finished image.
enum Phase {
    /// Re-parsing the header on each push until it succeeds from real bytes.
    Parsing,
    /// Resuming the pixel loop across pushes. Boxed so the enum stays small.
    Streaming(Box<StreamState>),
    /// Finished — owns the final post-inverse ARGB pixels.
    Done(Vec<u32>),
}

/// Everything the streaming pixel loop must persist across input pushes.
struct StreamState {
    /// Coded ARGB window, `pos`, color cache, groups/entropy, working width — the
    /// whole per-unit decode state, owned (no borrow, so no self-reference).
    core: PixelCore,
    /// Absolute bit offset of the next unit to decode. Chains across pushes via
    /// [`BitReader::new_at`]; only advanced when a unit commits.
    resume_bits: u64,
    /// Coded (working-width) rows already handed to `inverse`. Every completed
    /// coded row is pushed through the incremental inverse exactly once.
    coded_rows_done: u32,
    /// The incremental transform inverse: flows each completed coded row through
    /// every stage in application order, accumulating finalized output rows.
    inverse: InverseChain,
    /// The coded/working row width (reduced by palette bundling). One coded row
    /// spans `reduced_width` entries of `core.argb`.
    reduced_width: usize,
}

/// The streaming inverse-transform pipeline: flows one *coded* (working-width)
/// row at a time through every transform inverse in application
/// order, appending one finalized ARGB output row to `ready`.
///
/// It evaluates the same computation graph as the whole-buffer
/// [`apply_inverse_transforms`] but **row-major** instead of stage-major — legal
/// because no inverse stage reads *forward* in rows. The predictor is the only
/// stage with a cross-row dependency (it reads its own previous output row), so
/// it alone keeps a one-row `prev_out`; every other stage is row-local.
struct InverseChain {
    /// Stages in application order (reverse of parse order), matching the batch
    /// [`apply_inverse_transforms`] loop.
    stages: Vec<Stage>,
    /// Count of coded rows pushed so far (== finalized output rows in `ready`).
    height_done: u32,
    /// Finalized output ARGB rows, row-major at the output (`dst_width`) width.
    ready: Vec<u32>,
}

/// One inverse stage. Only the predictor carries per-row state (`prev_out`); the
/// others are row-local. `width`/`dst_width` are the working widths the stage
/// operates on (an invariant checked against the incoming row in debug builds).
enum Stage {
    SubtractGreen,
    CrossColor {
        bits: u32,
        width: u32,
        data: Vec<u32>,
    },
    Predictor {
        bits: u32,
        width: u32,
        data: Vec<u32>,
        /// The previously reconstructed output row, seeding this row's
        /// top/top-left/top-right; empty until the first row is pushed.
        prev_out: Vec<u32>,
    },
    ColorIndexing {
        bits: u32,
        dst_width: u32,
        palette: Vec<u32>,
    },
}

impl InverseChain {
    /// Build the stage list from parsed transforms, in reverse parse order (==
    /// the batch [`apply_inverse_transforms`] application order). The palette's
    /// color map is already expanded in [`Transform::ColorIndexing`], so it is
    /// carried through verbatim.
    fn new(transforms: &[Transform]) -> Self {
        let stages = transforms
            .iter()
            .rev()
            .map(|transform| match transform {
                Transform::SubtractGreen => Stage::SubtractGreen,
                Transform::CrossColor { bits, width, data } => Stage::CrossColor {
                    bits: *bits,
                    width: *width,
                    data: data.clone(),
                },
                Transform::Predictor { bits, width, data } => Stage::Predictor {
                    bits: *bits,
                    width: *width,
                    data: data.clone(),
                    prev_out: Vec::new(),
                },
                Transform::ColorIndexing {
                    bits,
                    dst_width,
                    palette,
                } => Stage::ColorIndexing {
                    bits: *bits,
                    dst_width: *dst_width,
                    palette: palette.clone(),
                },
            })
            .collect();
        Self {
            stages,
            height_done: 0,
            ready: Vec::new(),
        }
    }

    /// Flow one coded (working-width) row through every stage in application
    /// order, appending one finalized output row to `ready`.
    ///
    /// The row is **copied** first: `core.argb` is the LZ77 back-reference window
    /// and must never be mutated by the inverse (back-references read coded values
    /// throughout the pixel loop). Each stage transforms the working buffer in
    /// place, except the predictor (which reconstructs into a fresh row from its
    /// `prev_out`, saved before later stages can touch it) and color-indexing
    /// (which expands the reduced width to `dst_width`).
    fn push_coded_row(&mut self, coded_row: &[u32]) {
        let mut buf = coded_row.to_vec();
        let y = self.height_done as usize;
        for stage in &mut self.stages {
            match stage {
                Stage::SubtractGreen => subtract_green::inverse_row(&mut buf),
                Stage::CrossColor { bits, width, data } => {
                    debug_assert_eq!(buf.len(), *width as usize, "cross-color coded-row width");
                    cross_color::inverse_row(&mut buf, y, *bits, data);
                },
                Stage::Predictor {
                    bits,
                    width,
                    data,
                    prev_out,
                } => {
                    let w = *width as usize;
                    debug_assert_eq!(buf.len(), w, "predictor coded-row width");
                    let mut out = vec![0u32; w];
                    predictor::reconstruct_row_into(&mut out, &buf, prev_out, y, *bits, data);
                    // Save the reconstructed row before later stages mutate `buf`.
                    prev_out.clone_from(&out);
                    buf = out;
                },
                Stage::ColorIndexing {
                    bits,
                    dst_width,
                    palette,
                } => {
                    buf = palette::inverse_row(&buf, *dst_width, *bits, palette);
                },
            }
        }
        self.ready.extend_from_slice(&buf);
        self.height_done += 1;
    }
}

/// The outcome of one [`Vp8lStream::advance`].
#[derive(Debug)]
pub(crate) enum Step {
    /// The current prefix was consumed without finishing; push a larger one.
    NeedMore,
    /// The header parsed: `(width, height, alpha_used)`. Reported exactly once.
    Header(u32, u32, bool),
    /// Output rows newly finalized by the incremental inverse: `count` rows
    /// starting at `first_row` (0-based). One LZ77 copy can finalize a burst of
    /// rows, so a single advance may report many at once.
    Rows { first_row: u32, count: u32 },
    /// Every pixel is decoded and inverted; call [`Vp8lStream::into_pixels`].
    Done,
}

impl Vp8lStream {
    /// A fresh decoder, awaiting its first input push.
    pub(crate) const fn new() -> Self {
        Self {
            state: Phase::Parsing,
        }
    }

    /// Advance the decode over `payload` (an append-only prefix of the VP8L
    /// bitstream). `final_input` marks `payload` as the complete stream, turning
    /// any suspension into a hard [`Error::Truncated`].
    ///
    /// # Errors
    ///
    /// A definite bitstream contradiction (surfaced as soon as it is reached from
    /// real bytes), or [`Error::Truncated`] when `final_input` is set but the
    /// stream ends early.
    pub(crate) fn advance(&mut self, payload: &[u8], final_input: bool) -> Result<Step> {
        // Take ownership of the phase here (leaving a placeholder), so the streaming
        // path receives its `StreamState` by value — the phase is destructured in
        // exactly one place and `advance_streaming` can never be reached in the wrong
        // phase.
        match core::mem::replace(&mut self.state, Phase::Parsing) {
            Phase::Parsing => self.advance_parsing(payload, final_input),
            Phase::Streaming(ss) => self.advance_streaming(ss, payload, final_input),
            Phase::Done(pixels) => {
                self.state = Phase::Done(pixels);
                Ok(Step::Done)
            },
        }
    }

    /// The finished post-inverse ARGB pixels, or `None` if not yet [`Step::Done`].
    #[cfg_attr(
        not(any(test, feature = "oracle")),
        expect(
            dead_code,
            reason = "into_pixels yields the whole decoded buffer at once, which \
                      only the test/oracle split driver (`stream_over_splits`) \
                      wants; the public IncrementalDecoder consumes rows \
                      incrementally through `ready` and never needs it"
        )
    )]
    pub(crate) fn into_pixels(self) -> Option<Vec<u32>> {
        match self.state {
            Phase::Done(pixels) => Some(pixels),
            Phase::Parsing | Phase::Streaming(_) => None,
        }
    }

    /// The finalized output rows accumulated so far, row-major at the output
    /// width — the incremental decoder reads newly-reported [`Step::Rows`] from
    /// here to pack them. Empty before the streaming phase; after [`Step::Done`]
    /// it is the whole post-inverse buffer (every row already reported).
    pub(crate) fn ready(&self) -> &[u32] {
        match &self.state {
            Phase::Parsing => &[],
            Phase::Streaming(ss) => &ss.inverse.ready,
            Phase::Done(pixels) => pixels,
        }
    }

    /// Parsing phase — idempotent: a fresh [`BitReader`] re-parses the whole
    /// header (transforms + color cache + Huffman codes, recursing synchronously
    /// into sub-images) on every push until it succeeds from real bytes.
    fn advance_parsing(&mut self, payload: &[u8], final_input: bool) -> Result<Step> {
        let mut br = BitReader::new(payload);
        match parse_top_level(&mut br) {
            Ok((header, stream)) => {
                if br.is_eos() {
                    // Inconclusive: the parse only "succeeded" by reading past the
                    // buffer into zero-padding. Discard it and wait for a larger
                    // prefix — or, if this is the whole input, it is truncated.
                    return if final_input {
                        Err(Error::Truncated)
                    } else {
                        Ok(Step::NeedMore)
                    };
                }
                // Parsed entirely from real bytes: capture the boundary and enter
                // the streaming phase.
                let resume_bits = br.bit_position();
                let ParsedStream {
                    transforms,
                    working_width,
                    total,
                    cache_bits,
                    groups,
                    entropy,
                } = stream;
                let inverse = InverseChain::new(&transforms);
                let core = PixelCore::new(working_width, total, cache_bits, groups, entropy);
                self.state = Phase::Streaming(Box::new(StreamState {
                    core,
                    resume_bits,
                    coded_rows_done: 0,
                    inverse,
                    reduced_width: working_width as usize,
                }));
                Ok(Step::Header(header.0, header.1, header.2))
            },
            Err(err) => {
                if br.is_eos() {
                    // Ran past the buffer before reaching any contradiction:
                    // inconclusive, exactly like the `Ok` + `is_eos` case.
                    if final_input {
                        Err(err)
                    } else {
                        Ok(Step::NeedMore)
                    }
                } else {
                    // A definite contradiction reached entirely from real bytes —
                    // the same bytes the full stream carries (append-only), so
                    // surface it now. This also covers a truncated Huffman table
                    // that fails as `InvalidBitstream` rather than latching
                    // `is_eos`: `!is_eos` ⇒ definite.
                    Err(err)
                }
            },
        }
    }

    /// Streaming phase — resume the pixel loop from `resume_bits` over the (grown)
    /// buffer, committing units until the buffer is exhausted or the image is done.
    ///
    /// Each committed unit flows any newly-completed coded rows through the
    /// incremental [`InverseChain`]. Rows finalized during a call are reported as
    /// [`Step::Rows`]; the terminal [`Step::Done`] follows once every row has been
    /// handed out (so every output row is covered by exactly one `Rows`, then
    /// `Done`). [`Vp8lStream::into_pixels`] then yields the accumulated `ready`,
    /// which is byte-identical to the whole-buffer batch inverse.
    fn advance_streaming(
        &mut self,
        mut ss: Box<StreamState>,
        payload: &[u8],
        final_input: bool,
    ) -> Result<Step> {
        // `ss` is owned here (the dispatcher left a placeholder phase); it is put
        // back with `self.state = Phase::Streaming(ss)` on suspend/error, or
        // replaced with `Phase::Done` on completion.
        let mut br = BitReader::new_at(payload, ss.resume_bits);
        let rows_before = ss.inverse.height_done;
        loop {
            if ss.core.pos == ss.core.total {
                // Every coded pixel is in and every coded row has been pushed
                // through the incremental inverse. Report rows finalized on *this*
                // call first; a re-entry (still `pos == total`) then reports `Done`
                // once there is nothing left to hand out.
                let new_rows = ss.inverse.height_done - rows_before;
                if new_rows > 0 {
                    self.state = Phase::Streaming(ss);
                    return Ok(Step::Rows {
                        first_row: rows_before,
                        count: new_rows,
                    });
                }
                let pixels = core::mem::take(&mut ss.inverse.ready);
                self.state = Phase::Done(pixels);
                return Ok(Step::Done);
            }
            match decode_one(&mut br, &mut ss.core) {
                Ok(true) => {
                    // Committed a unit read entirely from real bytes; advance the
                    // resume point past it, then flow every coded row the commit
                    // just completed through the incremental inverse (one LZ77 copy
                    // can finalize a burst of rows at once).
                    ss.resume_bits = br.bit_position();
                    while (ss.coded_rows_done as usize) < ss.core.pos / ss.reduced_width {
                        let start = ss.coded_rows_done as usize * ss.reduced_width;
                        let end = start + ss.reduced_width;
                        ss.inverse.push_coded_row(&ss.core.argb[start..end]);
                        ss.coded_rows_done += 1;
                    }
                },
                Ok(false) => {
                    // Suspended: the unit ran past the buffer with nothing mutated.
                    // `resume_bits` still points before it, so the next (larger)
                    // push re-reads it from a byte-identical window. Report any rows
                    // finalized before the suspension.
                    let new_rows = ss.inverse.height_done - rows_before;
                    self.state = Phase::Streaming(ss);
                    if final_input {
                        return Err(Error::Truncated);
                    }
                    return Ok(if new_rows > 0 {
                        Step::Rows {
                            first_row: rows_before,
                            count: new_rows,
                        }
                    } else {
                        Step::NeedMore
                    });
                },
                Err(err) => {
                    self.state = Phase::Streaming(ss);
                    return Err(err);
                },
            }
        }
    }
}

/// Drive a fresh [`Vp8lStream`] over `payload` for a given sequence of
/// non-decreasing prefix cut points, returning `(width, height, alpha_used,
/// pixels)` — the shape a one-shot decode must agree with.
///
/// A final whole-payload feed is always appended, so the stream is forced to
/// resolve regardless of `splits`. Used by the equivalence proptests and, under
/// the `oracle` feature, the differential split test.
#[cfg(any(test, feature = "oracle"))]
pub(crate) fn stream_over_splits(
    payload: &[u8],
    splits: &[usize],
) -> Result<(u32, u32, bool, Vec<u32>)> {
    let len = payload.len();
    let mut stream = Vp8lStream::new();
    let mut header: Option<(u32, u32, bool)> = None;
    // Output rows reported via `Step::Rows` so far; every coded row is handed out
    // exactly once, contiguously from 0, before `Done`.
    let mut rows_reported = 0u32;

    // Feed each prefix, then a guaranteed final whole-payload feed. Each feed
    // advances until the stream needs a larger prefix (`NeedMore`) or finishes
    // (`Done`); a `Header` only transitions into streaming, so keep advancing.
    let cuts = splits.iter().copied().chain(core::iter::once(len));
    'outer: for cut in cuts {
        let cut = cut.min(len);
        let final_input = cut == len;
        loop {
            match stream.advance(&payload[..cut], final_input)? {
                Step::Header(w, h, a) => header = Some((w, h, a)),
                // Rows finalized on this prefix — verify they extend the payout
                // contiguously, then keep advancing the same prefix (more rows,
                // then `NeedMore`/`Done`).
                Step::Rows { first_row, count } => {
                    assert_eq!(first_row, rows_reported, "Rows payout is not contiguous");
                    rows_reported += count;
                },
                Step::NeedMore => break,
                Step::Done => break 'outer,
            }
        }
    }

    let (w, h, a) = header.ok_or(Error::InvalidBitstream {
        codec: crate::lossless::Codec::Lossless,
    })?;
    // On a clean finish, every output row was reported exactly once.
    assert_eq!(
        rows_reported, h,
        "reported rows did not sum to the image height"
    );
    let pixels = stream.into_pixels().ok_or(Error::Truncated)?;
    Ok((w, h, a, pixels))
}

/// Canonical split granularities for a `len`-byte payload: all-at-once, one byte
/// at a time (the suspend/resume worst case), and a couple of coarse cuts.
#[cfg(any(test, feature = "oracle"))]
pub(crate) fn split_patterns(len: usize) -> Vec<Vec<usize>> {
    let mut patterns = vec![
        // All-at-once (the driver appends the single final feed).
        Vec::new(),
        // One byte at a time; the driver appends the final `len` cut.
        (1..len).collect(),
    ];
    if len >= 2 {
        patterns.push(vec![len / 2]);
    }
    if len >= 3 {
        patterns.push(vec![len / 3, 2 * len / 3]);
    }
    patterns
}

#[cfg(test)]
mod tests {
    use super::{InverseChain, Step, Vp8lStream, split_patterns, stream_over_splits};
    use crate::Codec;
    use crate::lossless::Error;
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::vp8l::decode::{
        ParsedStream, PixelCore, decode, decode_one, parse_top_level,
    };
    use crate::lossless::vp8l::encode::encode as vp8l_encode;
    use proptest::prelude::*;

    /// The comparable form both decoders must agree on: `(w, h, pixels)` or the
    /// error.
    type DecodeResult = Result<(u32, u32, Vec<u32>), Error>;

    fn one_shot(payload: &[u8]) -> DecodeResult {
        decode(payload).map(|d| (d.width, d.height, d.argb))
    }

    fn streamed(payload: &[u8], splits: &[usize]) -> DecodeResult {
        stream_over_splits(payload, splits).map(|(w, h, _a, px)| (w, h, px))
    }

    /// The streaming decoder must reproduce the one-shot result — pixels *and*
    /// error — over every canonical split, including 1-byte-at-a-time and
    /// all-at-once.
    fn assert_stream_equivalence(payload: &[u8]) {
        let expected = one_shot(payload);
        for splits in split_patterns(payload.len()) {
            assert_eq!(
                streamed(payload, &splits),
                expected,
                "stream != one-shot for split {splits:?} on a {}-byte payload",
                payload.len()
            );
        }
    }

    /// Encode a small ARGB image to a raw VP8L payload (our encoder emits
    /// subtract-green; cross-transform coverage comes from the fixtures + oracle).
    fn encode_image(width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
        vp8l_encode(width, height, argb)
    }

    /// Locate the top-level `VP8L` chunk payload in a RIFF/WebP file (skipping
    /// `VP8X`/`ICCP`/… and never descending into `ANMF`), self-contained so the
    /// L2 streaming tests take no dependency on the L3 container layer.
    fn extract_vp8l(webp: &[u8]) -> Option<&[u8]> {
        if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
            return None;
        }
        let mut off = 12usize;
        while off + 8 <= webp.len() {
            let size =
                u32::from_le_bytes([webp[off + 4], webp[off + 5], webp[off + 6], webp[off + 7]])
                    as usize;
            let start = off + 8;
            let end = start.checked_add(size)?;
            if end > webp.len() {
                return None;
            }
            if &webp[off..off + 4] == b"VP8L" {
                return Some(&webp[start..end]);
            }
            off = end + (size & 1);
        }
        None
    }

    /// Arbitrary small ARGB images, encoded to a VP8L payload.
    fn arbitrary_argb_image() -> impl Strategy<Value = (u32, u32, Vec<u32>)> {
        (1u32..=32, 1u32..=32).prop_flat_map(|(w, h)| {
            let n = (w * h) as usize;
            (Just(w), Just(h), prop::collection::vec(any::<u32>(), n..=n))
        })
    }

    proptest! {
        /// Streaming our own encoder's output over an arbitrary non-decreasing
        /// split — including the 1-byte and all-at-once extremes via
        /// `split_patterns`, plus a random split — reproduces the one-shot pixels.
        #[test]
        fn stream_equals_one_shot_over_arbitrary_splits(
            (w, h, argb) in arbitrary_argb_image(),
            raw_splits in prop::collection::vec(0usize..2048, 0..16),
        ) {
            let payload = encode_image(w, h, &argb);
            let expected = one_shot(&payload);

            for splits in split_patterns(payload.len()) {
                prop_assert_eq!(
                    streamed(&payload, &splits),
                    expected.clone(),
                    "canonical split {:?}",
                    splits
                );
            }

            let mut splits: Vec<usize> =
                raw_splits.iter().map(|&s| s % (payload.len() + 1)).collect();
            splits.sort_unstable();
            prop_assert_eq!(streamed(&payload, &splits), expected, "random split {:?}", splits);
        }

        /// Over *arbitrary* (mostly malformed) bytes, streaming must still agree
        /// with one-shot bit-for-bit — same pixels when it decodes, same error
        /// variant when it does not. This exercises the parse-phase third-state
        /// interpretation on real, contradictory bytes.
        #[test]
        fn stream_equals_one_shot_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
        ) {
            assert_stream_equivalence(&bytes);
        }

        /// The same, but on bytes carrying a valid VP8L signature, so the parse
        /// gets further before diverging.
        #[test]
        fn stream_equals_one_shot_on_signed_arbitrary_bytes(
            tail in prop::collection::vec(any::<u8>(), 0..64),
        ) {
            let mut bytes = vec![0x2fu8];
            bytes.extend_from_slice(&tail);
            assert_stream_equivalence(&bytes);
        }
    }

    /// Cross-transform coverage: every still-image conformance fixture (which
    /// together exercise cross-color, palette, gradient, LZ77, color-cache, alpha,
    /// transparent, row, and column layouts) must stream-decode identically to
    /// one-shot over every split. Animations (no top-level `VP8L`) are skipped.
    #[test]
    fn stream_equals_one_shot_on_conformance_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../webpkit-lossless-conformance/fixtures/decode");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&dir).expect("read conformance fixtures dir") {
            let input = entry.expect("dir entry").path().join("input.webp");
            if !input.exists() {
                continue;
            }
            let webp = std::fs::read(&input).expect("read fixture input.webp");
            let Some(payload) = extract_vp8l(&webp) else {
                continue; // animation / no top-level VP8L chunk
            };
            // Sanity: the fixture must actually decode one-shot, or the
            // equivalence would be a vacuous error-equals-error.
            assert!(
                one_shot(payload).is_ok(),
                "{input:?} did not decode one-shot"
            );
            assert_stream_equivalence(payload);
            checked += 1;
        }
        assert!(
            checked >= 10,
            "expected the streaming equivalence to cover many transform families, \
             only reached {checked} fixtures"
        );
    }

    /// The raw VP8L payloads of every still-image conformance fixture (skipping
    /// animations, which carry no top-level `VP8L` chunk).
    fn conformance_vp8l_payloads() -> Vec<Vec<u8>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../webpkit-lossless-conformance/fixtures/decode");
        let mut payloads = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read conformance fixtures dir") {
            let input = entry.expect("dir entry").path().join("input.webp");
            if !input.exists() {
                continue;
            }
            let webp = std::fs::read(&input).expect("read fixture input.webp");
            if let Some(payload) = extract_vp8l(&webp) {
                payloads.push(payload.to_vec());
            }
        }
        payloads
    }

    /// The streaming decoder's incrementally-inverted `ready` (accumulated one
    /// coded row at a time through the [`InverseChain`]) must equal the
    /// whole-buffer batch decode over every transform family in the conformance
    /// corpus (cross-color, palette, gradient/predictor, LZ77, color-cache, alpha,
    /// transparent, row, and column layouts).
    #[test]
    fn streamed_rows_equal_batch() {
        let payloads = conformance_vp8l_payloads();
        assert!(
            payloads.len() >= 10,
            "expected many transform-family fixtures, only found {}",
            payloads.len()
        );
        for payload in &payloads {
            let batch = decode(payload).expect("fixture decodes one-shot").argb;
            let (_w, _h, _a, streamed) =
                stream_over_splits(payload, &[]).expect("fixture streams to completion");
            assert_eq!(streamed, batch, "incremental streamed rows != batch decode");
        }
    }

    /// The [`InverseChain`] reproduces the whole-buffer batch inverse row by row,
    /// and — because it operates only on copies — leaves the coded LZ77 window
    /// (`PixelCore::argb`) byte-for-byte unchanged.
    #[test]
    fn inverse_chain_matches_batch_and_leaves_coded_unmodified() {
        for payload in &conformance_vp8l_payloads() {
            // Parse + run the pixel loop to obtain the coded (pre-inverse) buffer.
            let mut br = BitReader::new(payload);
            let (_header, stream) = parse_top_level(&mut br).expect("parse top-level");
            let ParsedStream {
                transforms,
                working_width,
                total,
                cache_bits,
                groups,
                entropy,
            } = stream;
            let mut core = PixelCore::new(working_width, total, cache_bits, groups, entropy);
            while core.pos < core.total {
                assert!(
                    decode_one(&mut br, &mut core).expect("decode_one"),
                    "unexpected suspend on a whole fixture"
                );
            }
            let reduced_width = working_width as usize;
            let snapshot = core.argb.clone();

            // Drive the incremental inverse straight from the coded window.
            let mut chain = InverseChain::new(&transforms);
            let mut y = 0usize;
            while y * reduced_width < core.argb.len() {
                chain.push_coded_row(&core.argb[y * reduced_width..(y + 1) * reduced_width]);
                y += 1;
            }

            // The inverse reads `core.argb` only through `&[u32]`, so the coded
            // LZ77 window is untouched...
            assert_eq!(core.argb, snapshot, "InverseChain mutated the coded buffer");
            // ...and it reproduces the whole-buffer batch inverse exactly.
            let batch = decode(payload).expect("fixture decodes one-shot").argb;
            assert_eq!(chain.ready, batch, "InverseChain != batch inverse");
        }
    }

    #[test]
    fn short_prefix_needs_more() {
        // A 4-byte non-final prefix is too short to finish parsing the header.
        let payload = encode_image(4, 4, &[0xff00_0000u32; 16]);
        let mut stream = Vp8lStream::new();
        assert!(matches!(
            stream.advance(&payload[..4], false),
            Ok(Step::NeedMore)
        ));
    }

    #[test]
    fn definite_error_on_non_final_prefix_surfaces_now() {
        // The first byte is not the 0x2f VP8L signature — a contradiction reached
        // from real bytes, so even a non-final prefix must surface it immediately
        // rather than asking for more input.
        let bad = [0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55];
        let mut stream = Vp8lStream::new();
        assert_eq!(
            stream.advance(&bad, false).unwrap_err(),
            Error::InvalidBitstream {
                codec: Codec::Lossless
            }
        );
    }

    #[test]
    fn final_truncated_buffer_is_truncated() {
        // Build a valid payload, find a truncation the one-shot decoder reports as
        // Truncated (i.e. past the header, mid pixel data), and confirm the final
        // (whole-buffer) stream feed reports the same.
        let argb: Vec<u32> = (0..64u32).map(|i| 0xff00_0000 | (i * 4)).collect();
        let full = encode_image(8, 8, &argb);
        let cut = (1..full.len())
            .find(|&k| one_shot(&full[..k]) == Err(Error::Truncated))
            .expect("some prefix of a real image truncates mid-stream");
        assert_eq!(streamed(&full[..cut], &[]), Err(Error::Truncated));
    }

    /// `split_patterns` must yield the *exact* canonical split set — all-at-once,
    /// one-byte-at-a-time, and the half / thirds coarse cuts, gated by length — so
    /// the streaming-equivalence tests genuinely exercise several non-trivial
    /// suspension schedules. A degenerate set (empty, or a single trivial split)
    /// would silently hollow out that coverage, so pin the whole shape.
    #[test]
    fn split_patterns_are_the_canonical_set() {
        // len == 6 exercises both size gates: `[len/2] = [3]` and
        // `[len/3, 2*len/3] = [2, 4]`, plus the dense 1-byte schedule.
        assert_eq!(
            split_patterns(6),
            vec![
                vec![],              // all-at-once
                vec![1, 2, 3, 4, 5], // one byte at a time
                vec![3],             // half
                vec![2, 4],          // thirds
            ],
        );
        // len == 1: neither coarse cut fires; only the two base patterns, both
        // empty (`1..1` is empty).
        assert_eq!(
            split_patterns(1),
            vec![Vec::<usize>::new(), Vec::<usize>::new()],
        );
        // len == 2: the half cut fires, the thirds cut does not.
        assert_eq!(split_patterns(2), vec![vec![], vec![1], vec![1]]);
    }

    /// After the last coded rows are handed out, a re-entry at `pos == total`
    /// with nothing left to report must terminate with [`Step::Done`] — never a
    /// zero-count [`Step::Rows`] (which the `new_rows > 0` gate exists to prevent;
    /// weakening it to `>= 0` would spin the driver forever). Every emitted
    /// `Rows` step therefore carries a strictly positive count.
    #[test]
    fn completion_reports_done_not_empty_rows() {
        let payload = encode_image(4, 4, &[0xff00_0000u32; 16]);
        let mut stream = Vp8lStream::new();
        let mut rows_total = 0u32;
        let mut saw_done = false;
        // Bounded so a mutant that never emits `Done` (repeating empty `Rows`)
        // is caught by the assertions below rather than hanging the test.
        for _ in 0..64 {
            match stream
                .advance(&payload, true)
                .expect("full payload advances")
            {
                Step::Header(..) => {},
                Step::Rows { count, .. } => {
                    assert!(count > 0, "a Rows step must report at least one row");
                    rows_total += count;
                },
                Step::NeedMore => panic!("a final whole-payload feed must not NeedMore"),
                Step::Done => {
                    saw_done = true;
                    break;
                },
            }
        }
        assert!(saw_done, "streaming never reported Done");
        assert_eq!(rows_total, 4, "every output row reported exactly once");
    }

    /// A streaming-phase suspension that finalized *no* new row must report
    /// [`Step::NeedMore`], not a zero-count [`Step::Rows`]. This pins the
    /// `new_rows > 0` discriminant in the suspend arm against both `== 0` (which
    /// would flip Rows/NeedMore) and `>= 0` (which would emit an empty Rows and
    /// spin the driver).
    #[test]
    fn streaming_suspend_with_no_new_rows_reports_need_more() {
        // A single-row image: no coded row completes until the very last pixel, so
        // any earlier mid-pixel suspension necessarily has `new_rows == 0`.
        let argb: Vec<u32> = (0..24u32).map(|i| 0xff00_0000 | (i * 9)).collect();
        let payload = encode_image(24, 1, &argb);

        let need_more = (1..payload.len()).find(|&len| {
            let mut stream = Vp8lStream::new();
            // First advance must fully parse the header (enter the streaming phase).
            if !matches!(stream.advance(&payload[..len], false), Ok(Step::Header(..))) {
                return false;
            }
            // Second advance drives the pixel loop over the same short prefix; with
            // no row yet complete a suspension must ask for more input.
            matches!(stream.advance(&payload[..len], false), Ok(Step::NeedMore))
        });

        assert!(
            need_more.is_some(),
            "expected a prefix that parses the header then suspends mid-row as NeedMore"
        );
    }
}
