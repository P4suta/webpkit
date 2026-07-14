//! Tool-free encode verification: the public lossy encoder must produce output
//! that decodes back to the source within a quality floor, deterministically.
//!
//! Self-consistency (decode-of-our-output equals our reconstruction) is pinned in
//! `src/frame.rs`; here we measure fidelity against the *source* — a check the
//! in-crate tests cannot make because the crate forbids floating point, which the
//! PSNR metric needs. This is a separate integration crate, so it may use `f64`.

use webpkit::lossy::{
    Dimensions, Effort, Error, ImageRef, IncrementalDecoder, LossyConfig, PixelLayout, decode,
    encode, encode_vp8,
};
use webpkit_lossy_proptest::psnr_rgb;

/// A byte from a pattern value, wrapping into `0..=255` (no lossy cast).
fn byte(v: u32) -> u8 {
    u8::try_from(v & 0xff).unwrap_or(0)
}

/// Build a `width`×`height` RGBA image from a per-pixel function.
fn image(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let mut buf = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = f(x, y);
            buf.extend_from_slice(&[r, g, b, 0xff]);
        }
    }
    buf
}

/// Encode `rgba` at `quality` with effort `method` and decode it back to RGBA bytes.
fn round_trip_method(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: u8,
    method: Effort,
) -> Result<Vec<u8>, Error> {
    let dims = Dimensions::new(width, height)?;
    let img = ImageRef::new(dims, PixelLayout::Rgba8, rgba)?;
    let cfg = LossyConfig::new().with_quality(quality).with_effort(method);
    let (_dims, payload) = encode_vp8(img, &cfg)?;
    Ok(decode(&payload)?.into_pixels())
}

