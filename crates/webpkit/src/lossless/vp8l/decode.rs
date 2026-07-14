//! VP8L payload decoder: header, transform + entropy-image parse, the main
//! literal / LZ77 / color-cache pixel loop, and inverse transforms.
//!
//! Ported from libwebp 1.6.0 `src/dec/vp8l_dec.c` (`ReadImageInfo`,
//! `DecodeImageStream`, `ReadHuffmanCodes`, `DecodeImageData`) and the
//! subtract-green inverse from `src/dsp/lossless.c`.
//!
//! Coverage: all four VP8L transforms (predictor, cross-color, subtract-green,
//! and color-indexing/palette with pixel bundling), meta-Huffman entropy
//! images, color cache, and LZ77 back-references.
#![allow(
    clippy::cast_possible_truncation,
    reason = "pixel packing/unpacking and size arithmetic are bounded by the 14-bit VP8L \
              dimensions and 8-bit channels"
)]

use crate::lossless::bit_io::reader::BitReader;
use crate::lossless::color_cache::ColorCache;
use crate::lossless::constants::{
    ALPHABET_SIZE, COLOR_INDEXING_TRANSFORM, CROSS_COLOR_TRANSFORM, MAX_CACHE_BITS,
    MIN_TRANSFORM_BITS, NUM_LENGTH_CODES, NUM_LITERAL_CODES, NUM_TRANSFORM_BITS,
    PREDICTOR_TRANSFORM, SUBTRACT_GREEN_TRANSFORM, VP8L_IMAGE_SIZE_BITS, VP8L_MAGIC_BYTE,
    VP8L_VERSION_BITS, subsample_size,
};
use crate::lossless::huffman::decode::{HuffmanTable, read_huffman_code};
use crate::lossless::lz77::{plane_code_to_distance, read_prefix_value};
use crate::lossless::prelude::*;
use crate::lossless::transform::{cross_color, palette, predictor, subtract_green};
use crate::lossless::{Codec, Error, Result};

/// A decoded VP8L image: native ARGB pixels (`0xAARRGGBB`), row-major.
pub(crate) struct Decoded {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) argb: Vec<u32>,
}

/// The five Huffman codes used to decode one region of the image.
pub(crate) struct HuffmanGroup {
    green: HuffmanTable,
    red: HuffmanTable,
    blue: HuffmanTable,
    alpha: HuffmanTable,
    dist: HuffmanTable,
}

/// The meta-Huffman entropy image: a per-block Huffman-group selector.
pub(crate) struct EntropyImage {
    /// Group index per subsampled block.
    data: Vec<u16>,
    /// Width of the subsampled block grid.
    xsize: u32,
    /// Subsample precision in bits.
    bits: u32,
}

/// A reversible transform parsed from the stream (applied in reverse on decode).
///
/// Each variant captures the pixel width it operates on at parse time (libwebp
/// `VP8LTransform::xsize_`). Color-indexing additionally reduces the working
/// width of every later stage (pixel bundling), so its inverse expands the
/// decoded buffer back to `dst_width`.
///
/// Exposed to [`crate::lossless::vp8l::decode_incr`], which captures the transform list at
/// parse time and inverts it **incrementally** (one coded row at a time through
/// its `InverseChain`), producing pixels byte-identical to the whole-buffer
/// [`apply_inverse_transforms`] the one-shot path uses.
pub(crate) enum Transform {
    /// Spatial predictor; `data` is the per-tile mode sub-image (mode in green).
    Predictor {
        bits: u32,
        width: u32,
        data: Vec<u32>,
    },
    /// Cross-color decorrelation; `data` holds per-tile multiplier codes.
    CrossColor {
        bits: u32,
        width: u32,
        data: Vec<u32>,
    },
    /// Subtract-green (pointwise; width-agnostic).
    SubtractGreen,
    /// Color indexing (palette + bundling); `dst_width` is the expanded width.
    ColorIndexing {
        bits: u32,
        dst_width: u32,
        palette: Vec<u32>,
    },
}

/// Read the VP8L 5-byte header (signature, `width-1`, `height-1`, alpha-used
/// advisory, version) from `br`, returning `(width, height, alpha_used)`.
fn read_header(br: &mut BitReader<'_>) -> Result<(u32, u32, bool)> {
    if br.read_bits(8) != u32::from(VP8L_MAGIC_BYTE) {
        return Err(Error::InvalidBitstream {
            codec: Codec::Lossless,
        });
    }
    let width = br.read_bits(VP8L_IMAGE_SIZE_BITS) + 1;
    let height = br.read_bits(VP8L_IMAGE_SIZE_BITS) + 1;
    let alpha_used = br.read_bit() != 0;
    if br.read_bits(VP8L_VERSION_BITS) != 0 {
        return Err(Error::InvalidBitstream {
            codec: Codec::Lossless,
        });
    }
    Ok((width, height, alpha_used))
}

/// Peek a VP8L payload's header (`width`, `height`, `alpha_used`) without
/// decoding pixel data — used to size-check against a limit and to report image
/// info before a full decode.
pub(crate) fn peek_header(payload: &[u8]) -> Result<(u32, u32, bool)> {
    let mut br = BitReader::new(payload);
    let header = read_header(&mut br)?;
    if br.is_eos() {
        return Err(Error::Truncated);
    }
    Ok(header)
}

/// Decode a full VP8L payload (starting at the `0x2f` signature).
pub(crate) fn decode(payload: &[u8]) -> Result<Decoded> {
    let mut br = BitReader::new(payload);
    let (width, height, _alpha_used) = read_header(&mut br)?;
    let argb = decode_image_stream(&mut br, width, height, true)?;
    if br.is_eos() {
        return Err(Error::Truncated);
    }
    Ok(Decoded {
        width,
        height,
        argb,
    })
}

