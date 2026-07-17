//! Rewriting a WebP file's sidecar metadata in place, at the chunk level.
//!
//! [`rewrite_metadata`] copies every image-bitstream chunk (`VP8 `/`VP8L`/`ALPH`/
//! `ANIM`/`ANMF`) through byte-for-byte, drops the old `ICCP`/`EXIF`/`XMP `/`VP8X`,
//! and rebuilds the `VP8X` header plus the metadata chunks from the new
//! [`Metadata`] and the caller-supplied canvas/alpha/animation facts — decoding no
//! pixel. The webpmux counterpart to [`super::writer`], which frames a container
//! around a freshly encoded image.

use super::fourcc::FourCc;
use super::reader::{Chunk, chunks};
use super::vp8x::{Vp8xFlags, Vp8xInfo};
use super::writer::{push_chunk, riff_envelope};
use crate::Result;
use crate::image::{Dimensions, Metadata};
use crate::prelude::*;

/// Rewrite `input`'s sidecar metadata to `metadata`, leaving the image bitstream
/// untouched.
///
/// Walks the top-level chunks once: the image-bitstream chunks (`VP8 `, `VP8L`,
/// `ALPH`, `ANIM`, `ANMF`, and any unknown chunk) are copied in their original
/// relative order, while the old `VP8X`, `ICCP`, `EXIF`, and `XMP ` are dropped. A
/// fresh `VP8X` is composed from `canvas`, `has_alpha`, `animated`, and the new
/// `metadata`, and the file is reassembled in the spec's still/animation order
/// (`VP8X`, `ICCP`, image chunks, `EXIF`, `XMP `). A `VP8X` is emitted only when
/// something needs it — new `metadata`, a separate `ALPH` plane to announce, or an
/// animation; absent all three the file is written in the minimal simple form, so
/// stripping every sidecar shrinks an extended still back to bare `VP8L`/`VP8 ` and
/// drops any stray sidecar chunk.
///
/// The caller supplies `canvas`/`has_alpha`/`animated` (e.g. from [`crate::probe`]),
/// so this layer interprets no bitstream and stays a pure chunk shuffler.
///
/// # Errors
///
/// [`chunks`] errors on a non-WebP or truncated file.
pub fn rewrite_metadata(
    input: &[u8],
    metadata: &Metadata,
    canvas: Dimensions,
    has_alpha: bool,
    animated: bool,
) -> Result<Vec<u8>> {
    let mut passthrough: Vec<Chunk<'_>> = Vec::new();
    let mut has_alph_chunk = false;
    let mut has_anim_chunk = false;
    for chunk in chunks(input)? {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8X | FourCc::ICCP | FourCc::EXIF | FourCc::XMP => {},
            FourCc::ALPH => {
                has_alph_chunk = true;
                passthrough.push(chunk);
            },
            FourCc::ANIM | FourCc::ANMF => {
                has_anim_chunk = true;
                passthrough.push(chunk);
            },
            _ => passthrough.push(chunk),
        }
    }

    // A `VP8X` earns its place only by carrying metadata, announcing a sibling
    // `ALPH` plane, or marking an animation. With none of those the simple form
    // suffices, so a full strip shrinks an extended still back to bare and any stray
    // sidecar is dropped.
    if metadata.is_empty() && !has_alph_chunk && !has_anim_chunk {
        let mut body = Vec::new();
        for chunk in &passthrough {
            push_chunk(&mut body, chunk.id, chunk.data);
        }
        return Ok(riff_envelope(&body));
    }

    let base = Vp8xFlags::for_output(metadata, has_alpha);
    let flags = if animated { base.with_animation() } else { base };
    let mut body = Vec::new();
    push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, canvas));
    if let Some(icc) = &metadata.icc_profile {
        push_chunk(&mut body, FourCc::ICCP, icc);
    }
    for chunk in &passthrough {
        push_chunk(&mut body, chunk.id, chunk.data);
    }
    if let Some(exif) = &metadata.exif {
        push_chunk(&mut body, FourCc::EXIF, exif);
    }
    if let Some(xmp) = &metadata.xmp {
        push_chunk(&mut body, FourCc::XMP, xmp);
    }
    Ok(riff_envelope(&body))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::rewrite_metadata;
    use crate::container::fourcc::FourCc;
    use crate::container::reader::{chunks, extract_metadata};
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::image::{Dimensions, Metadata};

    /// Build a RIFF chunk: `FourCC` + LE size + payload + pad byte when odd.
    fn chunk(id: FourCc, data: &[u8]) -> Vec<u8> {
        let mut v = id.0.to_vec();
        v.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    /// Wrap chunk bytes in a `RIFF....WEBP` envelope.
    fn webp(body: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&u32::try_from(4 + body.len()).unwrap().to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(body);
        v
    }

    /// The top-level chunk ids, in order.
    fn ids(file: &[u8]) -> Vec<FourCc> {
        chunks(file).unwrap().filter_map(Result::ok).map(|c| c.id).collect()
    }

    /// The payloads of the image-bitstream chunks, so a rewrite can be checked to
    /// preserve them byte-for-byte.
    fn image_payloads(file: &[u8]) -> Vec<(FourCc, Vec<u8>)> {
        chunks(file)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|c| {
                matches!(
                    c.id,
                    FourCc::VP8 | FourCc::VP8L | FourCc::ALPH | FourCc::ANIM | FourCc::ANMF
                )
            })
            .map(|c| (c.id, c.data.to_vec()))
            .collect()
    }

    /// The rewritten file's `VP8X`, if it has one.
    fn vp8x_of(file: &[u8]) -> Option<Vp8xInfo> {
        chunks(file)
            .unwrap()
            .filter_map(Result::ok)
            .find(|c| c.id == FourCc::VP8X)
            .map(|c| Vp8xInfo::parse(c.data).unwrap())
    }

    proptest! {
        /// Any metadata written into a bare `VP8L` still round-trips exactly, and
        /// the image chunk survives byte-for-byte — the core mux invariant.
        #[test]
        fn round_trips_metadata_and_preserves_the_image(
            vp8l in prop::collection::vec(any::<u8>(), 1..48),
            icc in prop::option::of(prop::collection::vec(any::<u8>(), 0..24)),
            exif in prop::option::of(prop::collection::vec(any::<u8>(), 0..24)),
            xmp in prop::option::of(prop::collection::vec(any::<u8>(), 0..24)),
        ) {
            let file = webp(&chunk(FourCc::VP8L, &vp8l));
            let before = image_payloads(&file);
            let metadata = Metadata::none()
                .with_icc_profile(icc)
                .with_exif(exif)
                .with_xmp(xmp);
            let out =
                rewrite_metadata(&file, &metadata, Dimensions::new(8, 8).unwrap(), false, false)
                    .unwrap();
            prop_assert_eq!(extract_metadata(&out), metadata);
            prop_assert_eq!(image_payloads(&out), before);
        }
    }

    #[test]
    fn orders_chunks_per_spec_for_an_extended_still() {
        // VP8X + ALPH + VP8, plus new ICC and XMP: the output is spec-ordered.
        let mut body = chunk(
            FourCc::VP8X,
            &Vp8xInfo::build(Vp8xFlags(0x10), Dimensions::new(4, 4).unwrap()),
        );
        body.extend_from_slice(&chunk(FourCc::ALPH, &[0x01, 0xAA]));
        body.extend_from_slice(&chunk(FourCc::VP8, &[9, 8, 7]));
        let file = webp(&body);
        let metadata = Metadata::none()
            .with_icc_profile(vec![1, 2])
            .with_xmp(vec![3]);
        let out = rewrite_metadata(
            &file,
            &metadata,
            Dimensions::new(4, 4).unwrap(),
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            ids(&out),
            vec![
                FourCc::VP8X,
                FourCc::ICCP,
                FourCc::ALPH,
                FourCc::VP8,
                FourCc::XMP
            ]
        );
    }

    #[test]
    fn preserves_alpha_and_animation_flags() {
        // An animation (VP8X anim + ANIM + ANMF): the rebuilt VP8X keeps the alpha
        // and animation flags and adds only the requested Exif flag.
        let canvas = Dimensions::new(16, 16).unwrap();
        let mut body = chunk(FourCc::VP8X, &Vp8xInfo::build(Vp8xFlags(0x10 | 0x02), canvas));
        body.extend_from_slice(&chunk(FourCc::ANIM, &[0u8; 6]));
        body.extend_from_slice(&chunk(FourCc::ANMF, &[1, 2, 3, 4]));
        let file = webp(&body);
        let out = rewrite_metadata(
            &file,
            &Metadata::none().with_exif(vec![9]),
            canvas,
            true,
            true,
        )
        .unwrap();
        let vp8x = vp8x_of(&out).unwrap();
        assert!(vp8x.flags.is_animated() && vp8x.flags.has_alpha() && vp8x.flags.has_exif());
        assert!(!vp8x.flags.has_icc() && !vp8x.flags.has_xmp());
        // The frame chunks pass through untouched.
        assert_eq!(ids(&out), vec![FourCc::VP8X, FourCc::ANIM, FourCc::ANMF, FourCc::EXIF]);
    }

    #[test]
    fn empty_metadata_on_a_simple_file_stays_bare_and_drops_sidecars() {
        // A bare VP8L with a stray trailing EXIF (no VP8X): stripping yields a bare
        // VP8L, dropping the EXIF and adding no VP8X.
        let mut body = chunk(FourCc::VP8L, &[0x2f, 1, 2, 3]);
        body.extend_from_slice(&chunk(FourCc::EXIF, &[7, 7]));
        let file = webp(&body);
        let out = rewrite_metadata(
            &file,
            &Metadata::none(),
            Dimensions::new(2, 2).unwrap(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(ids(&out), vec![FourCc::VP8L]);
        assert!(extract_metadata(&out).is_empty());
    }

    #[test]
    fn strips_an_extended_still_back_to_bare() {
        // A VP8X + VP8L still with no separate alpha plane and no animation: a full
        // strip has nothing left for a VP8X to carry, so the output is minimal bare
        // VP8L (webpmux-style), not an empty VP8X.
        let canvas = Dimensions::new(4, 4).unwrap();
        let mut body = chunk(FourCc::VP8X, &Vp8xInfo::build(Vp8xFlags(0x20), canvas));
        body.extend_from_slice(&chunk(FourCc::ICCP, &[0xDE, 0xAD]));
        body.extend_from_slice(&chunk(FourCc::VP8L, &[0x2f, 1, 2, 3]));
        let file = webp(&body);
        let out = rewrite_metadata(&file, &Metadata::none(), canvas, false, false).unwrap();
        assert_eq!(ids(&out), vec![FourCc::VP8L]);
        assert!(extract_metadata(&out).is_empty());
    }

    #[test]
    fn empty_metadata_keeps_the_vp8x_for_a_separate_alpha_plane() {
        // A lossy alpha still needs its VP8X even with no metadata: the ALPH cannot
        // be announced without one, so the extended form is kept.
        let canvas = Dimensions::new(4, 4).unwrap();
        let mut body = chunk(FourCc::VP8X, &Vp8xInfo::build(Vp8xFlags(0x10), canvas));
        body.extend_from_slice(&chunk(FourCc::ALPH, &[0x01, 0xAA]));
        body.extend_from_slice(&chunk(FourCc::VP8, &[9, 8, 7]));
        let file = webp(&body);
        let out = rewrite_metadata(&file, &Metadata::none(), canvas, true, false).unwrap();
        assert_eq!(ids(&out), vec![FourCc::VP8X, FourCc::ALPH, FourCc::VP8]);
        let vp8x = vp8x_of(&out).unwrap();
        assert!(vp8x.flags.has_alpha() && !vp8x.flags.has_exif());
    }

    #[test]
    fn replaces_existing_metadata_rather_than_appending() {
        // A file that already carries ICC gets a wholly new metadata set: the old
        // ICCP is dropped, not duplicated.
        let mut body = chunk(
            FourCc::VP8X,
            &Vp8xInfo::build(Vp8xFlags(0x20), Dimensions::new(4, 4).unwrap()),
        );
        body.extend_from_slice(&chunk(FourCc::ICCP, &[0xDE, 0xAD]));
        body.extend_from_slice(&chunk(FourCc::VP8L, &[0x2f, 1]));
        let file = webp(&body);
        let out = rewrite_metadata(
            &file,
            &Metadata::none().with_exif(vec![0xBE, 0xEF]),
            Dimensions::new(4, 4).unwrap(),
            false,
            false,
        )
        .unwrap();
        let metadata = extract_metadata(&out);
        assert!(metadata.icc_profile.is_none());
        assert_eq!(metadata.exif.as_deref(), Some(&[0xBE, 0xEF][..]));
        assert_eq!(ids(&out), vec![FourCc::VP8X, FourCc::VP8L, FourCc::EXIF]);
    }

    #[test]
    fn propagates_a_truncated_container_error() {
        // A chunk header declaring more payload than is present is truncated, not
        // silently rewritten.
        let mut body = FourCc::VP8L.0.to_vec();
        body.extend_from_slice(&100u32.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3]);
        let file = webp(&body);
        assert!(
            rewrite_metadata(
                &file,
                &Metadata::none().with_exif(vec![1]),
                Dimensions::new(2, 2).unwrap(),
                false,
                false,
            )
            .is_err()
        );
    }
}
