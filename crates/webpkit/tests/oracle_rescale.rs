//! Byte-exact differential oracle for the core geometry rescaler (`crate::geometry`): cross-check [`Image::resize`] and [`Image::resize_exact`] against libwebp's `WebPPictureRescale` (the ARGB path `cwebp -resize` runs),
//! in-process.
//!
//! Enabled only with `--features oracle` (which links `libwebp-sys` and the vendored reference); never part of a normal build.
//!
//! The resize contract is **byte-exact** in both modes.
//! [`Image::resize`] must equal `WebPPictureRescale`, including the alpha black-matting it applies before interpolating.
//! [`Image::resize_exact`] must equal what `cwebp -resize -exact` builds: the color channels of an opaque copy's rescale, with alpha from the ordinary rescale.
//! Alpha is generated *independently* of the RGB so a wrong premultiply lane cannot pass silently, four content patterns cover opaque,
//! hard-edged, and saturated-corner alpha, and the size matrix sweeps every (src, dst) pair in `1..=8` on both axes plus large, prime, and extreme-ratio pairs — so upscaling, downscaling, non-integer ratios, and single-axis targets are all pinned.

#![cfg(feature = "oracle")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "test-only differential oracle: unwrap/panic are the accepted style for \
              provably-infallible reference-library successes, and the synthetic-pixel \
              generator truncates to u8 on purpose"
)]

use webpkit::{Dimensions, Image, PixelLayout};

