//! Shared, deterministic synthetic measurement corpus for `webpkit`.
//!
//! This crate renders a fixed matrix of synthetic `RGBA8` images that span the
//! VP8L compression-difficulty space (photographic, gradient, palette, noise,
//! tiled, solid). Generation is integer-only and portable: a `SplitMix64`
//! integer `PRNG` drives content, and every archetype is a pure function of a
//! seed derived from its [`Content`], edge length, and animation frame, so the
//! same bytes are produced on every platform.
//!
//! It is the single source of truth shared by the metrics ledger (`xtask`) and
//! the criterion benches (`webpkit-bench`), so recorded sizes describe exactly the
//! bytes the benches time. The crate is `no_std` (with `alloc`).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(
    clippy::float_arithmetic,
    reason = "the corpus must be bit-deterministic across platforms; floating-point \
              rounding is not portable. Every renderer is integer-only."
)]
#![allow(
    clippy::cast_possible_truncation,
    reason = "every narrowing `as` cast here targets one 8-bit RGBA lane or a bounded \
              index: values are masked to 8 bits or divided into `0..=255` before the \
              `as u8`, edge lengths are <= 512 so `edge as usize` cannot truncate, and \
              palette indices are taken modulo the table length before `as usize`"
)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// `SplitMix64` — a deterministic integer `PRNG` using only wrapping `u64`
/// arithmetic, so it produces the identical stream on every platform.
struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    const fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    const fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

/// Content archetypes spanning the VP8L compression-difficulty space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Content {
    /// Smoothed pseudo-random RGB (predictor-friendly), like a photograph.
    Photo,
    /// Two-axis linear ramp (transform-friendly, few residuals).
    Gradient,
    /// A 16-color indexed image (palette-transform friendly, `<= 256` colors).
    Palette,
    /// Independent per-channel entropy (near-incompressible stress).
    Noise,
    /// A small repeating tile (LZ77 / back-reference heavy).
    Tiled,
    /// A single constant color (trivially compressible).
    Solid,
}

impl Content {
    /// Every archetype, in the canonical stable order used by [`matrix`].
    pub const ALL: [Self; 6] = [
        Self::Photo,
        Self::Gradient,
        Self::Palette,
        Self::Noise,
        Self::Tiled,
        Self::Solid,
    ];

    /// The stable, lowercase, filesystem-safe name of this archetype.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Gradient => "gradient",
            Self::Palette => "palette",
            Self::Noise => "noise",
            Self::Tiled => "tiled",
            Self::Solid => "solid",
        }
    }

    /// A stable `0..6` index, mixed into the seed so distinct archetypes render
    /// distinct pseudo-random content. Never derived from iteration order.
    const fn index(self) -> u64 {
        match self {
            Self::Photo => 0,
            Self::Gradient => 1,
            Self::Palette => 2,
            Self::Noise => 3,
            Self::Tiled => 4,
            Self::Solid => 5,
        }
    }
}

/// Square edge lengths for the measurement matrix.
pub const SIZES: [u32; 3] = [64, 256, 512];

/// One synthetic image: raw row-major `RGBA8` plus its square dimension.
pub struct Sample {
    /// The archetype this image was rendered from.
    pub content: Content,
    /// The square edge length, in pixels (image is `edge` x `edge`).
    pub edge: u32,
    /// Row-major `RGBA8` pixels: `edge * edge * 4` bytes, alpha always 255.
    pub rgba: Vec<u8>,
}

/// A fixed 16-color palette. Distinct entries keep every archetype's color set
/// bounded and reproducible; [`Content::Tiled`] uses the first six, while
/// [`Content::Palette`] draws from all sixteen.
const PALETTE: [[u8; 4]; 16] = [
    [0x00, 0x00, 0x00, 255],
    [0xff, 0x00, 0x00, 255],
    [0x00, 0xff, 0x00, 255],
    [0x00, 0x00, 0xff, 255],
    [0xff, 0xff, 0x00, 255],
    [0xff, 0x00, 0xff, 255],
    [0x00, 0xff, 0xff, 255],
    [0xff, 0xff, 0xff, 255],
    [0x7f, 0x00, 0x00, 255],
    [0x00, 0x7f, 0x00, 255],
    [0x00, 0x00, 0x7f, 255],
    [0x7f, 0x7f, 0x00, 255],
    [0x7f, 0x00, 0x7f, 255],
    [0x00, 0x7f, 0x7f, 255],
    [0x7f, 0x7f, 0x7f, 255],
    [0xc0, 0x60, 0x30, 255],
];

