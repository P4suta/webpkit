//! The `ALPH` chunk: a WebP image's optional 8-bit alpha plane, carried
//! separately from a lossy `VP8` frame.
//!
//! This module owns the bitstream-agnostic pieces of `ALPH`: the one-byte header
//! ([`parse_header`] / [`build_header`]), the spatial *un-filter* that turns the
//! stored per-pixel deltas back into an alpha plane ([`unfilter`] /
//! [`unfilter_row`]), and its exact inverse, the forward *filter*
//! ([`filter_plane`]) an encoder applies before storage. It does **not**
//! (de)compress the lossless-coded variant — that hands the payload slice off to
//! the `lossless` (VP8L) codec — so nothing here interprets a single bit of a Huffman
//! stream.
//!
//! The header layout and the filter kernels are ported verbatim from libwebp:
//! `dec/alpha_dec.c` (the `ALPHDecodeHeader` bit fields) and `dsp/filters.c`
//! (`HorizontalUnfilter_C`, `VerticalUnfilter_C`, `GradientUnfilter_C`, and
//! `GradientPredictor_C`).

use crate::error::{Error, Result};
use crate::prelude::*;

/// How the alpha plane's bytes are stored in an `ALPH` chunk (the `method`
/// field, libwebp `dec/alpha_dec.c`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlphaCompression {
    /// Method `0`: the plane is stored as raw, uncompressed bytes.
    None,
    /// Method `1`: the plane is a WebP-lossless (VP8L) coded image.
    Lossless,
}

/// The spatial predictor applied to the alpha plane before storage (the
/// `filter` field, libwebp `dsp/filters.c`). Un-filtering inverts it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlphaFilter {
    /// No prediction: stored bytes are the alpha values themselves.
    None = 0,
    /// Each byte predicts from the pixel to its left.
    Horizontal = 1,
    /// Each byte predicts from the pixel directly above.
    Vertical = 2,
    /// Each byte predicts from `left + top - top_left`, clamped to `0..=255`.
    Gradient = 3,
}

/// A parsed `ALPH` header: the three interpreted 2-bit fields of its lead byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AlphaHeader {
    /// How the plane payload is encoded (`method`).
    pub compression: AlphaCompression,
    /// The spatial predictor to invert (`filter`).
    pub filter: AlphaFilter,
    /// The pre-processing level (`pre_processing`); `0` or `1` are defined.
    pub preprocessing: u8,
}

/// Build the one-byte `ALPH` header from its three fields (the inverse of
/// [`parse_header`]).
///
/// LSB-packed per libwebp: `method` in bits 0-1, `filter` in bits 2-3, and
/// `pre_processing` in bits 4-5 (masked to two bits). Reserved bits 6-7 stay clear.
#[must_use]
pub const fn build_header(
    compression: AlphaCompression,
    filter: AlphaFilter,
    preprocessing: u8,
) -> u8 {
    let method = match compression {
        AlphaCompression::None => 0,
        AlphaCompression::Lossless => 1,
    };
    method | ((filter as u8) << 2) | ((preprocessing & 0b11) << 4)
}

/// Parse the one-byte `ALPH` header and return it with the alpha data slice
/// (everything after the header byte, `chunk[1..]`).
///
/// The header byte is LSB-packed, per libwebp `dec/alpha_dec.c`:
/// `method` = bits 0-1, `filter` = bits 2-3, `pre_processing` = bits 4-5, and
/// `rsrv` (reserved, must be zero) = bits 6-7. Every 2-bit `filter` value maps
/// to a defined [`AlphaFilter`], so no filter value is rejected.
///
/// # Errors
///
/// * [`Error::Truncated`] if `chunk` is empty.
/// * [`Error::InvalidContainer`] if `method` is neither `0` nor `1`, if
///   `pre_processing` exceeds `1`, or if any reserved bit is set.
pub fn parse_header(chunk: &[u8]) -> Result<(AlphaHeader, &[u8])> {
    let (&byte, data) = chunk.split_first().ok_or(Error::Truncated)?;
    let compression = match byte & 0b11 {
        0 => AlphaCompression::None,
        1 => AlphaCompression::Lossless,
        _ => return Err(Error::InvalidContainer),
    };
    // Every 2-bit filter code is legal; mask leaves the value in `0..=3`.
    let filter = match (byte >> 2) & 0b11 {
        0 => AlphaFilter::None,
        1 => AlphaFilter::Horizontal,
        2 => AlphaFilter::Vertical,
        _ => AlphaFilter::Gradient,
    };
    let preprocessing = (byte >> 4) & 0b11;
    if preprocessing > 1 {
        return Err(Error::InvalidContainer);
    }
    if byte >> 6 != 0 {
        // Reserved bits 6-7 must be clear.
        return Err(Error::InvalidContainer);
    }
    Ok((
        AlphaHeader {
            compression,
            filter,
            preprocessing,
        },
        data,
    ))
}