/// Rescale `rgba` (`width * height * 4` bytes) to `dst_w x dst_h` with libwebp's `WebPPictureRescale` in ARGB mode, returning the result as RGBA8 bytes.
fn libwebp_rescale_rgba(rgba: &[u8], width: u32, height: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut picture = libwebp_sys::WebPPicture::new().unwrap();
    picture.use_argb = 1; // ARGB path: import stores 0xAARRGGBB directly, no YUV.
    picture.width = i32::try_from(width).unwrap();
    picture.height = i32::try_from(height).unwrap();
    let stride = i32::try_from(width * 4).unwrap();
    // SAFETY: `rgba` holds `width*height*4` bytes at `stride`; picture dims match.
    assert!(
        unsafe { libwebp_sys::WebPPictureImportRGBA(&raw mut picture, rgba.as_ptr(), stride) } != 0,
        "WebPPictureImportRGBA failed"
    );
    // SAFETY: `picture` is a valid, allocated ARGB picture from the import above.
    assert!(
        unsafe {
            libwebp_sys::WebPPictureRescale(
                &raw mut picture,
                i32::try_from(dst_w).unwrap(),
                i32::try_from(dst_h).unwrap(),
            )
        } != 0,
        "WebPPictureRescale failed"
    );

    let stride_px = picture.argb_stride as usize;
    let mut out = Vec::with_capacity((dst_w * dst_h * 4) as usize);
    for y in 0..dst_h as usize {
        for x in 0..dst_w as usize {
            // SAFETY: argb points at `argb_stride * dst_h` u32s; (x, y) is in range.
            let px = unsafe { *picture.argb.add(y * stride_px + x) };
            let [b, g, r, a] = px.to_le_bytes();
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    // SAFETY: `picture` owns its argb buffer; free it once.
    unsafe { libwebp_sys::WebPPictureFree(&raw mut picture) };
    out
}

/// The reference for `cwebp -resize -exact`: libwebp rescales a copy whose alpha has been forced opaque to get the un-weighted color, and keeps the alpha of the ordinary (matted) rescale.
fn libwebp_rescale_rgba_exact(
    rgba: &[u8],
    width: u32,
    height: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let matted = libwebp_rescale_rgba(rgba, width, height, dst_w, dst_h);
    let mut opaque = rgba.to_vec();
    for px in opaque.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    let color = libwebp_rescale_rgba(&opaque, width, height, dst_w, dst_h);
    color
        .chunks_exact(4)
        .zip(matted.chunks_exact(4))
        .flat_map(|(c, m)| [c[0], c[1], c[2], m[3]])
        .collect()
}

/// Four synthetic RGBA patterns.
/// Alpha always varies on a different law than the RGB,
/// so the premultiply/rescale/un-premultiply lanes are exercised distinctly.
fn content(kind: u32, width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    let mut state: u32 = 0x1234_5678 ^ (width << 8) ^ height;
    for y in 0..height {
        for x in 0..width {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let px = match kind {
                // Smooth color, alpha sweeping independently through 0 and 255.
                0 => [
                    (x * 7 + y * 3) as u8,
                    (x * 3 + y * 11 + 40) as u8,
                    (x * 13 + y * 5 + 90) as u8,
                    ((x + y) * 17 % 256) as u8,
                ],
                // Fully opaque: the matting must be the identity here.
                1 => [
                    (x * 5 + y * 9) as u8,
                    (x * 17 + y) as u8,
                    (x + y * 23) as u8,
                    0xff,
                ],
                // Hard 0/255 alpha edges over saturated color: the widest matting gap.
                2 => [
                    if (x / 2) % 2 == 0 { 255 } else { 0 },
                    if (y / 3) % 2 == 0 { 255 } else { 0 },
                    if (x ^ y) % 4 < 2 { 200 } else { 10 },
                    if (x + y) % 2 == 0 { 255 } else { 0 },
                ],
                // Pseudo-random, hitting the alpha 1 and 254 rounding corners.
                _ => [
                    (state >> 24) as u8,
                    (state >> 16) as u8,
                    (state >> 8) as u8,
                    match state % 8 {
                        0 => 0,
                        1 => 1,
                        2 => 254,
                        3 => 255,
                        v => (v * 31) as u8,
                    },
                ],
            };
            rgba.extend_from_slice(&px);
        }
    }
    rgba
}

/// Assert both resize modes against libwebp for one content/size case, returning whether the case carried any non-opaque pixel.
fn check(kind: u32, sw: u32, sh: u32, dw: u32, dh: u32) -> bool {
    let rgba = content(kind, sw, sh);
    let img = Image::new(
        Dimensions::new(sw, sh).unwrap(),
        PixelLayout::Rgba8,
        rgba.clone(),
    )
    .unwrap();
    let target = Dimensions::new(dw, dh).unwrap();

    assert_eq!(
        img.resize(target).as_bytes(),
        libwebp_rescale_rgba(&rgba, sw, sh, dw, dh).as_slice(),
        "resize k{kind} {sw}x{sh} -> {dw}x{dh} diverged from WebPPictureRescale"
    );
    assert_eq!(
        img.resize_exact(target).as_bytes(),
        libwebp_rescale_rgba_exact(&rgba, sw, sh, dw, dh).as_slice(),
        "resize_exact k{kind} {sw}x{sh} -> {dw}x{dh} diverged from `cwebp -resize -exact`"
    );
    rgba.chunks_exact(4).any(|p| p[3] != 0xff)
}

#[test]
fn resize_matches_libwebp_over_the_small_size_matrix() {
    // Every (src, dst) pair in 1..=8 on both axes: 1:1, both expansions, both shrinks, and every mixed/odd ratio a tiny plane can take.
    let mut exercised_alpha = false;
    for kind in 0..4 {
        for sw in 1..=8 {
            for sh in 1..=8 {
                for dw in 1..=8 {
                    for dh in 1..=8 {
                        exercised_alpha |= check(kind, sw, sh, dw, dh);
                    }
                }
            }
        }
    }
    assert!(exercised_alpha, "matrix must include non-opaque alpha");
}

#[test]
fn resize_matches_libwebp_at_scale_and_extremes() {
    // Photographic sizes, primes, single-row/column planes, and ratios extreme enough to hit the `fxy_scale == 0` special case and the 16384 dimension ceiling.
    let cases = [
        (64, 64, 32, 32),
        (64, 64, 63, 65),
        (64, 64, 1, 1),
        (64, 64, 127, 3),
        (64, 64, 256, 256),
        (100, 100, 33, 77),
        (17, 13, 200, 190),
        (97, 61, 300, 7),
        (223, 149, 97, 211),
        (640, 426, 320, 213),
        (640, 426, 641, 425),
        (1, 1, 512, 384),
        (512, 384, 1, 1),
        (997, 3, 13, 61),
        (3, 997, 61, 13),
        (256, 256, 255, 255),
        (255, 255, 256, 256),
        (16384, 1, 37, 1),
        (1, 16384, 1, 37),
    ];
    for kind in 0..4 {
        for (sw, sh, dw, dh) in cases {
            check(kind, sw, sh, dw, dh);
        }
    }
}

#[test]
fn the_two_modes_agree_exactly_when_the_source_is_opaque() {
    // Matting is the identity on an opaque plane, so `-exact` can only change pixels that carry transparency — a regression in either mode breaks this equality.
    for (sw, sh, dw, dh) in [(16, 16, 7, 23), (9, 5, 5, 9), (33, 33, 64, 64)] {
        let rgba = content(1, sw, sh);
        let img = Image::new(
            Dimensions::new(sw, sh).unwrap(),
            PixelLayout::Rgba8,
            rgba.clone(),
        )
        .unwrap();
        let target = Dimensions::new(dw, dh).unwrap();
        assert_eq!(
            img.resize(target).as_bytes(),
            img.resize_exact(target).as_bytes(),
            "opaque {sw}x{sh} -> {dw}x{dh}: the two modes must coincide"
        );
    }
}

#[test]
fn exact_mode_keeps_the_color_under_transparent_pixels() {
    // A fully transparent plane with a constant color: matting collapses it to black,
    // the exact mode carries the color through untouched.
    let mut rgba = Vec::new();
    for _ in 0..(12 * 12) {
        rgba.extend_from_slice(&[200, 90, 30, 0]);
    }
    let img = Image::new(Dimensions::new(12, 12).unwrap(), PixelLayout::Rgba8, rgba).unwrap();
    let target = Dimensions::new(5, 7).unwrap();

    assert!(
        img.resize(target)
            .as_bytes()
            .chunks_exact(4)
            .all(|p| p == [0, 0, 0, 0]),
        "matted resize must black out fully transparent color"
    );
    assert!(
        img.resize_exact(target)
            .as_bytes()
            .chunks_exact(4)
            .all(|p| p == [200, 90, 30, 0]),
        "exact resize must preserve fully transparent color"
    );
}