/// Decode a headerless VP8L alpha stream (the LOSSLESS-compressed `ALPH` payload,
/// i.e. the bytes AFTER the 1-byte ALPH header) into a `width*height` alpha-byte
/// vector. Alpha is carried in the GREEN channel of the decoded ARGB image, so we
/// extract `(argb >> 8) as u8`. Always uses the full-ARGB image-stream path, which
/// is byte-identical to libwebp's `use_8b_decode` optimization (`dec/vp8l_dec.c`).
pub(crate) fn decode_alpha_stream(payload: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    // The alpha stream has no 5-byte VP8L header; dimensions come from the
    // enclosing frame, so we drive the image-stream decoder directly.
    let mut br = BitReader::new(payload);
    let argb = decode_image_stream(&mut br, width, height, true)?;
    if br.is_eos() {
        return Err(Error::Truncated);
    }
    Ok(argb.iter().map(|&p| (p >> 8) as u8).collect())
}

/// A parsed (possibly nested) image-stream header: its transforms, working
/// width, color cache, and Huffman codes — everything up to (but not including)
/// the pixel data.
///
/// Produced by [`parse_image_stream`] and consumed by [`decode_image_stream`]
/// (one-shot) and [`crate::lossless::vp8l::decode_incr`] (which parses the top-level stream
/// once and then resumes pixel decoding across input pushes). The `groups` and
/// `entropy` are moved into a [`PixelCore`] for the pixel loop; the `transforms`
/// drive the batch inverse afterwards.
pub(crate) struct ParsedStream {
    /// Transforms in parse order (applied in reverse to invert).
    pub(crate) transforms: Vec<Transform>,
    /// Pixel width the entropy/pixel stages operate on (reduced by palette
    /// bundling if a color-indexing transform is present).
    pub(crate) working_width: u32,
    /// Total coded pixels to decode (`working_width * ysize`).
    pub(crate) total: usize,
    /// Color-cache precision in bits (0 when disabled).
    pub(crate) cache_bits: u32,
    /// Huffman groups (one per meta-Huffman block, or a single group).
    pub(crate) groups: Vec<HuffmanGroup>,
    /// Optional meta-Huffman entropy image selecting the group per block.
    pub(crate) entropy: Option<EntropyImage>,
}

/// Parse a (possibly nested) image stream's transforms, color cache, and Huffman
/// codes — everything up to the pixel data. Recurses synchronously into
/// sub-images (entropy image, transform tiles, palette color-map).
///
/// `is_level0` enables transforms and meta-Huffman (the top-level image only);
/// nested streams (entropy image, transform data) pass `false`.
fn parse_image_stream(
    br: &mut BitReader<'_>,
    xsize: u32,
    ysize: u32,
    is_level0: bool,
) -> Result<ParsedStream> {
    // 1. Transforms (level 0 only), innermost last. Track the working width:
    //    color-indexing reduces it (pixel bundling), mirroring libwebp's
    //    `ReadTransform` mutating `*xsize`.
    let mut transforms = Vec::new();
    let mut transform_xsize = xsize;
    if is_level0 {
        let mut seen = 0u8;
        while br.read_bit() != 0 {
            let (transform, reduced) = read_transform(br, transform_xsize, ysize, &mut seen)?;
            transform_xsize = reduced;
            transforms.push(transform);
        }
    }

    // 2. Color cache.
    let cache_bits = if br.read_bit() != 0 {
        let bits = br.read_bits(4);
        if !(1..=MAX_CACHE_BITS).contains(&bits) {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossless,
            });
        }
        bits
    } else {
        0
    };

    // 3. Huffman codes (with optional meta-Huffman entropy image), at the
    //    working width (reduced by color-indexing if present).
    let (groups, entropy) = read_huffman_codes(br, transform_xsize, ysize, cache_bits, is_level0)?;

    let total = (transform_xsize as usize)
        .checked_mul(ysize as usize)
        .ok_or(Error::InvalidBitstream {
            codec: Codec::Lossless,
        })?;

    Ok(ParsedStream {
        transforms,
        working_width: transform_xsize,
        total,
        cache_bits,
        groups,
        entropy,
    })
}

/// Decode one (possibly nested) image stream: parse its header, decode the pixel
/// data at the working width, then invert its transforms.
fn decode_image_stream(
    br: &mut BitReader<'_>,
    xsize: u32,
    ysize: u32,
    is_level0: bool,
) -> Result<Vec<u32>> {
    let ParsedStream {
        transforms,
        working_width,
        total,
        cache_bits,
        groups,
        entropy,
    } = parse_image_stream(br, xsize, ysize, is_level0)?;

    // `decode_image_data` takes ownership of the parsed groups/entropy (a cheap
    // move) through a `PixelCore` and hands back the coded buffer.
    let argb = decode_image_data(br, working_width, total, cache_bits, groups, entropy)?;

    Ok(apply_inverse_transforms(argb, &transforms))
}

/// Apply parsed transforms in reverse of parse order to a coded ARGB buffer,
/// yielding the final pixels.
///
/// Predictor/cross-color/subtract-green run in place at their captured width;
/// color-indexing expands the working width back to the output width. Used by the
/// one-shot [`decode_image_stream`]; the streaming decoder ([`crate::lossless::vp8l::decode_incr`])
/// inverts the same graph row-by-row through its `InverseChain`, so both produce
/// byte-identical pixels.
pub(crate) fn apply_inverse_transforms(mut argb: Vec<u32>, transforms: &[Transform]) -> Vec<u32> {
    for transform in transforms.iter().rev() {
        match transform {
            Transform::SubtractGreen => subtract_green::inverse(&mut argb),
            Transform::Predictor { bits, width, data } => {
                predictor::inverse(&mut argb, *width, *bits, data);
            },
            Transform::CrossColor { bits, width, data } => {
                cross_color::inverse(&mut argb, *width, *bits, data);
            },
            Transform::ColorIndexing {
                bits,
                dst_width,
                palette,
            } => {
                argb = palette::inverse(&argb, *dst_width, *bits, palette);
            },
        }
    }
    argb
}

/// Read the VP8L 5-byte header and parse the top-level image stream (transforms,
/// color cache, Huffman codes) — everything up to the pixel data.
///
/// Used by the suspend/resume streaming decoder ([`crate::lossless::vp8l::decode_incr`]),
/// which re-runs it idempotently on each input push until it succeeds, then
/// resumes pixel decoding from the returned parse boundary. It is the exact parse
/// prefix of [`decode`], so on a complete buffer it reads bit-for-bit the same.
pub(crate) fn parse_top_level(br: &mut BitReader<'_>) -> Result<((u32, u32, bool), ParsedStream)> {
    let header = read_header(br)?;
    let stream = parse_image_stream(br, header.0, header.1, true)?;
    Ok((header, stream))
}