/// Encode `rgba` at `quality` (default effort) and decode it back to RGBA bytes.
fn round_trip(rgba: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, Error> {
    round_trip_method(rgba, width, height, quality, Effort::Balanced)
}

#[test]
fn smooth_gradient_meets_quality_floors() {
    // A smooth gradient is the friendliest content for a DC-prediction encoder:
    // fidelity should rise with quality. Floors are conservative (the MVP uses no
    // mode search and round-to-nearest quantization).
    let (w, h) = (64, 64);
    let rgba = image(w, h, |x, y| [byte(x * 4), byte(y * 4), byte((x + y) * 2)]);
    for &(quality, floor) in &[(50u8, 26.0f64), (75, 30.0), (95, 34.0)] {
        let out = round_trip(&rgba, w, h, quality).unwrap();
        let psnr = psnr_rgb(&rgba, &out);
        assert!(
            psnr >= floor,
            "quality {quality}: PSNR {psnr:.2} dB below floor {floor}"
        );
    }
}

#[test]
fn fidelity_is_monotonic_in_quality() {
    // Higher quality must not reduce fidelity on a fixed image.
    let (w, h) = (48, 40);
    let rgba = image(w, h, |x, y| [byte(x * 5), byte(y * 6), 100]);
    let low = psnr_rgb(&rgba, &round_trip(&rgba, w, h, 30).unwrap());
    let high = psnr_rgb(&rgba, &round_trip(&rgba, w, h, 95).unwrap());
    assert!(
        high >= low - 0.01,
        "PSNR dropped with quality: {low:.2} -> {high:.2}"
    );
}

#[test]
fn solid_color_is_near_lossless_at_high_quality() {
    // A flat color has a DC-only residual; at high quality it should round-trip
    // almost perfectly (bounded only by the 4:2:0 + color-conversion rounding).
    let (w, h) = (32, 32);
    let rgba = image(w, h, |_, _| [90, 155, 210]);
    let out = round_trip(&rgba, w, h, 95).unwrap();
    let psnr = psnr_rgb(&rgba, &out);
    assert!(psnr >= 40.0, "flat color PSNR {psnr:.2} dB too low");
}

#[test]
fn encoding_is_deterministic_across_qualities() {
    let (w, h) = (40, 24);
    let rgba = image(w, h, |x, y| [byte(x * 6), byte(y * 10), byte(x ^ y)]);
    let dims = Dimensions::new(w, h).unwrap();
    for quality in [10u8, 50, 90] {
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let cfg = LossyConfig::new().with_quality(quality);
        let a = encode_vp8(img, &cfg).unwrap().1;
        let b = encode_vp8(img, &cfg).unwrap().1;
        assert_eq!(a, b, "quality {quality} is non-deterministic");
    }
}

#[test]
fn odd_and_tiny_dimensions_round_trip() {
    // Partial edge macroblocks and 1-pixel sides must encode and decode cleanly.
    for &(w, h) in &[(1u32, 1u32), (3, 7), (17, 13), (16, 1), (1, 16)] {
        let rgba = image(w, h, |x, y| [byte(x * 20), byte(y * 30), 128]);
        let out = round_trip(&rgba, w, h, 75).unwrap();
        let expected = usize::try_from(w * h * 4).unwrap_or(0);
        assert_eq!(out.len(), expected, "{w}x{h} pixel count");
    }
}

#[test]
fn every_method_round_trips_with_a_fidelity_floor() {
    // Fast, Balanced and Best each produce a decodable frame of the right size that
    // reproduces the source above a conservative floor (Fast, DC-only, is loosest).
    let (w, h) = (48, 32);
    let rgba = image(w, h, |x, y| [byte(x * 5), byte(y * 7), byte((x + y) * 3)]);
    for method in [Effort::Fast, Effort::Balanced, Effort::Best] {
        let out = round_trip_method(&rgba, w, h, 75, method).unwrap();
        assert_eq!(
            out.len(),
            usize::try_from(w * h * 4).unwrap(),
            "{method:?} pixel count"
        );
        let psnr = psnr_rgb(&rgba, &out);
        assert!(psnr >= 22.0, "{method:?}: PSNR {psnr:.2} dB below floor");
    }
}

#[test]
fn mode_search_and_trellis_win_rate_distortion_on_a_gradient() {
    // On a smooth gradient the intra-mode search fits the content far better than
    // Fast's fixed DC prediction, and trellis quantization then drops the residual's
    // rate-inefficient tail. Together they are a decisive rate-distortion win:
    // Balanced/Best code the frame much smaller than Fast while holding fidelity to
    // within ~1 dB. (Before trellis, mode search was a pure fidelity win; trellis
    // turns the surplus into a size win instead — hence the size assertion here.)
    let (w, h) = (64, 64);
    let rgba = image(w, h, |x, y| [byte(x * 4), byte(y * 4), byte((x + y) * 2)]);
    let dims = Dimensions::new(w, h).unwrap();
    let encode_len = |method| {
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        encode_vp8(
            img,
            &LossyConfig::new().with_quality(75).with_effort(method),
        )
        .unwrap()
        .1
        .len()
    };
    let fast = psnr_rgb(
        &rgba,
        &round_trip_method(&rgba, w, h, 75, Effort::Fast).unwrap(),
    );
    let balanced = psnr_rgb(
        &rgba,
        &round_trip_method(&rgba, w, h, 75, Effort::Balanced).unwrap(),
    );
    let best = psnr_rgb(
        &rgba,
        &round_trip_method(&rgba, w, h, 75, Effort::Best).unwrap(),
    );
    let (fast_len, balanced_len, best_len) = (
        encode_len(Effort::Fast),
        encode_len(Effort::Balanced),
        encode_len(Effort::Best),
    );
    // A generous fidelity guard against gross breakage; the tight bound is the size
    // win below (Best trades a little more fidelity than Balanced via its i4x4 search).
    assert!(
        balanced >= fast - 2.0,
        "Balanced PSNR {balanced:.2} fell far below Fast {fast:.2}"
    );
    assert!(
        best >= fast - 2.0,
        "Best PSNR {best:.2} fell far below Fast {fast:.2}"
    );
    assert!(
        balanced_len < fast_len,
        "Balanced {balanced_len} did not code smaller than Fast {fast_len}"
    );
    assert!(
        best_len < fast_len,
        "Best {best_len} did not code smaller than Fast {fast_len}"
    );
}

#[test]
fn flat_image_skip_shrinks_balanced_below_fast() {
    // A flat color makes almost every macroblock skippable. Balanced codes those
    // with a one-bit skip flag and no residual tokens, while Fast never skips (it
    // emits an all-zero token tree for each block). The skip-using Balanced frame
    // must therefore be strictly smaller than the Fast frame of the same image.
    let (w, h) = (128u32, 128u32);
    let rgba = image(w, h, |_, _| [120, 90, 200]);
    let dims = Dimensions::new(w, h).unwrap();
    let payload_len = |method| {
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let cfg = LossyConfig::new().with_quality(75).with_effort(method);
        encode_vp8(img, &cfg).unwrap().1.len()
    };
    let fast = payload_len(Effort::Fast);
    let balanced = payload_len(Effort::Balanced);
    assert!(
        balanced < fast,
        "per-macroblock skip did not shrink the frame: balanced {balanced} !< fast {fast}"
    );
    // The skip-using Balanced frame must still round-trip to the right size.
    let out = round_trip_method(&rgba, w, h, 75, Effort::Balanced).unwrap();
    assert_eq!(out.len(), usize::try_from(w * h * 4).unwrap());
}

#[test]
fn filtered_output_streams_identically_to_one_shot() {
    // A blocky, multi-macroblock-row image encoded with Balanced (loop filter on):
    // decoding OUR filtered stream row-by-row through the IncrementalDecoder — whose
    // deferred, one-row-behind rolling filter must reproduce the one-shot whole-frame
    // filter pass — must yield pixels byte-identical to a one-shot decode, at every
    // push granularity (including one byte at a time, the suspend/resume worst case).
    let (w, h) = (48u32, 48u32); // 3x3 macroblocks -> several streamed row bursts
    let rgba = image(w, h, |x, y| {
        if (x / 4 + y / 4) % 2 == 0 {
            [20, 20, 20]
        } else {
            [220, 220, 220]
        }
    });
    let dims = Dimensions::new(w, h).unwrap();
    let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
    let cfg = LossyConfig::new()
        .with_quality(50)
        .with_effort(Effort::Balanced);
    // The full container (what IncrementalDecoder consumes) and the raw payload (what
    // one-shot decode consumes) wrap the same, filtered VP8 stream.
    let webp = encode(img, &cfg).unwrap();
    let (_dims, payload) = encode_vp8(img, &cfg).unwrap();
    let one_shot = decode(&payload).unwrap().into_pixels();

    for &chunk in &[1usize, 5, 33] {
        let mut dec = IncrementalDecoder::new();
        let mut off = 0;
        while off < webp.len() {
            let end = (off + chunk).min(webp.len());
            dec.push(&webp[off..end]).unwrap();
            off = end;
        }
        let streamed = dec.into_image().unwrap().into_pixels();
        assert_eq!(
            streamed, one_shot,
            "chunk={chunk}: streamed differs from one-shot"
        );
    }
}

#[test]
fn best_improves_rate_distortion_on_detailed_content() {
    // Detailed vertical stripes: Best's intra-4×4 search fits the local structure far
    // better than a flat 16×16 predictor, so at the same quality it codes the frame
    // markedly smaller than Balanced for only a small fidelity trade — a favorable
    // rate-distortion move (i4x4 is chosen only when its cost beats the 16×16
    // candidate). The hard correctness guarantees (self-consistency, libwebp Level-A
    // byte-exactness) are pinned in `src/frame.rs` and the oracle; here we assert the
    // size win and a bounded PSNR.
    let (w, h) = (64, 64);
    let rgba = image(w, h, |x, _| {
        if (x / 2) % 2 == 0 {
            [220, 220, 220]
        } else {
            [20, 20, 20]
        }
    });
    let dims = Dimensions::new(w, h).unwrap();
    let size = |m| {
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        encode_vp8(img, &LossyConfig::new().with_quality(50).with_effort(m))
            .unwrap()
            .1
            .len()
    };
    let bal_size = size(Effort::Balanced);
    let best_size = size(Effort::Best);
    let bal_psnr = psnr_rgb(
        &rgba,
        &round_trip_method(&rgba, w, h, 50, Effort::Balanced).unwrap(),
    );
    let best_psnr = psnr_rgb(
        &rgba,
        &round_trip_method(&rgba, w, h, 50, Effort::Best).unwrap(),
    );
    assert!(
        best_size < bal_size,
        "Best size {best_size} should beat Balanced {bal_size} on detailed content"
    );
    assert!(
        best_psnr >= bal_psnr - 2.0,
        "Best PSNR {best_psnr:.2} unexpectedly far below Balanced {bal_psnr:.2}"
    );
}

#[test]
fn encoding_is_deterministic_per_quality_and_method() {
    // Each (quality, method) pair must be byte-identical across repeated encodes.
    let (w, h) = (40, 24);
    let rgba = image(w, h, |x, y| [byte(x * 6), byte(y * 10), byte(x ^ y)]);
    let dims = Dimensions::new(w, h).unwrap();
    for method in [Effort::Fast, Effort::Balanced, Effort::Best] {
        for quality in [10u8, 50, 90] {
            let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
            let cfg = LossyConfig::new().with_quality(quality).with_effort(method);
            let a = encode_vp8(img, &cfg).unwrap().1;
            let b = encode_vp8(img, &cfg).unwrap().1;
            assert_eq!(a, b, "{method:?} q{quality} is non-deterministic");
        }
    }
}

#[test]
fn segmentation_holds_fidelity_on_mixed_content() {
    // Balanced partitions mixed flat-vs-noise content into multiple quantizer
    // segments (coarser on the busy half where distortion is masked, finer on the
    // flat half). That is a rate-distortion move — a smaller frame for a masked
    // distortion — so it must not grossly regress fidelity: the round-trip PSNR must
    // stay above a conservative floor. The size win and the byte-exact correctness
    // (self-consistency + libwebp Level A) are pinned in `src/frame.rs` and the oracle.
    let (w, h) = (96u32, 96u32);
    let rgba = image(w, h, |x, y| {
        if x < w / 2 {
            [96, 96, 96]
        } else {
            let n = (x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503)))
                >> 8;
            [byte(n & 0xff), byte((n >> 3) & 0xff), byte((x ^ y) * 3)]
        }
    });
    let out = round_trip_method(&rgba, w, h, 60, Effort::Balanced).unwrap();
    let psnr = psnr_rgb(&rgba, &out);
    assert!(
        psnr >= 12.0,
        "segmented Balanced PSNR {psnr:.2} dB below floor"
    );
}

