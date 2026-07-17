//! Byte-exact differential oracle for the core geometry rescaler
//! (`crate::geometry`): cross-check [`Image::resize`] against libwebp's
//! `WebPPictureRescale` (the ARGB path `cwebp -resize` runs), in-process.
//!
//! Enabled only with `--features oracle` (which links `libwebp-sys` and the vendored
//! reference); never part of a normal build.
//!
//! The resize contract is **byte-exact**: our fixed-point port of `WebPRescaler`
//! reproduces libwebp's output pixel-for-pixel, including the alpha black-matting
//! that `WebPPictureRescale` applies before interpolating. The alpha channel is
//! generated *independently* of the RGB so a wrong premultiply lane cannot pass
//! silently, and the size matrix covers upscaling, downscaling, non-integer ratios,
//! and single-axis (aspect) targets.

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

/// Rescale `rgba` (`width * height * 4` bytes) to `dst_w x dst_h` with libwebp's
/// `WebPPictureRescale` in ARGB mode, returning the result as RGBA8 bytes.
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

/// A synthetic RGBA image whose alpha varies on a different gradient than the RGB, so
/// the premultiply/rescale/un-premultiply lanes are all exercised distinctly.
fn synthetic(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 7 + y * 3) as u8;
            let g = (x * 3 + y * 11 + 40) as u8;
            let b = (x * 13 + y * 5 + 90) as u8;
            // Alpha sweeps independently, including 0 and 255 corners.
            let a = ((x + y) * 17 % 256) as u8;
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    rgba
}

#[test]
fn resize_matches_libwebp_byte_for_byte() {
    // (src_w, src_h, dst_w, dst_h): upscale, downscale, non-integer, and aspect rows.
    let cases = [
        (16, 16, 8, 8),   // integer 2x downscale
        (16, 16, 32, 32), // integer 2x upscale
        (17, 13, 5, 9),   // odd downscale, non-integer ratios both axes
        (10, 10, 23, 7),  // mixed up/down
        (9, 9, 9, 9),     // 1:1 (still routes through the matting path)
        (7, 20, 20, 7),   // transpose-ish aspect swap
        (1, 8, 4, 3),     // 1-wide source expansion
        (32, 1, 5, 1),    // 1-tall source shrink
    ];
    let mut exercised_alpha = false;
    for (sw, sh, dw, dh) in cases {
        let rgba = synthetic(sw, sh);
        exercised_alpha |= rgba.chunks_exact(4).any(|p| p[3] != 0xff);

        let img = Image::new(
            Dimensions::new(sw, sh).unwrap(),
            PixelLayout::Rgba8,
            rgba.clone(),
        )
        .unwrap();
        let ours = img.resize(Dimensions::new(dw, dh).unwrap());
        let theirs = libwebp_rescale_rgba(&rgba, sw, sh, dw, dh);

        assert_eq!(
            ours.as_bytes(),
            theirs.as_slice(),
            "resize {sw}x{sh} -> {dw}x{dh} diverged from libwebp WebPPictureRescale"
        );
    }
    assert!(exercised_alpha, "matrix must include non-opaque alpha");
}
