//! Assembling a lossy image's `ALPH` chunk: the encode-side counterpart of the
//! umbrella crate's alpha compositor.
//!
//! A lossy `VP8 ` frame carries no alpha; a WebP image's 8-bit alpha plane rides
//! in a sibling `ALPH` chunk, whose payload is a 1-byte header plus the plane
//! stored either raw (`method = 0`) or as a lossless VP8L stream (`method = 1`),
//! each optionally spatially pre-filtered (none / horizontal / vertical /
//! gradient). Alpha is **lossless**, so the sole objective is the smallest valid
//! payload; [`compress_alpha`] searches every filter × method combination and
//! keeps the smallest, deterministically.
//!
//! The filter kernels and the 1-byte header live in the core shell (bitstream-
//! agnostic); the VP8L compression is delegated to the `lossless` codec. This module is only
//! the orchestration seam that ties them together — the layering the umbrella
//! crate mirrors on decode (core-shell un-filter + `crate::lossless::decode_alpha`).

use crate::alpha::{self, AlphaCompression, AlphaFilter};

use crate::lossy::prelude::*;

/// The four spatial filters, tried in this fixed order so ties break
/// deterministically toward the earliest (and, within a filter, toward the
/// lossless method — see [`compress_alpha`]).
const FILTERS: [AlphaFilter; 4] = [
    AlphaFilter::None,
    AlphaFilter::Horizontal,
    AlphaFilter::Vertical,
    AlphaFilter::Gradient,
];

/// Compress an 8-bit alpha `plane` (`width * height` bytes, row-major) into the
/// smallest valid `ALPH` chunk payload: its 1-byte header followed by the stored
/// plane.
///
/// For each spatial filter the plane is forward-filtered once, then two candidate
/// encodings are formed — lossless VP8L (`method = 1`) and raw (`method = 0`) — and
/// the globally smallest payload across all eight candidates is returned. The scan
/// order (filter `None → H → V → Gradient`, lossless before raw within each filter)
/// with a strict "smaller wins" rule makes the choice deterministic: on a tie the
/// earlier candidate is kept.
#[must_use]
pub(crate) fn compress_alpha(plane: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut best: Option<Vec<u8>> = None;
    for &filter in &FILTERS {
        let filtered = alpha::filter_plane(filter, plane, w, h);
        // method 1: the filtered plane as a headerless (green-lane) VP8L stream.
        let lossless = crate::lossless::encode_alpha(&filtered, width, height);
        consider(
            &mut best,
            assemble(AlphaCompression::Lossless, filter, &lossless),
        );
        // method 0: the filtered plane stored raw.
        consider(
            &mut best,
            assemble(AlphaCompression::None, filter, &filtered),
        );
    }
    // width*height >= 1 guarantees at least one candidate (`None`/raw always fits).
    best.unwrap_or_else(|| assemble(AlphaCompression::None, AlphaFilter::None, plane))
}

/// Build one `ALPH` payload: the 1-byte header (`compression`, `filter`,
/// pre-processing = 0) followed by the stored `data`.
fn assemble(compression: AlphaCompression, filter: AlphaFilter, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + data.len());
    out.push(alpha::build_header(compression, filter, 0));
    out.extend_from_slice(data);
    out
}

/// Replace `best` with `candidate` when it is strictly smaller (ties keep the
/// incumbent, so the earlier scan position wins).
fn consider(best: &mut Option<Vec<u8>>, candidate: Vec<u8>) {
    if best.as_ref().is_none_or(|b| candidate.len() < b.len()) {
        *best = Some(candidate);
    }
}

#[cfg(test)]
mod tests {
    use crate::alpha::{AlphaCompression, parse_header, unfilter};

    use super::compress_alpha;

    /// A pattern value narrowed into a byte (no lossy cast).
    fn byte(v: u32) -> u8 {
        u8::try_from(v & 0xff).unwrap_or(0)
    }

    /// Decompress an `ALPH` payload back to a `width * height` alpha plane, the
    /// exact inverse the umbrella crate performs on decode.
    fn decompress(alph: &[u8], width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let (header, data) = parse_header(alph).unwrap();
        let mut plane = match header.compression {
            AlphaCompression::None => data[..w * h].to_vec(),
            AlphaCompression::Lossless => {
                crate::lossless::decode_alpha(data, width, height).unwrap()
            },
        };
        unfilter(header.filter, &mut plane, w, h);
        plane
    }