/// libwebp `GradientPredictor_C`: `clamp(a + b - c, 0, 255)`, with `a`/`b`/`c`
/// the left, top, and top-left reconstructed neighbors.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the sum is clamped into 0..=255 before the byte cast, exactly as \
              libwebp GradientPredictor_C stores its clamped int into a uint8_t"
)]
fn gradient_predictor(a: u8, b: u8, c: u8) -> u8 {
    (i32::from(a) + i32::from(b) - i32::from(c)).clamp(0, 255) as u8
}

/// libwebp `HorizontalUnfilter_C`: reconstruct `row` as the running sum of its
/// deltas, seeded by the pixel directly above column 0 (`prev[0]`), or `0` when
/// there is no previous row.
fn horizontal(prev: Option<&[u8]>, row: &mut [u8]) {
    let mut acc = prev.map_or(0, |p| p.first().copied().unwrap_or(0));
    for value in row.iter_mut() {
        acc = acc.wrapping_add(*value);
        *value = acc;
    }
}

/// libwebp `VerticalUnfilter_C` (interior rows): each byte adds the byte
/// directly above it. `prev` is the previous reconstructed row.
fn vertical(prev: &[u8], row: &mut [u8]) {
    for (value, &top) in row.iter_mut().zip(prev) {
        *value = top.wrapping_add(*value);
    }
}

/// libwebp `GradientUnfilter_C` (interior rows): each byte adds
/// `gradient_predictor(left, top, top_left)`. `prev` is the previous
/// reconstructed row; `left`/`top_left` seed from `prev[0]`, so column 0
/// predicts from directly above.
fn gradient(prev: &[u8], row: &mut [u8]) {
    let mut top_left = prev.first().copied().unwrap_or(0);
    let mut left = top_left;
    for (value, &top) in row.iter_mut().zip(prev) {
        let grad = gradient_predictor(left, top, top_left);
        left = value.wrapping_add(grad);
        top_left = top;
        *value = left;
    }
}

/// Reconstruct one row in place from its filtered deltas (libwebp
/// `WebPUnfilters`).
///
/// On entry `row` holds the stored deltas; on return it holds the reconstructed
/// alpha. `prev` is the previously reconstructed row, or `None` for row 0 —
/// where every non-[`AlphaFilter::None`] filter reduces to the horizontal
/// running sum.
pub fn unfilter_row(filter: AlphaFilter, prev: Option<&[u8]>, row: &mut [u8]) {
    match (filter, prev) {
        (AlphaFilter::None, _) => {},
        // Row 0 (no previous row) is a horizontal running sum for all filters;
        // the plain horizontal filter is that on every row.
        (AlphaFilter::Horizontal, _) | (AlphaFilter::Vertical | AlphaFilter::Gradient, None) => {
            horizontal(prev, row);
        },
        (AlphaFilter::Vertical, Some(prev)) => vertical(prev, row),
        (AlphaFilter::Gradient, Some(prev)) => gradient(prev, row),
    }
}

/// Un-filter a whole `width * height` alpha plane in place, feeding each row the
/// prior *reconstructed* row via [`unfilter_row`].
///
/// Does nothing (safely) if `width` or `height` is `0`, or if `plane.len()`
/// does not equal `width * height`.
pub fn unfilter(filter: AlphaFilter, plane: &mut [u8], width: usize, height: usize) {
    let Some(expected) = width.checked_mul(height) else {
        return;
    };
    if width == 0 || height == 0 || plane.len() != expected {
        return;
    }
    for r in 0..height {
        // Split so the already-reconstructed rows (`done`) can seed the current
        // one (`cur`) without aliasing.
        let (done, rest) = plane.split_at_mut(r * width);
        let cur = &mut rest[..width];
        let prev = if r == 0 {
            None
        } else {
            Some(&done[(r - 1) * width..r * width])
        };
        unfilter_row(filter, prev, cur);
    }
}

