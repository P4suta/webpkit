//! Color-indexing (palette) transform — inverse ports libwebp
//! `ColorIndexInverseTransform`, and `expand_color_map` ports `ExpandColorMap`.
//!
//! The color map is transmitted per-byte delta-coded (stride 4), so expanding it
//! is a per-channel cumulative `add_pixels`. When `bits > 0` several small indices
//! are bundled LSB-first into the green byte of each source pixel, so the inverse
//! also un-bundles and expands the reduced width back to the destination width.

use crate::lossless::constants::subsample_size;
use crate::lossless::prelude::*;
use crate::lossless::transform::{add_pixels, sub_pixels};

/// Expand the transmitted (per-byte delta-coded) color map to `1 << (8 >> bits)`
/// entries; the unused tail stays transparent `0x0000_0000` (not `ARGB_BLACK`).
#[must_use]
pub(crate) fn expand_color_map(num_colors: usize, raw: &[u32], bits: u32) -> Vec<u32> {
    let final_num_colors = 1usize << (8u32 >> bits);
    let mut map = vec![0u32; final_num_colors];
    let n = num_colors.min(final_num_colors).min(raw.len());
    if n > 0 {
        map[0] = raw[0];
        for k in 1..n {
            map[k] = add_pixels(map[k - 1], raw[k]);
        }
    }
    map
}

/// Map bundled index pixels (index carried in the green byte) to palette colors,
/// expanding the reduced source width back to `dst_width`.
///
/// Each source row is independent (the packed index byte is refreshed at every
/// row start), so this delegates to [`inverse_row`] — the batch and
/// row-streaming forms share one implementation and can never drift.
#[must_use]
pub(crate) fn inverse(src: &[u32], dst_width: u32, bits: u32, palette: &[u32]) -> Vec<u32> {
    let dst_w = dst_width as usize;
    let src_w = subsample_size(dst_width, bits) as usize;
    if src_w == 0 {
        return Vec::new();
    }
    let height = src.len() / src_w;
    let mut dst = Vec::with_capacity(dst_w * height);
    for row in src.chunks_exact(src_w) {
        dst.extend(inverse_row(row, dst_width, bits, palette));
    }
    dst
}

/// Palette inverse over a single reduced-width source row, expanding it to
/// `dst_width` — the row-streaming counterpart of the whole-buffer [`inverse`].
/// The packed index byte is unpacked LSB-first and refreshed at every `ppb`
/// boundary from the start of the row, so looping this over each source row
/// reproduces [`inverse`] exactly (proven in tests).
#[must_use]
pub(crate) fn inverse_row(src_row: &[u32], dst_width: u32, bits: u32, palette: &[u32]) -> Vec<u32> {
    let dst_w = dst_width as usize;
    let bpp = 8u32 >> bits;
    let mut dst = vec![0u32; dst_w];
    if bpp == 8 {
        // No bundling: one index per source pixel, straight through the palette.
        for (d, &s) in dst.iter_mut().zip(src_row) {
            *d = lookup(palette, (s >> 8) & 0xff);
        }
    } else {
        let pixels_per_byte = 1usize << bits;
        let count_mask = pixels_per_byte - 1;
        let bit_mask = (1u32 << bpp) - 1;
        let mut si = 0usize;
        let mut packed = 0u32;
        for (x, d) in dst.iter_mut().enumerate() {
            if x & count_mask == 0 {
                // Fresh index byte from the GREEN channel, unpacked LSB-first.
                packed = (src_row[si] >> 8) & 0xff;
                si += 1;
            }
            *d = lookup(palette, packed & bit_mask);
            packed >>= bpp;
        }
    }
    dst
}

/// Palette read; out-of-range indices fall to the transparent zero tail.
fn lookup(palette: &[u32], index: u32) -> u32 {
    palette.get(index as usize).copied().unwrap_or(0)
}

/// A palette-encoded image: the delta-coded color map plus the bundled index
/// plane — exactly what [`inverse`] + [`expand_color_map`] consume to rebuild the
/// original pixels.
pub(crate) struct Palette {
    /// Bundling bit-width (`0/1/2/3`), chosen from `num_colors` by the same
    /// threshold the decoder applies; `8 >> bits` index bits pack per pixel.
    pub(crate) bits: u32,
    /// Number of distinct colors (`1..=256`), i.e. the color map's entry count.
    pub(crate) num_colors: u32,
    /// Per-channel delta-coded color map (`colormap[0]` verbatim, later entries
    /// `sub_pixels(palette[k], palette[k - 1])`), transmitted as a sub-image.
    pub(crate) colormap: Vec<u32>,
    /// Index plane: palette indices packed LSB-first into each green byte,
    /// `subsample_size(width, bits)` pixels wide per row.
    pub(crate) bundled: Vec<u32>,
}