/// Map a color index to its `RGBA8` entry (wrapping into the 16-color table).
const fn band_color(index: usize) -> [u8; 4] {
    PALETTE[index % PALETTE.len()]
}

/// Byte length of an `edge` x `edge` `RGBA8` buffer.
const fn rgba_len(edge: u32) -> usize {
    (edge as usize) * (edge as usize) * 4
}

/// Truncate `v` to its low 8 bits.
const fn lo(v: u32) -> u8 {
    (v & 0xff) as u8
}

/// Deterministically render one archetype at `edge` x `edge`.
#[must_use]
pub fn render(content: Content, edge: u32) -> Sample {
    render_frame(content, edge, 0)
}

/// One animation frame: like [`render`] but the seed is perturbed by `frame` so
/// pseudo-random archetypes shift frame-to-frame. `frame == 0` is identical to
/// [`render`].
#[must_use]
pub fn render_frame(content: Content, edge: u32, frame: u32) -> Sample {
    let seed =
        0x5AFE_5EED ^ (content.index() << 40) ^ (u64::from(edge) << 8) ^ (u64::from(frame) << 20);
    let rgba = match content {
        Content::Photo => render_photo(edge, seed),
        Content::Gradient => render_gradient(edge),
        Content::Palette => render_palette(edge, seed),
        Content::Noise => render_noise(edge, seed),
        Content::Tiled => render_tiled(edge),
        Content::Solid => render_solid(edge),
    };
    Sample {
        content,
        edge,
        rgba,
    }
}

/// The full still matrix ([`Content::ALL`] x [`SIZES`]), in stable order.
#[must_use]
pub fn matrix() -> Vec<Sample> {
    let mut out = Vec::with_capacity(Content::ALL.len() * SIZES.len());
    for &content in &Content::ALL {
        for &edge in &SIZES {
            out.push(render(content, edge));
        }
    }
    out
}

/// Stable, filesystem-safe, lexically-sortable id: `"{name}_{edge:03}"`, e.g.
/// `"gradient_064"`.
#[must_use]
pub fn sample_id(content: Content, edge: u32) -> String {
    format!("{}_{edge:03}", content.name())
}

/// Two-axis linear ramp: `[lo(4x), lo(4y), lo(2(x + y)), 255]`.
fn render_gradient(edge: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(rgba_len(edge));
    for y in 0..edge {
        for x in 0..edge {
            rgba.push(lo(4 * x));
            rgba.push(lo(4 * y));
            rgba.push(lo(2 * (x + y)));
            rgba.push(255);
        }
    }
    rgba
}

/// A single constant color.
fn render_solid(edge: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(rgba_len(edge));
    for _ in 0..rgba_len(edge) / 4 {
        rgba.extend_from_slice(&[37, 130, 91, 255]);
    }
    rgba
}

/// A small repeating 8x8 tile drawn from the first six palette colors.
fn render_tiled(edge: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(rgba_len(edge));
    for y in 0..edge {
        for x in 0..edge {
            let band = ((x / 8 + y / 8) % 6) as usize;
            rgba.extend_from_slice(&band_color(band));
        }
    }
    rgba
}

/// 16-color indexed content: each pixel picks a palette entry by a `PRNG` index.
fn render_palette(edge: u32, seed: u64) -> Vec<u8> {
    let mut prng = SplitMix64::new(seed);
    let mut rgba = Vec::with_capacity(rgba_len(edge));
    for _ in 0..rgba_len(edge) / 4 {
        let band = (prng.next_u64() % 16) as usize;
        rgba.extend_from_slice(&band_color(band));
    }
    rgba
}

/// Independent per-channel entropy; alpha is held at 255.
fn render_noise(edge: u32, seed: u64) -> Vec<u8> {
    let mut prng = SplitMix64::new(seed);
    let mut rgba = Vec::with_capacity(rgba_len(edge));
    for _ in 0..rgba_len(edge) / 4 {
        rgba.push(prng.next_u8());
        rgba.push(prng.next_u8());
        rgba.push(prng.next_u8());
        rgba.push(255);
    }
    rgba
}