/// Parse one transform header (2-bit type + its data), returning the transform
/// and the (possibly reduced) working width for the stages that follow.
fn read_transform(
    br: &mut BitReader<'_>,
    xsize: u32,
    ysize: u32,
    seen: &mut u8,
) -> Result<(Transform, u32)> {
    let ty = br.read_bits(2);
    let bit = 1u8 << ty;
    if *seen & bit != 0 {
        // libwebp rejects a transform type that appears twice.
        return Err(Error::InvalidBitstream {
            codec: Codec::Lossless,
        });
    }
    *seen |= bit;

    match ty {
        PREDICTOR_TRANSFORM => {
            let bits = br.read_bits(NUM_TRANSFORM_BITS) + MIN_TRANSFORM_BITS;
            let data = read_tile_image(br, xsize, ysize, bits)?;
            Ok((
                Transform::Predictor {
                    bits,
                    width: xsize,
                    data,
                },
                xsize,
            ))
        },
        CROSS_COLOR_TRANSFORM => {
            let bits = br.read_bits(NUM_TRANSFORM_BITS) + MIN_TRANSFORM_BITS;
            let data = read_tile_image(br, xsize, ysize, bits)?;
            Ok((
                Transform::CrossColor {
                    bits,
                    width: xsize,
                    data,
                },
                xsize,
            ))
        },
        SUBTRACT_GREEN_TRANSFORM => Ok((Transform::SubtractGreen, xsize)),
        COLOR_INDEXING_TRANSFORM => {
            let num_colors = br.read_bits(8) + 1;
            let bits = if num_colors > 16 {
                0
            } else if num_colors > 4 {
                1
            } else if num_colors > 2 {
                2
            } else {
                3
            };
            let raw = decode_image_stream(br, num_colors, 1, false)?;
            let palette = palette::expand_color_map(num_colors as usize, &raw, bits);
            Ok((
                Transform::ColorIndexing {
                    bits,
                    dst_width: xsize,
                    palette,
                },
                subsample_size(xsize, bits),
            ))
        },
        // `read_bits(2)` only yields 0..=3, all matched above.
        _ => Err(Error::InvalidBitstream {
            codec: Codec::Lossless,
        }),
    }
}

/// Decode a transform's tile sub-image (predictor modes / cross-color codes).
fn read_tile_image(br: &mut BitReader<'_>, xsize: u32, ysize: u32, bits: u32) -> Result<Vec<u32>> {
    let cols = subsample_size(xsize, bits);
    let rows = subsample_size(ysize, bits);
    decode_image_stream(br, cols, rows, false)
}