/// Color-indexing forward — the exact inverse of [`inverse`] + [`expand_color_map`].
///
/// Returns `None` when the image has more than 256 distinct colors (palette
/// coding is inapplicable) or `width` is zero. Distinct colors are collected in
/// sorted order (deterministic, no hash set) and their positions become the
/// indices; those indices are bundled LSB-first into the green byte exactly as
/// [`inverse`] unpacks them, and the palette is delta-coded the way
/// [`expand_color_map`] re-expands it.
#[must_use]
pub(crate) fn forward(argb: &[u32], width: u32) -> Option<Palette> {
    if width == 0 {
        return None;
    }
    // Distinct colors in deterministic sorted order; index = sorted position.
    let mut colors = argb.to_vec();
    colors.sort_unstable();
    colors.dedup();
    if colors.len() > 256 {
        return None;
    }
    let num_colors = u32::try_from(colors.len()).ok()?;

    // Same threshold as the decoder (`decode.rs`): more colors need fewer index
    // bits per pixel, so fewer indices bundle into each green byte.
    let bits: u32 = if colors.len() > 16 {
        0
    } else if colors.len() > 4 {
        1
    } else if colors.len() > 2 {
        2
    } else {
        3
    };

    // Color map: verbatim first entry, then per-channel deltas — the inverse of
    // the decoder's cumulative `add_pixels` expansion.
    let mut colormap = Vec::with_capacity(colors.len());
    if !colors.is_empty() {
        colormap.push(colors[0]);
        for k in 1..colors.len() {
            colormap.push(sub_pixels(colors[k], colors[k - 1]));
        }
    }

    // Bundle indices LSB-first into the green byte, `pixels_per_byte` per output
    // pixel, restarting the accumulator at every row and every `ppb` boundary so
    // the decoder's per-row refresh recovers them exactly.
    let bpp = 8u32 >> bits;
    let pixels_per_byte = 1u32 << bits;
    let w = width as usize;
    let src_w = subsample_size(width, bits) as usize;
    let mut bundled = Vec::with_capacity(src_w * (argb.len() / w));
    for row in argb.chunks_exact(w) {
        let mut bundle = 0u32;
        let mut count = 0u32;
        for &pixel in row {
            let pos = colors.binary_search(&pixel).unwrap_or(0);
            let index = u32::try_from(pos).unwrap_or(0);
            bundle |= index << (count * bpp);
            count += 1;
            if count == pixels_per_byte {
                bundled.push(bundle << 8);
                bundle = 0;
                count = 0;
            }
        }
        // Flush a partial trailing bundle (row width not a multiple of `ppb`).
        if count != 0 {
            bundled.push(bundle << 8);
        }
    }

    Some(Palette {
        bits,
        num_colors,
        colormap,
        bundled,
    })
}

#[cfg(test)]
mod tests {
    use super::{expand_color_map, forward, inverse, inverse_row};
    use crate::lossless::constants::subsample_size;
    use proptest::prelude::*;

    #[test]
    fn expand_color_map_cumulative_delta_with_transparent_tail() {
        // bits=1 -> final_num_colors = 1 << (8 >> 1) = 1 << 4 = 16.
        // map[0] = raw[0]; each later entry is a per-channel wrapping add.
        let raw = [0x0102_0304, 0x1020_3040, 0x0101_0101];
        let map = expand_color_map(3, &raw, 1);
        assert_eq!(map.len(), 16);
        assert_eq!(map[0], 0x0102_0304);
        assert_eq!(map[1], 0x1122_3344); // add_pixels(map[0], raw[1])
        assert_eq!(map[2], 0x1223_3445); // add_pixels(map[1], raw[2])
        // Tail beyond num_colors stays transparent zero (NOT ARGB_BLACK).
        for &entry in &map[3..16] {
            assert_eq!(entry, 0x0000_0000);
        }
    }

    #[test]
    fn expand_color_map_zero_colors_stays_all_transparent() {
        // n == num_colors.min(final).min(raw.len()) == 0 here. The `n > 0` guard
        // must stay false so raw[0] is never copied in: `>=` would fire on n == 0
        // and write raw[0] into map[0], which this asserts against.
        let raw = [0x1122_3344u32, 0x5566_7788];
        let map = expand_color_map(0, &raw, 1);
        assert_eq!(map.len(), 16);
        assert_eq!(map[0], 0x0000_0000);
        for &entry in &map {
            assert_eq!(entry, 0x0000_0000);
        }
    }