/// Pseudo-random RGB smoothed by a 3x3 integer box blur (edge-clamped, `/9`),
/// yielding predictor-friendly photographic content. Alpha is held at 255.
fn render_photo(edge: u32, seed: u64) -> Vec<u8> {
    let mut prng = SplitMix64::new(seed);
    let side = edge as usize;
    let pixels = side * side;

    // Independent per-channel noise source (RGB only).
    let mut rgb = Vec::with_capacity(pixels * 3);
    for _ in 0..pixels {
        rgb.push(prng.next_u8());
        rgb.push(prng.next_u8());
        rgb.push(prng.next_u8());
    }

    // 3x3 box blur with border clamping: each output samples exactly nine
    // neighbors (clamped coordinates), so the integer `/ 9` is exact.
    let mut rgba = Vec::with_capacity(pixels * 4);
    for y in 0..side {
        let ys = [y.saturating_sub(1), y, (y + 1).min(side - 1)];
        for x in 0..side {
            let xs = [x.saturating_sub(1), x, (x + 1).min(side - 1)];
            for ch in 0..3 {
                let mut sum: u32 = 0;
                for &ny in &ys {
                    for &nx in &xs {
                        sum += u32::from(rgb[(ny * side + nx) * 3 + ch]);
                    }
                }
                rgba.push((sum / 9) as u8);
            }
            rgba.push(255);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::{Content, SIZES, matrix, render, render_frame, sample_id};
    use alloc::string::String;
    use alloc::vec::Vec;

    /// FNV-1a-64 over `data`, pinning cross-platform byte determinism.
    fn fnv1a64(data: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    #[test]
    fn render_is_deterministic() {
        for &content in &Content::ALL {
            let first = render(content, 64);
            let second = render(content, 64);
            assert_eq!(
                first.rgba,
                second.rgba,
                "{} not deterministic",
                content.name()
            );
        }
    }

    #[test]
    fn render_frame_zero_equals_render() {
        for &content in &Content::ALL {
            assert_eq!(render(content, 64).rgba, render_frame(content, 64, 0).rgba);
        }
    }

    #[test]
    fn render_frame_perturbs_prng_archetypes() {
        // Archetypes driven by the PRNG must differ between frames.
        for content in [Content::Photo, Content::Palette, Content::Noise] {
            let frame0 = render_frame(content, 64, 0);
            let frame1 = render_frame(content, 64, 1);
            assert_ne!(frame0.rgba, frame1.rgba, "{} did not shift", content.name());
        }
    }

    #[test]
    fn sample_len_matches_dims() {
        for &edge in &[1u32, 2, 7, 64] {
            for &content in &Content::ALL {
                let sample = render(content, edge);
                let expected = (edge as usize) * (edge as usize) * 4;
                assert_eq!(sample.rgba.len(), expected, "{} @ {edge}", content.name());
                // Alpha (every 4th byte) is always 255.
                for pixel in sample.rgba.chunks_exact(4) {
                    assert_eq!(pixel[3], 255, "{} @ {edge} alpha", content.name());
                }
            }
        }
    }

    #[test]
    fn matrix_ids_unique_and_sorted() {
        let samples = matrix();
        assert_eq!(samples.len(), Content::ALL.len() * SIZES.len());

        // Emission order is Content::ALL x SIZES (content-major), stable.
        let mut iter = samples.iter();
        for &content in &Content::ALL {
            for &edge in &SIZES {
                let sample = iter.next().unwrap();
                assert_eq!(sample.content, content);
                assert_eq!(sample.edge, edge);
            }
        }

        // Every id is unique and lexically sortable (zero-padded sizes keep
        // 064 < 256 < 512): sorting yields a strictly increasing sequence.
        let ids: Vec<String> = samples
            .iter()
            .map(|s| sample_id(s.content, s.edge))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted.len(), ids.len());
        for window in sorted.windows(2) {
            assert!(window[0] < window[1], "ids not unique/sortable: {sorted:?}");
        }
    }

    #[test]
    fn render_matches_golden() {
        // FNV-1a-64 of render(content, 64).rgba, in Content::ALL order.
        // Captured deterministically; pins cross-platform byte reproduction.
        const GOLDEN: [u64; 6] = [
            0xfe98_fa05_45b8_f734, // photo
            0xba6a_ec00_8c57_e525, // gradient
            0x0cfa_9f55_ea3c_3254, // palette
            0x4756_86b6_b85e_860a, // noise
            0xc294_514f_7fc9_0625, // tiled
            0x9559_8490_973d_8325, // solid
        ];
        let actual: Vec<u64> = Content::ALL
            .iter()
            .map(|&content| fnv1a64(&render(content, 64).rgba))
            .collect();
        assert_eq!(actual.as_slice(), GOLDEN.as_slice());
    }
}