/// Read the color-cache-independent meta-Huffman header and all Huffman groups.
fn read_huffman_codes(
    br: &mut BitReader<'_>,
    xsize: u32,
    ysize: u32,
    color_cache_bits: u32,
    allow_recursion: bool,
) -> Result<(Vec<HuffmanGroup>, Option<EntropyImage>)> {
    // `mapping[i]` is the destination slot of the group at addressable index `i`,
    // or `None` if `i` is never referenced by the entropy image. The bitstream
    // carries one 5-code set per index in `mapping`, but a single entropy pixel can
    // name group 0xffff, so `mapping.len()` reaches 65536 while only the DISTINCT
    // used indices are selected. Following libwebp `ReadHuffmanCodes`, we build a
    // first-appearance remap old -> [0..num_used) and allocate only the referenced
    // groups (bounded by the entropy pixel count), reading and discarding the rest.
    let (mapping, num_used, entropy) = if allow_recursion && br.read_bit() != 0 {
        let bits = br.read_bits(3) + 2;
        let entropy_cols = subsample_size(xsize, bits);
        let entropy_rows = subsample_size(ysize, bits);
        let entropy_argb = decode_image_stream(br, entropy_cols, entropy_rows, false)?;
        let mut num_groups_max = 1u32;
        let raw: Vec<u16> = entropy_argb
            .iter()
            .map(|&pixel| {
                let group = ((pixel >> 8) & 0xffff) as u16;
                num_groups_max = num_groups_max.max(u32::from(group) + 1);
                group
            })
            .collect();
        let mut mapping: Vec<Option<usize>> = vec![None; num_groups_max as usize];
        let mut num_used = 0usize;
        let data: Vec<u16> = raw
            .iter()
            .map(|&group| {
                let slot = &mut mapping[group as usize];
                let new = *slot.get_or_insert_with(|| {
                    let new = num_used;
                    num_used += 1;
                    new
                });
                new as u16
            })
            .collect();
        (
            mapping,
            num_used,
            Some(EntropyImage {
                data,
                xsize: entropy_cols,
                bits,
            }),
        )
    } else {
        (vec![Some(0usize)], 1usize, None)
    };

    let cache_codes = if color_cache_bits > 0 {
        1usize << color_cache_bits
    } else {
        0
    };
    let green_alphabet = ALPHABET_SIZE[0] + cache_codes;

    // Read every addressable index (`mapping.len()` sets) to hold the bit position
    // exact, but store only the referenced groups into their remapped slot.
    let mut groups: Vec<Option<HuffmanGroup>> = (0..num_used).map(|_| None).collect();
    for slot in &mapping {
        let group = HuffmanGroup {
            green: read_huffman_code(br, green_alphabet).ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?,
            red: read_huffman_code(br, ALPHABET_SIZE[1]).ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?,
            blue: read_huffman_code(br, ALPHABET_SIZE[2]).ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?,
            alpha: read_huffman_code(br, ALPHABET_SIZE[3]).ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?,
            dist: read_huffman_code(br, ALPHABET_SIZE[4]).ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })?,
        };
        if let &Some(new) = slot {
            groups[new] = Some(group);
        }
    }
    let groups = groups
        .into_iter()
        .map(|g| {
            g.ok_or(Error::InvalidBitstream {
                codec: Codec::Lossless,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((groups, entropy))
}

/// The mutable per-unit decode state shared by the one-shot driver and the
/// suspend/resume streaming decoder ([`crate::lossless::vp8l::decode_incr`]).
///
/// It **owns** the parsed Huffman groups and entropy image (moved in from
/// [`ParsedStream`]), so a streaming decoder can persist a `PixelCore` across
/// input pushes without a self-referential borrow. `argb` holds the *coded*
/// pixel values — the LZ77 back-reference window — at the working width; inverse
/// transforms are applied afterwards (whole-buffer via
/// [`apply_inverse_transforms`] for one-shot, or row-by-row via the streaming
/// decoder's `InverseChain`). `pos` is the next output index, `total` its bound.
pub(crate) struct PixelCore {
    /// Coded ARGB values written so far (also the LZ77 copy window).
    pub(crate) argb: Vec<u32>,
    /// Next output index (units committed so far).
    pub(crate) pos: usize,
    /// Total number of pixels to decode (`argb.len()`).
    pub(crate) total: usize,
    /// Working width used for `(x, y)` group selection and distance planes.
    pub(crate) width: u32,
    /// Optional color cache, updated in output order.
    pub(crate) cache: Option<ColorCache>,
    /// Color-cache precision in bits (0 when disabled).
    pub(crate) cache_bits: u32,
    /// Huffman groups (one per meta-Huffman block, or a single group).
    pub(crate) groups: Vec<HuffmanGroup>,
    /// Optional meta-Huffman entropy image selecting the group per block.
    pub(crate) entropy: Option<EntropyImage>,
}

impl PixelCore {
    /// Build a fresh core with a zeroed `total`-pixel coded buffer, taking
    /// ownership of the parsed Huffman `groups` and `entropy`. Shared by the
    /// one-shot pixel loop and the streaming decoder.
    pub(crate) fn new(
        width: u32,
        total: usize,
        cache_bits: u32,
        groups: Vec<HuffmanGroup>,
        entropy: Option<EntropyImage>,
    ) -> Self {
        let cache = (cache_bits > 0).then(|| ColorCache::new(cache_bits));
        Self {
            argb: vec![0u32; total],
            pos: 0,
            total,
            width,
            cache,
            cache_bits,
            groups,
            entropy,
        }
    }
}

/// The main pixel loop: literal / LZ77 back-reference / color-cache reference.
///
/// Allocates the working ARGB buffer (via [`PixelCore::new`], which takes
/// ownership of `groups`/`entropy`), drives [`decode_one`] until every pixel is
/// committed, and returns the coded buffer for inverse-transform application. A
/// unit that runs past the buffered input surfaces as [`Error::Truncated`].
fn decode_image_data(
    br: &mut BitReader<'_>,
    width: u32,
    total: usize,
    cache_bits: u32,
    groups: Vec<HuffmanGroup>,
    entropy: Option<EntropyImage>,
) -> Result<Vec<u32>> {
    let mut core = PixelCore::new(width, total, cache_bits, groups, entropy);
    while core.pos < core.total {
        if !decode_one(br, &mut core)? {
            return Err(Error::Truncated);
        }
    }
    Ok(core.argb)
}

/// Decode exactly one pixel unit at the reader's current offset.
///
/// Returns `Ok(true)` when a unit was committed (`argb` / `cache` / `pos`
/// advanced), `Ok(false)` when the reader ran past the buffered input
/// (`is_eos`) with **nothing** mutated, and `Err` on a definite bitstream
/// contradiction (distance beyond the window, copy past the end, or a
/// color-cache key out of range).
///
/// All bit reads for the unit happen before the single `is_eos` gate, and every
/// effect (buffer write, cache insert, `pos` advance) happens after it — so the
/// `Ok(false)` path needs no rollback. This mirrors the original loop body
/// bit-for-bit: the same symbols are read in the same order, and truncation
/// still wins over a range error because the `is_eos` gate precedes every range
/// check.
pub(crate) fn decode_one(br: &mut BitReader<'_>, st: &mut PixelCore) -> Result<bool> {
    let cache_limit = NUM_LITERAL_CODES + NUM_LENGTH_CODES;
    // Single-group streams (no entropy image — every Fast/Balanced stream and most
    // Best ones) skip the per-pixel div/mod and group selection: the group is
    // always index 0. Only the meta-Huffman path needs the `(x, y)` block lookup.
    let group = match st.entropy.as_ref() {
        None => st.groups.first(),
        Some(e) => {
            let pos = st.pos as u32;
            let idx = select_group(Some(e), pos % st.width, pos / st.width).ok_or(
                Error::InvalidBitstream {
                    codec: Codec::Lossless,
                },
            )?;
            st.groups.get(idx)
        },
    }
    .ok_or(Error::InvalidBitstream {
        codec: Codec::Lossless,
    })?;

    let code = group.green.read_symbol(br) as usize;

    if code < NUM_LITERAL_CODES {
        let red = group.red.read_symbol(br);
        let blue = group.blue.read_symbol(br);
        let alpha = group.alpha.read_symbol(br);
        if br.is_eos() {
            return Ok(false);
        }
        let pixel = (u32::from(alpha) << 24)
            | (u32::from(red) << 16)
            | ((code as u32) << 8)
            | u32::from(blue);
        st.argb[st.pos] = pixel;
        if let Some(c) = st.cache.as_mut() {
            c.insert(pixel);
        }
        st.pos += 1;
    } else if code < cache_limit {
        let length = read_prefix_value((code - NUM_LITERAL_CODES) as u32, br) as usize;
        let dist_symbol = group.dist.read_symbol(br);
        let dist_code = read_prefix_value(u32::from(dist_symbol), br);
        if br.is_eos() {
            return Ok(false);
        }
        let dist = plane_code_to_distance(st.width, dist_code) as usize;
        if dist > st.pos || st.pos + length > st.total {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossless,
            });
        }
        // Hoist the cache-present test out of the copy loop into two variants, so a
        // long copy pays the `Option` check once, not per copied pixel. Byte-
        // identical: the same values are written and inserted in the same order.
        let end = st.pos + length;
        if let Some(c) = st.cache.as_mut() {
            while st.pos < end {
                let value = st.argb[st.pos - dist];
                st.argb[st.pos] = value;
                c.insert(value);
                st.pos += 1;
            }
        } else {
            while st.pos < end {
                st.argb[st.pos] = st.argb[st.pos - dist];
                st.pos += 1;
            }
        }
    } else {
        if br.is_eos() {
            return Ok(false);
        }
        let key = code - cache_limit;
        let cache = st.cache.as_mut().ok_or(Error::InvalidBitstream {
            codec: Codec::Lossless,
        })?;
        if key >= (1usize << st.cache_bits) {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossless,
            });
        }
        let value = cache.get(key);
        st.argb[st.pos] = value;
        cache.insert(value);
        st.pos += 1;
    }
    Ok(true)
}