/// Forward of [`horizontal`]: the running-sum reconstruction run backwards,
/// subtracting each predictor instead of adding it. `above` is the previous
/// *original* row (or `None` on row 0, seeding the predictor with `0`); interior
/// rows seed column 0 from `above[0]`.
fn forward_horizontal(above: Option<&[u8]>, orig: &[u8], dst: &mut [u8]) {
    let mut left = above.map_or(0, |a| a.first().copied().unwrap_or(0));
    for (d, &value) in dst.iter_mut().zip(orig) {
        *d = value.wrapping_sub(left);
        left = value;
    }
}

/// Forward of [`gradient`]: subtract `gradient_predictor(left, top, top_left)`
/// from each original byte, with `left`/`top_left` seeded from `above[0]` so
/// column 0 predicts from directly above.
fn forward_gradient(above: &[u8], orig: &[u8], dst: &mut [u8]) {
    let mut top_left = above.first().copied().unwrap_or(0);
    let mut left = top_left;
    for ((d, &value), &top) in dst.iter_mut().zip(orig).zip(above) {
        *d = value.wrapping_sub(gradient_predictor(left, top, top_left));
        left = value;
        top_left = top;
    }
}

/// Spatially *filter* a `width * height` alpha plane by `filter` into the
/// per-pixel deltas an `ALPH` chunk stores.
///
/// The exact algebraic inverse of [`unfilter`], mirroring [`unfilter_row`]'s
/// arm-for-arm structure. Row 0 always uses the horizontal forward with no
/// predecessor (every
/// non-[`AlphaFilter::None`] filter reduces to the running sum there); interior
/// rows use the per-filter forward against the previous *original* row (never the
/// deltas). Returns the input unchanged (cloned) when the dimensions do not match
/// `plane.len()`, so callers cannot trip an out-of-bounds index.
#[must_use]
pub fn filter_plane(filter: AlphaFilter, plane: &[u8], width: usize, height: usize) -> Vec<u8> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(plane.len()) {
        return plane.to_vec();
    }
    let mut out = vec![0u8; plane.len()];
    for r in 0..height {
        let orig = &plane[r * width..(r + 1) * width];
        let above = if r == 0 {
            None
        } else {
            Some(&plane[(r - 1) * width..r * width])
        };
        // `out` and `plane` are separate buffers, so borrow the destination row
        // and have each arm write its deltas straight into it — no per-row temp.
        let dst = &mut out[r * width..(r + 1) * width];
        match (filter, above) {
            (AlphaFilter::None, _) => dst.copy_from_slice(orig),
            // Row 0 for every filter, and every row of Horizontal, is the
            // horizontal running-sum forward.
            (AlphaFilter::Horizontal, _)
            | (AlphaFilter::Vertical | AlphaFilter::Gradient, None) => {
                forward_horizontal(above, orig, dst);
            },
            (AlphaFilter::Vertical, Some(above)) => {
                for ((d, &value), &top) in dst.iter_mut().zip(orig).zip(above) {
                    *d = value.wrapping_sub(top);
                }
            },
            (AlphaFilter::Gradient, Some(above)) => forward_gradient(above, orig, dst),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{
        AlphaCompression, AlphaFilter, AlphaHeader, build_header, filter_plane, parse_header,
        unfilter, unfilter_row,
    };
    use crate::error::Error;

    #[test]
    fn none_filter_is_identity() {
        let mut plane = [10u8, 20, 30, 40];
        unfilter(AlphaFilter::None, &mut plane, 2, 2);
        assert_eq!(plane, [10, 20, 30, 40]);
    }

    #[test]
    fn filter_plane_returns_input_clone_on_dimension_mismatch() {
        // plane.len() (3) != width * height (4): the guard must return the input
        // unchanged rather than index out of bounds. Kills the length guard's
        // `|| -> &&` (the mutant would fall through and panic).
        let plane = [10u8, 20, 30];
        assert_eq!(
            filter_plane(AlphaFilter::Horizontal, &plane, 2, 2),
            plane.to_vec()
        );
    }

    #[test]
    fn horizontal_running_sum_with_wrap_and_seed() {
        // 2x3 deltas [[10,5,250],[3,1,2]].
        // row0 (prev None): 0+10=10, 10+5=15, 15+250=265 wraps to 9 -> [10,15,9].
        // row1 seeds pred = prev[0] = 10: 13, 14, 16.
        let mut plane = [10u8, 5, 250, 3, 1, 2];
        unfilter(AlphaFilter::Horizontal, &mut plane, 3, 2);
        assert_eq!(plane, [10, 15, 9, 13, 14, 16]);
    }

    #[test]
    fn vertical_adds_pixel_above_with_wrap() {
        // 2x3. row0 = horizontal running sum: 10, 30, 60.
        // row1[i] = prev[i] + delta[i]: 10+250=260 wraps to 4, 30+5=35, 60+100=160.
        let mut plane = [10u8, 20, 30, 250, 5, 100];
        unfilter(AlphaFilter::Vertical, &mut plane, 3, 2);
        assert_eq!(plane, [10, 30, 60, 4, 35, 160]);
    }

    #[test]
    fn gradient_col0_interior_and_saturation() {
        // 2x4. row0 deltas [50,150,66,90] reconstruct (200+66 wraps to 10) to
        // prev = [50,200,10,100]. row1 deltas [10,100,200,5], seeded from prev[0]:
        //   col0: gp(50,50,50)=50, left = 10+50 = 60          (predict from above)
        //   col1: gp(60,200,50)=clamp(210)=210, 100+210 = 54  (in-range interior)
        //   col2: gp(54,10,200)=clamp(-136)=0,  200+0   = 200 (saturate low)
        //   col3: gp(200,100,10)=clamp(290)=255, 5+255  = 4   (saturate high)
        let mut plane = [50u8, 150, 66, 90, 10, 100, 200, 5];
        unfilter(AlphaFilter::Gradient, &mut plane, 4, 2);
        assert_eq!(plane, [50, 200, 10, 100, 60, 54, 200, 4]);
    }

    #[test]
    fn all_filters_agree_on_row0() {
        // No previous row: every non-NONE filter is the horizontal running sum.
        // deltas [10,5,250,3] -> 10, 15, 250+15=265 wraps to 9, 9+3=12.
        let deltas = [10u8, 5, 250, 3];
        let expected = [10u8, 15, 9, 12];
        for filter in [
            AlphaFilter::Horizontal,
            AlphaFilter::Vertical,
            AlphaFilter::Gradient,
        ] {
            let mut plane = deltas;
            unfilter(filter, &mut plane, 4, 1);
            assert_eq!(plane, expected, "{filter:?} row 0");
        }
    }

    #[test]
    fn unfilter_row_row0_matches_horizontal_or_identity() {
        let deltas = [10u8, 5, 250, 3];
        let running_sum = [10u8, 15, 9, 12];
        for filter in [
            AlphaFilter::None,
            AlphaFilter::Horizontal,
            AlphaFilter::Vertical,
            AlphaFilter::Gradient,
        ] {
            let mut row = deltas;
            unfilter_row(filter, None, &mut row);
            let want = if filter == AlphaFilter::None {
                deltas
            } else {
                running_sum
            };
            assert_eq!(row, want, "{filter:?}");
        }
    }

    #[test]
    fn unfilter_guards_bad_sizes() {
        // Length mismatch (needs 4, has 3) leaves the plane untouched.
        let mut plane = [1u8, 2, 3];
        unfilter(AlphaFilter::Horizontal, &mut plane, 2, 2);
        assert_eq!(plane, [1, 2, 3]);
        // Zero dimensions are no-ops.
        let mut p2 = [9u8, 9];
        unfilter(AlphaFilter::Gradient, &mut p2, 0, 5);
        unfilter(AlphaFilter::Gradient, &mut p2, 5, 0);
        assert_eq!(p2, [9, 9]);
    }

    #[test]
    fn parse_header_empty_is_truncated() {
        assert_eq!(parse_header(&[]).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn parse_header_fields_and_data_slice() {
        // 0x1D = 0b0001_1101: method=1 (Lossless), filter=3 (Gradient),
        // pre_processing=1, rsrv=0.
        let (header, data) = parse_header(&[0x1D, 0xAA, 0xBB]).unwrap();
        assert_eq!(
            header,
            AlphaHeader {
                compression: AlphaCompression::Lossless,
                filter: AlphaFilter::Gradient,
                preprocessing: 1,
            }
        );
        assert_eq!(data, &[0xAA, 0xBB]);
    }

    #[test]
    fn parse_header_accepts_every_filter() {
        // method=0, pre_processing=0, rsrv=0; filter occupies bits 2-3.
        for (byte, filter) in [
            (0x00u8, AlphaFilter::None),
            (0x04, AlphaFilter::Horizontal),
            (0x08, AlphaFilter::Vertical),
            (0x0C, AlphaFilter::Gradient),
        ] {
            let bytes = [byte];
            let (header, data) = parse_header(&bytes).unwrap();
            assert_eq!(header.filter, filter);
            assert_eq!(header.compression, AlphaCompression::None);
            assert_eq!(header.preprocessing, 0);
            assert!(data.is_empty());
        }
    }

    #[test]
    fn parse_header_reads_both_methods() {
        assert_eq!(
            parse_header(&[0x00]).unwrap().0.compression,
            AlphaCompression::None
        );
        assert_eq!(
            parse_header(&[0x01]).unwrap().0.compression,
            AlphaCompression::Lossless
        );
    }

    #[test]
    fn parse_header_rejects_reserved_bits_and_bad_fields() {
        // Reserved bits 6-7 set.
        assert_eq!(parse_header(&[0x40]).unwrap_err(), Error::InvalidContainer);
        assert_eq!(parse_header(&[0x80]).unwrap_err(), Error::InvalidContainer);
        // method 2 and 3.
        assert_eq!(parse_header(&[0x02]).unwrap_err(), Error::InvalidContainer);
        assert_eq!(parse_header(&[0x03]).unwrap_err(), Error::InvalidContainer);
        // pre_processing 2 (0x20) and 3 (0x30).
        assert_eq!(parse_header(&[0x20]).unwrap_err(), Error::InvalidContainer);
        assert_eq!(parse_header(&[0x30]).unwrap_err(), Error::InvalidContainer);
    }

    /// A strategy over all four spatial filters.
    fn any_filter() -> impl Strategy<Value = AlphaFilter> {
        prop_oneof![
            Just(AlphaFilter::None),
            Just(AlphaFilter::Horizontal),
            Just(AlphaFilter::Vertical),
            Just(AlphaFilter::Gradient),
        ]
    }

    #[test]
    fn build_header_round_trips_through_parse() {
        // Every field combination `build_header` can emit must parse back to the
        // same three fields, with no data bytes.
        for compression in [AlphaCompression::None, AlphaCompression::Lossless] {
            for filter in [
                AlphaFilter::None,
                AlphaFilter::Horizontal,
                AlphaFilter::Vertical,
                AlphaFilter::Gradient,
            ] {
                for preprocessing in 0u8..=1 {
                    let bytes = [build_header(compression, filter, preprocessing)];
                    let (header, data) = parse_header(&bytes).unwrap();
                    assert_eq!(header.compression, compression);
                    assert_eq!(header.filter, filter);
                    assert_eq!(header.preprocessing, preprocessing);
                    assert!(data.is_empty());
                }
            }
        }
        // Reserved bits 6-7 are never set.
        assert_eq!(
            build_header(AlphaCompression::Lossless, AlphaFilter::Gradient, 1) >> 6,
            0
        );
    }

    proptest! {
        /// Filtering an original plane and then [`unfilter`]ing the result is the
        /// identity, for every filter and random plane / dimensions — the forward
        /// kernels here are the algebraic inverse of the decoder's un-filters.
        #[test]
        fn forward_then_unfilter_round_trips(
            (width, height, plane) in (1usize..=17, 1usize..=17).prop_flat_map(|(w, h)| {
                prop::collection::vec(any::<u8>(), w * h).prop_map(move |plane| (w, h, plane))
            }),
            filter in any_filter(),
        ) {
            let mut filtered = filter_plane(filter, &plane, width, height);
            unfilter(filter, &mut filtered, width, height);
            prop_assert_eq!(filtered, plane);
        }

        /// [`unfilter`] never panics for arbitrary dimensions and buffer lengths
        /// (which need not equal `width * height`), always preserves the buffer
        /// length, and leaves the bytes untouched whenever its length guard trips.
        #[test]
        fn unfilter_never_panics_and_is_length_guarded(
            filter in any_filter(),
            width in 0usize..=40,
            height in 0usize..=40,
            bytes in prop::collection::vec(any::<u8>(), 0..=64),
        ) {
            let original = bytes.clone();
            let mut buf = bytes;
            unfilter(filter, &mut buf, width, height);
            prop_assert_eq!(buf.len(), original.len());
            let matches_plane =
                width != 0 && height != 0 && width.checked_mul(height) == Some(original.len());
            if !matches_plane {
                // The guarded early return must leave the buffer byte-for-byte.
                prop_assert_eq!(buf, original);
            }
        }

        /// [`parse_header`] never panics on arbitrary bytes: it always returns a
        /// `Result`, and any success carries exactly the post-header slice.
        #[test]
        fn parse_header_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=8)) {
            if let Ok((_, data)) = parse_header(&bytes) {
                prop_assert_eq!(data, &bytes[1..]);
            }
        }
    }
}