    #[test]
    fn inverse_bundled_bpp2_unpacks_lsb_first_and_refreshes_each_row() {
        let palette = [0x0000_00aa, 0x0000_00bb, 0x0000_00cc, 0x0000_00dd];
        // bits=2 -> bpp=2, 4 px/byte; dst_width=4 -> src_w=1 (one index byte/row).
        // Row 0 green byte 0b11_10_01_00 unpacks LSB-first to 0,1,2,3.
        // Row 1 green byte 0b00_01_10_11 unpacks LSB-first to 3,2,1,0, proving the
        // packed byte is refreshed at the start of every row.
        let src = [0x0000_e400, 0x0000_1b00];
        let dst = inverse(&src, 4, 2, &palette);
        assert_eq!(
            dst,
            [
                0x0000_00aa,
                0x0000_00bb,
                0x0000_00cc,
                0x0000_00dd, // row 0
                0x0000_00dd,
                0x0000_00cc,
                0x0000_00bb,
                0x0000_00aa, // row 1
            ]
        );
    }

    #[test]
    fn whole_buffer_inverse_equals_looping_inverse_row_bpp2() {
        // Reuse the bpp=2 fixture: two source rows (src_w=1 each) must produce the
        // same pixels whether decoded whole-buffer or one row at a time.
        let palette = [0x0000_00aa, 0x0000_00bb, 0x0000_00cc, 0x0000_00dd];
        let src = [0x0000_e400u32, 0x0000_1b00];
        let batch = inverse(&src, 4, 2, &palette);
        let mut rows = Vec::new();
        rows.extend(inverse_row(&src[0..1], 4, 2, &palette));
        rows.extend(inverse_row(&src[1..2], 4, 2, &palette));
        assert_eq!(batch, rows);
    }

    #[test]
    fn inverse_8bpp_direct_lookup_with_out_of_range_index() {
        let palette = [0x0000_00aa, 0x0000_00bb, 0x0000_00cc];
        // bits=0 -> bpp=8, src_w=dst_width=3, one index per pixel via green byte.
        // Green bytes 0, 2, 5: index 5 is out of range and returns transparent 0.
        let src = [0x0000_0000, 0x0000_0200, 0x0000_0500];
        let dst = inverse(&src, 3, 0, &palette);
        assert_eq!(dst, [0x0000_00aa, 0x0000_00cc, 0x0000_0000]);
    }

    /// Distinct-color count -> index bit-width, mirroring the decoder threshold
    /// (`decode.rs:249-257`); the rule `forward` must reproduce byte-for-byte.
    fn expected_bits(num_colors: u32) -> u32 {
        if num_colors > 16 {
            0
        } else if num_colors > 4 {
            1
        } else if num_colors > 2 {
            2
        } else {
            3
        }
    }

    /// `k`-th distinct palette color: `k * ODD (+ base)` is a bijection on `u32`,
    /// so entries stay distinct while spanning every channel.
    fn distinct_color(k: usize) -> u32 {
        0x9E37_79B1u32
            .wrapping_mul(u32::try_from(k).unwrap())
            .wrapping_add(0x1234_5678)
    }

    #[test]
    fn forward_2_colors_uses_bits3_and_round_trips() {
        // 2 distinct colors -> bits=3 (bpp=1, 8 px/byte). Width 3 leaves partial
        // bundles (only 3 of 8 index slots per row filled).
        let a = 0x1122_3344;
        let b = 0xaabb_ccdd;
        let argb = [a, b, a, b, a, b];
        let p = forward(&argb, 3).unwrap();
        assert_eq!(p.num_colors, 2);
        assert_eq!(p.bits, 3);
        let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
        assert_eq!(inverse(&p.bundled, 3, p.bits, &expanded), argb.to_vec());
    }

    #[test]
    fn forward_16_colors_uses_bits1_and_round_trips() {
        // 16 distinct colors -> bits=1 (bpp=4, 2 px/byte).
        let argb: Vec<u32> = (0..16).map(distinct_color).collect();
        let p = forward(&argb, 4).unwrap();
        assert_eq!(p.num_colors, 16);
        assert_eq!(p.bits, 1);
        let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
        assert_eq!(inverse(&p.bundled, 4, p.bits, &expanded), argb);
    }