/// Select the Huffman-group index for a pixel position via the entropy image.
///
/// Returns `None` if the block index falls outside the entropy image (unreachable
/// for a consistently-parsed stream, where the grid geometry bounds `block`), so
/// the caller can reject it uniformly with the group-`get` guard rather than an
/// index panic. Without an entropy image the group is always index 0.
fn select_group(entropy: Option<&EntropyImage>, x: u32, y: u32) -> Option<usize> {
    entropy.map_or(Some(0), |e| {
        let block = (y >> e.bits) * e.xsize + (x >> e.bits);
        e.data.get(block as usize).map(|&group| group as usize)
    })
}

#[cfg(test)]
mod tests {
    use super::{PixelCore, decode, decode_one, parse_top_level};
    use crate::lossless::bit_io::reader::BitReader;

    /// Minimal LSB-first bit writer for hand-crafting VP8L test streams.
    #[derive(Default)]
    struct BitBuf {
        bytes: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl BitBuf {
        fn put(&mut self, value: u32, bits: u32) {
            self.acc |= value << self.n;
            self.n += bits;
            while self.n >= 8 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.bytes.push((self.acc & 0xff) as u8);
            }
            self.bytes
        }
    }

    /// Emit a simple 1-symbol Huffman code for `symbol` (a literal value).
    fn put_simple_code(b: &mut BitBuf, symbol: u32) {
        b.put(1, 1); // simple code
        b.put(0, 1); // num_symbols - 1 == 0 (one symbol)
        if symbol <= 1 {
            b.put(0, 1); // first_symbol_len_code: 1-bit value
            b.put(symbol, 1);
        } else {
            b.put(1, 1); // 8-bit value
            b.put(symbol, 8);
        }
    }