    #[test]
    fn compress_alpha_round_trips_byte_exact() {
        // A flat run, a smooth ramp, a scattered pattern, and a two-region field:
        // every one must decompress back to the source plane byte-for-byte.
        let cases: [(u32, u32, Vec<u8>); 4] = [
            (4, 4, vec![0xC0u8; 16]),
            (8, 3, (0..24u32).map(|v| byte(v * 10)).collect()),
            (
                5,
                5,
                (0..25u32)
                    .map(|v| byte(v.wrapping_mul(53) ^ 0x1f))
                    .collect(),
            ),
            (
                6,
                4,
                (0..24u32)
                    .map(|i| if i % 6 < 3 { 0 } else { 255 })
                    .collect(),
            ),
        ];
        for (w, h, plane) in cases {
            let alph = compress_alpha(&plane, w, h);
            assert_eq!(decompress(&alph, w, h), plane, "{w}x{h} alpha round-trip");
        }
    }

    #[test]
    fn compress_alpha_keeps_the_smallest_candidate() {
        // A flat plane: every filter's deltas are trivially compressible, but the
        // chosen payload must be no larger than any individual candidate. Compare
        // against the raw method-0 none-filter baseline (1 header byte + plane).
        let plane = vec![0x42u8; 32];
        let chosen = compress_alpha(&plane, 8, 4);
        let raw_baseline = 1 + plane.len();
        assert!(
            chosen.len() <= raw_baseline,
            "chosen {} must not exceed raw baseline {raw_baseline}",
            chosen.len()
        );
    }

    #[test]
    fn compress_alpha_incompressible_plane_keeps_the_first_smallest_raw() {
        use crate::alpha::{AlphaFilter, build_header};
        // A high-entropy plane VP8L cannot shrink below its raw size, so every
        // lossless candidate is strictly larger than the raw ones; the four raw
        // candidates (one per filter) then tie at the global minimum length
        // (1 header byte + `width*height`). The strict "smaller wins" rule must
        // keep the FIRST such candidate in scan order — filter None, method 0
        // (raw) — which for the identity None filter is exactly the header byte
        // followed by the untouched plane. A `<`->`<=` flip picks the LAST tying
        // raw (filter Gradient); `<`->`==`/`>` picks a larger lossless candidate;
        // each changes these exact bytes.
        let plane: Vec<u8> = (0..64u32)
            .map(|i| {
                let mut z = i.wrapping_add(1).wrapping_mul(0x9E37_79B1);
                z ^= z >> 15;
                z = z.wrapping_mul(0x85EB_CA77);
                z ^= z >> 13;
                z = z.wrapping_mul(0xC2B2_AE3D);
                z ^= z >> 16;
                byte(z)
            })
            .collect();
        let mut expected = vec![build_header(AlphaCompression::None, AlphaFilter::None, 0)];
        expected.extend_from_slice(&plane);
        assert_eq!(compress_alpha(&plane, 8, 8), expected);
    }

    #[test]
    fn compress_alpha_flat_plane_chooses_lossless() {
        // A flat plane collapses to a handful of VP8L bytes, far under the raw
        // header+plane size, so the smallest candidate MUST be a lossless one.
        // A `consider` turned into a no-op — or a `>` flip that keeps the largest
        // candidate — instead returns the raw None-filter fallback (method 0),
        // which this pins against.
        let plane = vec![0xA7u8; 32];
        let chosen = compress_alpha(&plane, 8, 4);
        let (header, _) = parse_header(&chosen).unwrap();
        assert_eq!(header.compression, AlphaCompression::Lossless);
        assert!(
            chosen.len() < 1 + plane.len(),
            "lossless flat-plane payload {} must beat the {}-byte raw baseline",
            chosen.len(),
            1 + plane.len()
        );
    }

    #[test]
    fn compress_alpha_is_deterministic() {
        let plane: Vec<u8> = (0..48u32).map(|v| byte(v.wrapping_mul(29))).collect();
        assert_eq!(compress_alpha(&plane, 8, 6), compress_alpha(&plane, 8, 6));
    }
}