#[test]
fn trellis_holds_fidelity_within_a_decibel_of_round_to_nearest() {
    // Trellis quantization shrinks the frame (pinned in `src/frame.rs`) by dropping
    // rate-inefficient coefficients, which costs a little fidelity. This bounds that
    // cost: the Balanced (trellis) PSNR must stay within ~1.0 dB of the pre-trellis
    // round-to-nearest reference on the banded photo and the AC-rich noise, at every
    // quality. The reference PSNRs are the committed round-to-nearest measurements.
    let photo = image(96, 96, |x, y| {
        let band = i32::try_from(x / 8 % 3).unwrap_or(0);
        let r = u8::try_from(128 + 40 * band - 30).unwrap_or(0);
        [r, byte(x * 2 + y), byte(200 - y)]
    });
    let noisy = image(96, 96, |x, y| {
        let n = (x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503)))
            >> 8;
        [byte(n & 0xff), byte((n >> 3) & 0xff), byte((x ^ y) * 3)]
    });
    // (quality, round-to-nearest reference PSNR) for photo96 and noisy96.
    let photo_ref = [(50u8, 30.11f64), (75, 31.58), (90, 33.71)];
    let noisy_ref = [(50u8, 16.29f64), (75, 16.42), (90, 16.51)];
    for (rgba, refs, name) in [(&photo, &photo_ref, "photo"), (&noisy, &noisy_ref, "noisy")] {
        for &(q, reference) in refs {
            let out = round_trip_method(rgba, 96, 96, q, Effort::Balanced).unwrap();
            let psnr = psnr_rgb(rgba, &out);
            assert!(
                psnr >= reference - 1.0,
                "{name} q{q}: trellis PSNR {psnr:.2} dB fell >1 dB below round-to-nearest {reference:.2}"
            );
        }
    }
}

#[test]
#[ignore = "large frame; run with `-- --ignored`"]
fn large_frame_round_trips_without_breaking() {
    // A mid-size 2048-wide frame (many macroblock columns and rows) must encode and
    // decode without panicking or truncating: the output is a decodable frame of the
    // right dimensions above a conservative fidelity floor. Ignored by default so the
    // normal suite stays fast; run with `cargo test -p webpkit -- --ignored`.
    let (w, h) = (2048u32, 128u32);
    let rgba = image(w, h, |x, y| {
        [byte(x / 8), byte(y * 2), byte((x ^ y) + x / 16)]
    });
    let out = round_trip_method(&rgba, w, h, 75, Effort::Balanced).unwrap();
    assert_eq!(
        out.len(),
        usize::try_from(w * h * 4).unwrap(),
        "large frame pixel count"
    );
    let psnr = psnr_rgb(&rgba, &out);
    assert!(psnr >= 20.0, "large frame PSNR {psnr:.2} dB below floor");
}