    #[test]
    fn forward_256_colors_uses_bits0_and_round_trips() {
        // 256 distinct colors -> bits=0 (bpp=8, one index per pixel, no bundling).
        let argb: Vec<u32> = (0..256).map(distinct_color).collect();
        let p = forward(&argb, 16).unwrap();
        assert_eq!(p.num_colors, 256);
        assert_eq!(p.bits, 0);
        let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
        assert_eq!(inverse(&p.bundled, 16, p.bits, &expanded), argb);
    }

    #[test]
    fn forward_over_256_colors_is_none() {
        let argb: Vec<u32> = (0..257).map(distinct_color).collect();
        assert!(forward(&argb, 257).is_none());
    }

    #[test]
    fn forward_zero_width_is_none() {
        assert!(forward(&[0x0000_00aa], 0).is_none());
    }

    #[test]
    fn forward_reverses_the_bpp2_inverse_fixture() {
        // Reverse `inverse_bundled_bpp2_unpacks_lsb_first_and_refreshes_each_row`:
        // its `dst` fed to forward must reproduce that test's bundled `src` and a
        // 4-color / bits-2 palette. The sorted palette [aa,bb,cc,dd] is already
        // ascending, so indices match the fixture and the green bytes are 0xE4/0x1B.
        let dst = [
            0x0000_00aa,
            0x0000_00bb,
            0x0000_00cc,
            0x0000_00dd, // row 0
            0x0000_00dd,
            0x0000_00cc,
            0x0000_00bb,
            0x0000_00aa, // row 1
        ];
        let p = forward(&dst, 4).unwrap();
        assert_eq!(p.num_colors, 4);
        assert_eq!(p.bits, 2);
        assert_eq!(p.bundled, [0x0000_e400, 0x0000_1b00]);
        // Delta-coded map: verbatim first entry then per-channel deltas.
        assert_eq!(
            p.colormap,
            [0x0000_00aa, 0x0000_0011, 0x0000_0011, 0x0000_0011]
        );
        let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
        assert_eq!(inverse(&p.bundled, 4, p.bits, &expanded), dst.to_vec());
    }

    proptest! {
        #[test]
        fn forward_then_inverse_is_identity(
            // A random color pool bounds the distinct count; the pixel stream
            // draws from it. Small dims exercise partial rows and per-row bundle
            // refresh across bits 0..=3.
            pool in prop::collection::vec(any::<u32>(), 1..=64),
            picks in prop::collection::vec(any::<u16>(), 1..96),
            width in 1u32..=9,
        ) {
            let w = width as usize;
            let argb: Vec<u32> =
                picks.iter().map(|&v| pool[v as usize % pool.len()]).collect();
            // Trim to a whole number of rows so height is exact.
            let usable = argb.len() / w * w;
            prop_assume!(usable > 0);
            let argb = &argb[..usable];

            let p = forward(argb, width).expect("<= 256 distinct colors yields Some");
            // The bits field is derived from num_colors by the decoder's own rule.
            prop_assert_eq!(p.bits, expected_bits(p.num_colors));
            let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
            let restored = inverse(&p.bundled, width, p.bits, &expanded);
            prop_assert_eq!(restored, argb.to_vec());
        }

        /// The whole-buffer inverse equals concatenating [`inverse_row`] over each
        /// reduced source row — the row-streaming decoder's per-row payout — across
        /// every bundling width (`bits` 0..=3).
        #[test]
        fn whole_buffer_inverse_equals_looping_inverse_row(
            pool in prop::collection::vec(any::<u32>(), 1..=64),
            picks in prop::collection::vec(any::<u16>(), 1..96),
            width in 1u32..=9,
        ) {
            let w = width as usize;
            let argb: Vec<u32> =
                picks.iter().map(|&v| pool[v as usize % pool.len()]).collect();
            let usable = argb.len() / w * w;
            prop_assume!(usable > 0);
            let argb = &argb[..usable];

            let p = forward(argb, width).expect("<= 256 distinct colors yields Some");
            let expanded = expand_color_map(p.num_colors as usize, &p.colormap, p.bits);
            let batch = inverse(&p.bundled, width, p.bits, &expanded);

            let src_w = subsample_size(width, p.bits) as usize;
            let mut rows = Vec::with_capacity(batch.len());
            for row in p.bundled.chunks_exact(src_w) {
                rows.extend(inverse_row(row, width, p.bits, &expanded));
            }
            prop_assert_eq!(batch, rows);
        }
    }
}