    /// Hand-craft a solid-color VP8L stream: `width`x`height`, every pixel the
    /// same (r, g, b, a), using 1-symbol codes so the pixel data is empty.
    fn solid_stream(width: u32, height: u32, r: u32, g: u32, b: u32, a: u32) -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8); // signature
        buf.put(width - 1, 14);
        buf.put(height - 1, 14);
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(0, 1); // no transform
        buf.put(0, 1); // no color cache
        buf.put(0, 1); // no meta-huffman
        put_simple_code(&mut buf, g); // green
        put_simple_code(&mut buf, r); // red
        put_simple_code(&mut buf, b); // blue
        put_simple_code(&mut buf, a); // alpha
        put_simple_code(&mut buf, 0); // distance (unused)
        buf.finish()
    }

    #[test]
    fn decodes_a_1x1_pixel() {
        let stream = solid_stream(1, 1, 50, 100, 200, 255);
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(out.argb, vec![0xff32_64c8]); // A=255 R=50 G=100 B=200
    }

    /// Pack channels into a native ARGB pixel.
    fn argb(a: u32, r: u32, g: u32, b: u32) -> u32 {
        (a << 24) | (r << 16) | (g << 8) | b
    }

    #[test]
    fn decodes_a_solid_block() {
        let stream = solid_stream(3, 2, 10, 20, 30, 40);
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(out.argb, vec![argb(40, 10, 20, 30); 6]);
    }

    #[test]
    fn rejects_bad_signature() {
        let mut stream = solid_stream(1, 1, 0, 0, 0, 0);
        stream[0] = 0x00;
        assert!(decode(&stream).is_err());
    }

    /// A solid stream carrying a single subtract-green transform.
    fn solid_stream_subtract_green(
        width: u32,
        height: u32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
    ) -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(width - 1, 14);
        buf.put(height - 1, 14);
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(1, 1); // transform present
        buf.put(2, 2); // type = subtract-green (2)
        buf.put(0, 1); // no more transforms
        buf.put(0, 1); // no color cache
        buf.put(0, 1); // no meta-huffman
        put_simple_code(&mut buf, g);
        put_simple_code(&mut buf, r);
        put_simple_code(&mut buf, b);
        put_simple_code(&mut buf, a);
        put_simple_code(&mut buf, 0); // distance (unused)
        buf.finish()
    }

    #[test]
    fn decodes_with_subtract_green() {
        // Stored (r, g, b) = (10, 100, 20); the inverse adds green into red/blue.
        let stream = solid_stream_subtract_green(2, 1, 10, 100, 20, 255);
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        // red' = (10 + 100) & 0xff = 110, blue' = (20 + 100) & 0xff = 120.
        assert_eq!(out.argb, vec![argb(255, 110, 100, 120); 2]);
    }

    // End-to-end coverage of the predictor, cross-color, and color-indexing
    // (palette + bundling) transforms is provided by the conformance golden
    // fixtures (decoded against dwebp), the authoritative external oracle; the
    // per-transform math is unit-tested in `crate::lossless::transform::*`.

    /// Emit a 2-symbol simple Huffman code `{sym0, sym1}` (both length 1). The
    /// decoder assigns canonical codes by ascending symbol, so reading bit 0
    /// selects the smaller symbol and bit 1 the larger.
    fn put_simple_code2(b: &mut BitBuf, sym0: u32, sym1: u32) {
        b.put(1, 1); // simple code
        b.put(1, 1); // num_symbols - 1 == 1 (two symbols)
        if sym0 <= 1 {
            b.put(0, 1);
            b.put(sym0, 1);
        } else {
            b.put(1, 1);
            b.put(sym0, 8);
        }
        b.put(sym1, 8); // second symbol is always 8-bit
    }

    /// A 1x1 main image using meta-Huffman whose single entropy pixel names group
    /// `0xffff` (green=255, red=255), forcing `num_htree_groups_max` to 65536. The
    /// stream carries all 65536 trivial 5-code sets so the parse completes; only
    /// one distinct group index is ever referenced.
    fn meta_huffman_amplify_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(0, 14); // width - 1 == 0
        buf.put(0, 14); // height - 1 == 0
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(0, 1); // no transform
        buf.put(0, 1); // no color cache (top level)
        buf.put(1, 1); // meta-huffman present
        buf.put(0, 3); // precision = read_bits(3) + 2 == 2; subsample_size(1, 2) == 1
        // Nested 1x1 entropy sub-image (is_level0 == false: no transforms/meta).
        buf.put(0, 1); // nested: no color cache
        put_simple_code(&mut buf, 255); // green -> 255
        put_simple_code(&mut buf, 255); // red -> 255  => (pixel >> 8) & 0xffff == 0xffff
        put_simple_code(&mut buf, 0); // blue
        put_simple_code(&mut buf, 0); // alpha
        put_simple_code(&mut buf, 0); // dist
        // Top-level: 65536 trivial 5-code sets (one per addressable group index).
        for _ in 0..65536u32 {
            put_simple_code(&mut buf, 0);
            put_simple_code(&mut buf, 0);
            put_simple_code(&mut buf, 0);
            put_simple_code(&mut buf, 0);
            put_simple_code(&mut buf, 0);
        }
        buf.finish()
    }

    #[test]
    fn meta_huffman_group_alloc_is_bounded_by_used_groups() {
        // The single entropy pixel names group 0xffff, so `num_htree_groups_max`
        // is 65536 — but only ONE distinct index is referenced. A faithful decoder
        // builds exactly one HuffmanGroup, not 65536 (~335 MB, an alloc-amplifying
        // DoS). All 65536 5-code sets are still read to keep the bit position exact.
        let stream = meta_huffman_amplify_stream();
        let mut br = BitReader::new(&stream);
        let (_, parsed) = parse_top_level(&mut br).unwrap();
        assert_eq!(parsed.groups.len(), 1);
    }

    /// An 8x1 main image whose 2-block entropy image names non-contiguous group
    /// indices (block 0 -> group 3, block 1 -> group 1). Group 3 codes color X,
    /// group 1 codes color Y; the unused indices 0 and 2 code black.
    fn meta_huffman_multigroup_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(7, 14); // width - 1 == 7 (width = 8)
        buf.put(0, 14); // height - 1 == 0 (height = 1)
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(0, 1); // no transform
        buf.put(0, 1); // no color cache (top level)
        buf.put(1, 1); // meta-huffman present
        buf.put(0, 3); // precision = 2; subsample_size(8, 2) == 2 cols, 1 row
        // Nested 2x1 entropy sub-image: pixel0 -> group 3, pixel1 -> group 1.
        buf.put(0, 1); // nested: no color cache
        put_simple_code2(&mut buf, 1, 3); // green: {1, 3}; bit0 -> 1, bit1 -> 3
        put_simple_code(&mut buf, 0); // red
        put_simple_code(&mut buf, 0); // blue
        put_simple_code(&mut buf, 0); // alpha
        put_simple_code(&mut buf, 0); // dist
        buf.put(1, 1); // entropy pixel0 green bit=1 -> symbol 3 -> group 3
        buf.put(0, 1); // entropy pixel1 green bit=0 -> symbol 1 -> group 1
        // Top-level: num_htree_groups_max == 4; read groups for old indices 0..=3.
        let mut emit_group = |g: u32, r: u32, b: u32, a: u32| {
            put_simple_code(&mut buf, g);
            put_simple_code(&mut buf, r);
            put_simple_code(&mut buf, b);
            put_simple_code(&mut buf, a);
            put_simple_code(&mut buf, 0); // dist
        };
        emit_group(0, 0, 0, 0); // old index 0 (unused)
        emit_group(20, 21, 22, 23); // old index 1 -> Y = argb(23, 21, 20, 22)
        emit_group(0, 0, 0, 0); // old index 2 (unused)
        emit_group(30, 31, 32, 33); // old index 3 -> X = argb(33, 31, 30, 32)
        buf.finish()
    }

    #[test]
    fn meta_huffman_remaps_non_contiguous_groups() {
        // Blocks select the physically-correct group regardless of the sparse,
        // out-of-order labels: pixels 0..4 (block 0 -> group 3) are X, pixels 4..8
        // (block 1 -> group 1) are Y. Guards that the distinct-group remap keeps
        // entropy-image indices and group placement consistent.
        let stream = meta_huffman_multigroup_stream();
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (8, 1));
        let x = argb(33, 31, 30, 32);
        let y = argb(23, 21, 20, 22);
        assert_eq!(out.argb, vec![x, x, x, x, y, y, y, y]);
    }

    /// Two subtract-green transforms in a row: libwebp (and we) reject a transform
    /// type that appears twice. Guards the duplicate-detection bit math in
    /// `read_transform` — `let bit = 1u8 << ty` and `*seen |= bit`. With `<<`
    /// mutated to `>>` (bit becomes 0 for ty=2) or `|=` mutated to `&=` (seen
    /// stays 0), the second subtract-green is wrongly accepted and the stream
    /// decodes instead of being rejected.
    fn duplicate_subtract_green_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(0, 14); // width - 1 == 0
        buf.put(0, 14); // height - 1 == 0
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(1, 1); // transform present
        buf.put(2, 2); // subtract-green (type 2)
        buf.put(1, 1); // transform present again
        buf.put(2, 2); // subtract-green AGAIN -> must be rejected
        buf.put(0, 1); // no more transforms
        buf.put(0, 1); // no color cache
        buf.put(0, 1); // no meta-huffman
        put_simple_code(&mut buf, 10);
        put_simple_code(&mut buf, 20);
        put_simple_code(&mut buf, 30);
        put_simple_code(&mut buf, 40);
        put_simple_code(&mut buf, 0);
        buf.finish()
    }

    #[test]
    fn rejects_a_repeated_transform_type() {
        // A valid stream never carries the same transform twice; accepting one is
        // a bitstream contradiction. Real code returns Err at the second
        // subtract-green; a broken duplicate-check would decode it to Ok.
        let stream = duplicate_subtract_green_stream();
        assert!(decode(&stream).is_err());
    }

    /// A single-row palette (color-indexing) stream: `dst_width` output pixels
    /// decoded from a `num_colors`-entry palette whose every color-map pixel is
    /// `cmap` (so `map[k] = (k + 1) * cmap` per channel), with the bundled index
    /// byte carried in the main image's single green literal `main_green`.
    fn palette_stream(
        dst_width: u32,
        num_colors: u32,
        cmap: (u32, u32, u32, u32),
        main_green: u32,
    ) -> Vec<u8> {
        let (cg, cr, cb, ca) = cmap;
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(dst_width - 1, 14);
        buf.put(0, 14); // height - 1 == 0 (1 row)
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(1, 1); // transform present
        buf.put(3, 2); // color-indexing (type 3)
        buf.put(num_colors - 1, 8);
        // Nested (num_colors x 1) color-map sub-image (is_level0 == false).
        buf.put(0, 1); // nested: no color cache
        put_simple_code(&mut buf, cg);
        put_simple_code(&mut buf, cr);
        put_simple_code(&mut buf, cb);
        put_simple_code(&mut buf, ca);
        put_simple_code(&mut buf, 0); // dist
        // Back to the top level.
        buf.put(0, 1); // no more transforms
        buf.put(0, 1); // top-level: no color cache
        buf.put(0, 1); // no meta-huffman
        put_simple_code(&mut buf, main_green);
        put_simple_code(&mut buf, 0); // red
        put_simple_code(&mut buf, 0); // blue
        put_simple_code(&mut buf, 0); // alpha
        put_simple_code(&mut buf, 0); // dist
        buf.finish()
    }

    #[test]
    fn palette_16_colors_uses_two_index_bundling() {
        // num_colors == 16 lands on `num_colors > 16 == false`, so `bits == 1`
        // (two 4-bit indices bundled per source pixel). Mutating `> 16` to `== 16`
        // or `>= 16` flips `bits` to 0 (no bundling, 256-entry palette), which
        // reads the whole green byte 0x21 == 33 as one out-of-range index and
        // yields transparent zeros instead of the two unpacked palette colors.
        let stream = palette_stream(2, 16, (1, 1, 1, 1), 0x21);
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        // bits=1: nibble 0 -> index 1 -> map[1], nibble 1 -> index 2 -> map[2].
        assert_eq!(out.argb, vec![0x0202_0202, 0x0303_0303]);
    }

    #[test]
    fn palette_2_colors_uses_eight_index_bundling() {
        // num_colors == 2 lands on `num_colors > 2 == false`, so `bits == 3`
        // (eight 1-bit indices bundled per source pixel). Mutating `> 2` to `>= 2`
        // flips `bits` to 2 (four 2-bit indices), which reads 0x55 == 0b01010101
        // as all-ones indices -> a uniform color instead of the alternating
        // bit-plane the real single-bit unpack produces.
        let stream = palette_stream(8, 2, (1, 1, 1, 1), 0x55);
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (8, 1));
        let m0 = 0x0101_0101u32; // map[0]
        let m1 = 0x0202_0202u32; // map[1]
        // 0x55 LSB-first single-bit indices: 1,0,1,0,1,0,1,0.
        assert_eq!(out.argb, vec![m1, m0, m1, m0, m1, m0, m1, m0]);
    }

    /// An 8x1 image carrying a single PREDICTOR transform whose tile grid is a
    /// per-tile sub-image with a *2-symbol* green code (1 bit per tile pixel), so
    /// the number of tile pixels the parser consumes depends on the transform's
    /// `bits`. The transform's `read_bits(NUM_TRANSFORM_BITS)` field is 0, so the
    /// real decoder computes `bits = 0 + MIN_TRANSFORM_BITS = 2` and reads
    /// `subsample_size(8, 2) = 2` tile pixels (2 selector bits). The main image is
    /// a solid green literal `10`; with height 1 the predictor's row-0 rule turns
    /// it into a cumulative left-add green ramp.
    fn predictor_transform_bits_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(7, 14); // width - 1 == 7 (width = 8)
        buf.put(0, 14); // height - 1 == 0 (height = 1)
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(1, 1); // transform present
        buf.put(0, 2); // type = predictor (0)
        buf.put(0, 3); // read_bits(NUM_TRANSFORM_BITS=3) == 0 -> real bits = 0+2 = 2
        // Nested tile sub-image (is_level0 == false): 2-symbol green code so every
        // tile pixel consumes exactly one selector bit. Real reads
        // subsample_size(8, 2) == 2 tile pixels; the `+`->`*` mutant computes
        // bits = 0*2 = 0 and reads subsample_size(8, 0) == 8, over-reading 6 extra
        // bits and desyncing the whole top-level parse.
        buf.put(0, 1); // nested: no color cache
        put_simple_code2(&mut buf, 0, 1); // green: {0, 1}, 1 bit/pixel
        put_simple_code(&mut buf, 0); // red
        put_simple_code(&mut buf, 0); // blue
        put_simple_code(&mut buf, 0); // alpha
        put_simple_code(&mut buf, 0); // dist
        buf.put(0, 1); // tile pixel 0 green selector -> symbol 0
        buf.put(0, 1); // tile pixel 1 green selector -> symbol 0
        // Back to the top level.
        buf.put(0, 1); // no more transforms
        buf.put(0, 1); // top-level: no color cache
        buf.put(0, 1); // no meta-huffman
        put_simple_code(&mut buf, 10); // green literal 10
        put_simple_code(&mut buf, 0); // red
        put_simple_code(&mut buf, 0); // blue
        put_simple_code(&mut buf, 0); // alpha
        put_simple_code(&mut buf, 0); // dist
        buf.finish()
    }

    #[test]
    fn predictor_transform_bits_is_sum_not_product() {
        // `let bits = br.read_bits(NUM_TRANSFORM_BITS) + MIN_TRANSFORM_BITS`; with
        // read_bits == 0 the real tile size is `bits = 2`, so the predictor tile
        // sub-image is 2 pixels (2 selector bits). Mutating `+` to `*` yields
        // `bits = 0`, an 8-pixel tile grid that reads 8 selector bits, shifting the
        // rest of the parse by 6 bits and corrupting (or rejecting) the image.
        // Height 1 means the predictor uses only the row-0 rule (origin + black,
        // then cumulative left add), so the solid green-10 literal reconstructs to
        // a green ramp 10, 20, ..., 80 with opaque alpha.
        let stream = predictor_transform_bits_stream();
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (8, 1));
        assert_eq!(
            out.argb,
            vec![
                0xff00_0a00, // g=10
                0xff00_1400, // g=20
                0xff00_1e00, // g=30
                0xff00_2800, // g=40
                0xff00_3200, // g=50
                0xff00_3c00, // g=60
                0xff00_4600, // g=70
                0xff00_5000, // g=80
            ]
        );
    }

    /// Emit a normal (non-simple) green Huffman code assigning length-1 codes to
    /// exactly two symbols: literal `0` (read bit 0) and the first length code
    /// `256` (read bit 1). This is the only way to hand-craft the green symbol
    /// `256`, which a simple code (8-bit symbols) cannot express.
    fn put_lz77_green_code(b: &mut BitBuf) {
        b.put(0, 1); // normal code (not simple)
        b.put(0, 4); // num_code_lengths - 4 == 0 -> 4
        // Code-length-code lengths for orders [17, 18, 0, 1]:
        b.put(0, 3); // order 17 -> len 0
        b.put(1, 3); // order 18 -> len 1
        b.put(0, 3); // order 0  -> len 0
        b.put(1, 3); // order 1  -> len 1
        // read_code_lengths: control bit 0 -> max_symbol = alphabet (280).
        b.put(0, 1);
        // Code-length symbols over the 280-symbol green alphabet. CL-symbol 1
        // (bit 0) sets length 1; CL-symbol 18 (bit 1 + 7 extra) repeats zeros.
        b.put(0, 1); // len[0] = 1
        b.put(1, 1);
        b.put(127, 7); // repeat 11 + 127 = 138 zeros -> symbols 1..=138
        b.put(1, 1);
        b.put(106, 7); // repeat 11 + 106 = 117 zeros -> symbols 139..=255
        b.put(0, 1); // len[256] = 1
        b.put(1, 1);
        b.put(12, 7); // repeat 11 + 12 = 23 zeros -> symbols 257..=279
    }

    /// A 2x1 image: pixel 0 a green-`0` literal, pixel 1 a green-`256` length-1
    /// back-reference (distance 1) copying pixel 0.
    fn lz77_literal_then_backref_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(1, 14); // width - 1 == 1 (width = 2)
        buf.put(0, 14); // height - 1 == 0
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(0, 1); // no transform
        buf.put(0, 1); // no color cache
        buf.put(0, 1); // no meta-huffman
        put_lz77_green_code(&mut buf);
        put_simple_code(&mut buf, 4); // red (even, so bit 16 is clear)
        put_simple_code(&mut buf, 8); // blue
        put_simple_code(&mut buf, 255); // alpha
        put_simple_code(&mut buf, 1); // dist: symbol 1 -> value 2 -> plane code 2 -> dist 1
        buf.put(0, 1); // pixel 0: green bit 0 -> symbol 0 (literal green = 0)
        buf.put(1, 1); // pixel 1: green bit 1 -> symbol 256 (length code)
        buf.finish()
    }

    #[test]
    fn literal_boundary_at_num_literal_codes() {
        // Green symbol 256 is the FIRST length code, not a literal. Mutating
        // `code < NUM_LITERAL_CODES` to `<=` treats 256 as a literal, so pixel 1
        // becomes `(256 << 8)`-tainted garbage instead of a back-reference copy of
        // pixel 0. Pixel 0 = A=255 R=4 G=0 B=8 = 0xff040008; pixel 1 copies it.
        let stream = lz77_literal_then_backref_stream();
        let out = decode(&stream).unwrap();
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.argb, vec![0xff04_0008, 0xff04_0008]);
    }

    /// A 1x1 image whose sole pixel is a green-`256` length-1 back-reference at
    /// output position 0 — distance 1 with no prior pixel, an invalid copy.
    fn lz77_backref_out_of_range_stream() -> Vec<u8> {
        let mut buf = BitBuf::default();
        buf.put(0x2f, 8);
        buf.put(0, 14); // width - 1 == 0
        buf.put(0, 14); // height - 1 == 0
        buf.put(0, 1); // alpha_is_used (advisory)
        buf.put(0, 3); // version
        buf.put(0, 1); // no transform
        buf.put(0, 1); // no color cache
        buf.put(0, 1); // no meta-huffman
        put_lz77_green_code(&mut buf);
        put_simple_code(&mut buf, 4); // red
        put_simple_code(&mut buf, 8); // blue
        put_simple_code(&mut buf, 255); // alpha
        put_simple_code(&mut buf, 1); // dist -> dist 1
        buf.put(1, 1); // pixel 0: green bit 1 -> symbol 256 (length code) at pos 0
        buf.finish()
    }

    #[test]
    fn rejects_backreference_distance_past_window() {
        // A back-reference at position 0 has `dist (1) > pos (0)` while
        // `pos + length (1) <= total (1)`. The guard is `dist > pos || pos +
        // length > total`; mutating `||` to `&&` lets this invalid copy through
        // (real returns Err; the mutant reads before the buffer start).
        let stream = lz77_backref_out_of_range_stream();
        assert!(decode(&stream).is_err());
    }

    #[test]
    fn decode_one_commits_exactly_one_literal_pixel() {
        // Drives `decode_one` directly on a 1x1 literal stream. Real code writes
        // the pixel, advances `pos` to 1, and returns Ok(true). Replacing the body
        // with `Ok(true)` (never advances `pos`) or mutating `st.pos += 1` to
        // `*= 1` (0 * 1 == 0) both leave `pos == 0` — an infinite loop in the full
        // driver, caught here as a fast, direct assertion.
        let stream = solid_stream(1, 1, 50, 100, 200, 255);
        let mut br = BitReader::new(&stream);
        let (_, parsed) = parse_top_level(&mut br).unwrap();
        let mut core = PixelCore::new(
            parsed.working_width,
            parsed.total,
            parsed.cache_bits,
            parsed.groups,
            parsed.entropy,
        );
        assert!(decode_one(&mut br, &mut core).unwrap());
        assert_eq!(core.pos, 1);
        assert_eq!(core.argb[0], 0xff32_64c8); // A=255 R=50 G=100 B=200
    }
}
