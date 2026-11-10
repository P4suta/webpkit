//! Synthetic campaign gates for shapes the square benchmark matrix does not cover.
#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    reason = "test fixtures use fixed, valid dimensions and fail immediately on setup errors"
)]

use webpkit::lossless::{Effort, EncoderConfig, ImageRef, decode_rgba, encode};
use webpkit::{Dimensions, Image, PixelLayout, decode, encode_lossy_rgba};

fn rgba(width: u32, height: u32, alpha: bool) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(usize::try_from(u64::from(width) * u64::from(height) * 4).unwrap());
    for y in 0..height {
        for x in 0..width {
            let a = if alpha {
                (x.wrapping_mul(17) + y.wrapping_mul(29)).to_le_bytes()[0]
            } else {
                255
            };
            out.extend_from_slice(&[
                x.wrapping_mul(11).to_le_bytes()[0],
                y.wrapping_mul(7).to_le_bytes()[0],
                (x ^ y).wrapping_mul(13).to_le_bytes()[0],
                a,
            ]);
        }
    }
    out
}

#[test]
fn odd_portrait_landscape_and_alpha_round_trip_losslessly() {
    for &(width, height, alpha) in &[(17, 31, true), (31, 17, false), (1, 37, true)] {
        let dims = Dimensions::new(width, height).unwrap();
        let pixels = rgba(width, height, alpha);
        let image = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let config = EncoderConfig::new().with_effort(Effort::level(0));
        let encoded = encode(image, &config).unwrap();
        let (decoded_dims, decoded) = decode_rgba(&encoded).unwrap();
        assert_eq!(decoded_dims, dims);
        assert_eq!(decoded, pixels);
    }
}

#[test]
fn odd_portrait_landscape_and_alpha_decode_through_lossy_container() {
    for &(width, height, alpha) in &[(17, 31, true), (31, 17, false), (1, 37, true)] {
        let pixels = rgba(width, height, alpha);
        let encoded = encode_lossy_rgba(width, height, &pixels, 75).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(
            decoded.dimensions(),
            Dimensions::new(width, height).unwrap()
        );
        assert_eq!(decoded.layout(), PixelLayout::Rgba8);
        assert_eq!(decoded.as_bytes().len(), pixels.len());
        for (source, output) in pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(decoded.as_bytes().as_chunks::<4>().0.iter())
        {
            assert_eq!(
                output[3], source[3],
                "the lossy container must preserve alpha semantics"
            );
        }
    }
}

#[test]
fn large_portrait_and_landscape_scale_to_the_requested_longest_edge() {
    for &(width, height, target_width, target_height) in
        &[(2049, 1025, 1024, 513), (1025, 2049, 513, 1024)]
    {
        let dims = Dimensions::new(width, height).unwrap();
        let target = if width >= height {
            dims.scaled(1024, 0).unwrap()
        } else {
            dims.scaled(0, 1024).unwrap()
        };
        assert_eq!(
            target,
            Dimensions::new(target_width, target_height).unwrap()
        );

        let image = Image::new(dims, PixelLayout::Rgba8, rgba(width, height, true)).unwrap();
        let resized = image.resize_exact(target);
        assert_eq!(resized.dimensions(), target);
        assert!(resized.has_alpha());
    }
}
