//! Writing the WebP RIFF container: the simple (`VP8L`) and extended (`VP8X`)
//! lossless forms.
//!
//! [`wrap_vp8l`] emits the 12-byte `RIFF....WEBP` header plus a single `VP8L`
//! chunk. [`wrap`] upgrades to the extended form (`VP8X` + `ICCP`/`EXIF`/`XMP `)
//! when metadata must be carried — the exact inverse of
//! [`super::reader::parse_container`].
#![expect(
    clippy::cast_possible_truncation,
    reason = "callers validate dimensions <= MAX_DIMENSION and metadata blobs are \
              small, so every chunk/RIFF size stays far below u32::MAX"
)]

use super::fourcc::FourCc;
use super::vp8x::{Vp8xFlags, Vp8xInfo};
use crate::image::{Dimensions, Metadata};
use crate::prelude::*;

/// Wrap a VP8L bitstream `payload` in a minimal simple-lossless WebP container.
#[must_use]
pub fn wrap_vp8l(payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + payload.len() + (payload.len() & 1));
    push_chunk(&mut body, FourCc::VP8L, payload);
    riff_envelope(&body)
}

/// Wrap a lossy VP8 bitstream `payload` in a minimal simple-lossy WebP container.
///
/// Produces a bare `RIFF`/`WEBP`/`VP8 ` file — all a bare opaque, metadata-free
/// lossy image needs. The extended (`VP8X`) form carrying metadata or a sibling
/// `ALPH` chunk is emitted by [`wrap_vp8_extended`].
#[must_use]
pub fn wrap_vp8(payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + payload.len() + (payload.len() & 1));
    push_chunk(&mut body, FourCc::VP8, payload);
    riff_envelope(&body)
}

/// Wrap a lossy `VP8` bitstream in the extended (`VP8X`) WebP container, carrying
/// an optional `ALPH` alpha chunk and any ICC/Exif/XMP `metadata`.
///
/// Emits the chunks in the spec's mandated still-image order: `VP8X`, `ICCP`,
/// `ALPH`, `VP8 `, `EXIF`, `XMP `. `alpha` is the FULL `ALPH` payload (its 1-byte
/// header plus the compressed or raw plane), assembled by the caller; passing it
/// (or any metadata) is what forces the extended form — a bare opaque lossy image
/// needs only [`wrap_vp8`]. `dims` is the canvas size; the alpha and metadata flags
/// are set from `alpha.is_some()` and `metadata` via [`Vp8xFlags::for_output`].
#[must_use]
pub fn wrap_vp8_extended(
    vp8_payload: &[u8],
    alpha: Option<&[u8]>,
    dims: Dimensions,
    metadata: &Metadata,
) -> Vec<u8> {
    let flags = Vp8xFlags::for_output(metadata, alpha.is_some());
    let mut body = Vec::new();
    push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, dims));
    if let Some(icc) = &metadata.icc_profile {
        push_chunk(&mut body, FourCc::ICCP, icc);
    }
    if let Some(alph) = alpha {
        push_chunk(&mut body, FourCc::ALPH, alph);
    }
    push_chunk(&mut body, FourCc::VP8, vp8_payload);
    if let Some(exif) = &metadata.exif {
        push_chunk(&mut body, FourCc::EXIF, exif);
    }
    if let Some(xmp) = &metadata.xmp {
        push_chunk(&mut body, FourCc::XMP, xmp);
    }
    riff_envelope(&body)
}

/// Wrap a VP8L `payload` in a WebP container, upgrading to the extended (`VP8X`)
/// form when `metadata` is present.
///
/// A bare `VP8L` file suffices for a plain lossless image (even with alpha); the
/// `VP8X` form is emitted only to carry ICC/Exif/XMP metadata. Chunk order
/// follows the spec: `VP8X`, `ICCP`, `VP8L`, `EXIF`, `XMP `. `dims` is the canvas
/// size (= the VP8L image size); `has_alpha` sets the VP8X alpha flag.
#[must_use]
pub fn wrap(payload: &[u8], dims: Dimensions, metadata: &Metadata, has_alpha: bool) -> Vec<u8> {
    if metadata.is_empty() {
        return wrap_vp8l(payload);
    }
    let flags = Vp8xFlags::for_output(metadata, has_alpha);
    let mut body = Vec::new();
    push_chunk(&mut body, FourCc::VP8X, &Vp8xInfo::build(flags, dims));
    if let Some(icc) = &metadata.icc_profile {
        push_chunk(&mut body, FourCc::ICCP, icc);
    }
    push_chunk(&mut body, FourCc::VP8L, payload);
    if let Some(exif) = &metadata.exif {
        push_chunk(&mut body, FourCc::EXIF, exif);
    }
    if let Some(xmp) = &metadata.xmp {
        push_chunk(&mut body, FourCc::XMP, xmp);
    }
    riff_envelope(&body)
}

/// Append a `fourcc + little-endian size + payload + even-pad` chunk to `out`.
///
/// Shared by the still-image writer and the animation writer (`ANIM`/`ANMF`).
pub fn push_chunk(out: &mut Vec<u8>, id: FourCc, payload: &[u8]) {
    out.extend_from_slice(&id.0);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() & 1 == 1 {
        out.push(0); // odd-size chunks are padded; the pad is not counted in size
    }
}

/// Wrap chunk `body` bytes in the 12-byte `RIFF....WEBP` envelope. The RIFF size
/// counts the `WEBP` tag (4) plus every chunk.
#[must_use]
pub fn riff_envelope(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + body.len());
    out.extend_from_slice(&FourCc::RIFF.0);
    out.extend_from_slice(&((4 + body.len()) as u32).to_le_bytes());
    out.extend_from_slice(&FourCc::WEBP.0);
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::wrap_vp8l;
    use crate::container::reader::parse_container;

    proptest! {
        /// `wrap` then `parse_container` recovers the VP8L payload and any
        /// metadata exactly, whether the file stays bare `VP8L` or upgrades to
        /// `VP8X` (the exact inverse relationship).
        #[test]
        fn wrap_parse_round_trips(
            payload in prop::collection::vec(any::<u8>(), 1..64),
            icc in prop::option::of(prop::collection::vec(any::<u8>(), 0..32)),
            exif in prop::option::of(prop::collection::vec(any::<u8>(), 0..32)),
            xmp in prop::option::of(prop::collection::vec(any::<u8>(), 0..32)),
            w in 1u32..=256,
            h in 1u32..=256,
            has_alpha in any::<bool>(),
        ) {
            use crate::image::{Dimensions, Metadata};
            let metadata = Metadata {
                icc_profile: icc,
                exif,
                xmp,
            };
            let dims = Dimensions::new(w, h).unwrap();
            let file = super::wrap(&payload, dims, &metadata, has_alpha);
            let parsed = parse_container(&file, true).unwrap();
            prop_assert_eq!(parsed.vp8l, &payload[..]);
            prop_assert_eq!(parsed.metadata, metadata);
        }
    }

    /// The VP8L payload of a simple-form file.
    fn vp8l_of(file: &[u8]) -> Vec<u8> {
        parse_container(file, false).unwrap().vp8l.to_vec()
    }

    #[test]
    fn round_trips_even_payload_through_parse_container() {
        let payload = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66];
        let file = wrap_vp8l(&payload);
        // An even payload needs no pad byte, so the file length stays even.
        assert_eq!(file.len() % 2, 0);
        assert_eq!(vp8l_of(&file), payload);
    }

    #[test]
    fn round_trips_odd_payload_with_pad_byte() {
        let payload = [0xaau8, 0xbb, 0xcc];
        let file = wrap_vp8l(&payload);
        // The odd payload gets a single pad byte, restoring even file length.
        assert_eq!(file.len() % 2, 0);
        assert_eq!(vp8l_of(&file), payload);
    }

    #[test]
    fn writes_exact_header_bytes() {
        // payload = [1,2,3,4]: chunk_len = 8+4 = 12, RIFF size = 4+12 = 16.
        let file = wrap_vp8l(&[1, 2, 3, 4]);
        let expected = [
            b'R', b'I', b'F', b'F', 16, 0, 0, 0, // RIFF + LE size (16)
            b'W', b'E', b'B', b'P', // form type
            b'V', b'P', b'8', b'L', 4, 0, 0, 0, // VP8L + LE payload length (4)
            1, 2, 3, 4, // payload
        ];
        assert_eq!(file, expected);
    }

    #[test]
    fn wrap_vp8_extended_orders_chunks_and_round_trips() {
        use super::wrap_vp8_extended;
        use crate::container::fourcc::FourCc;
        use crate::container::reader::{ImageChunk, locate_image_with_alpha};
        use crate::image::{Dimensions, Metadata};
        let vp8 = [0x11u8, 0x22, 0x33];
        let alph = [0x01u8, 0xAA, 0xBB, 0xCC]; // header byte + 3 plane bytes
        let dims = Dimensions::new(8, 4).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![9, 9]),
            exif: Some(vec![7]),
            ..Metadata::none()
        };
        let file = wrap_vp8_extended(&vp8, Some(&alph), dims, &metadata);
        // Chunk order: VP8X, ICCP, ALPH, VP8, EXIF (no XMP here).
        let order: Vec<[u8; 4]> = {
            let mut ids = Vec::new();
            let mut cur = 12;
            while cur + 8 <= file.len() {
                let id = [file[cur], file[cur + 1], file[cur + 2], file[cur + 3]];
                let size = u32::from_le_bytes([
                    file[cur + 4],
                    file[cur + 5],
                    file[cur + 6],
                    file[cur + 7],
                ]) as usize;
                ids.push(id);
                cur += 8 + size + (size & 1);
            }
            ids
        };
        assert_eq!(
            order,
            vec![
                FourCc::VP8X.0,
                FourCc::ICCP.0,
                FourCc::ALPH.0,
                FourCc::VP8.0,
                FourCc::EXIF.0
            ]
        );
        let located = locate_image_with_alpha(&file).unwrap();
        assert_eq!(located.image, ImageChunk::Lossy(&vp8[..]));
        assert_eq!(located.alpha, Some(&alph[..]));
        let vp8x = located.vp8x.unwrap();
        assert!(vp8x.flags.has_alpha() && vp8x.flags.has_icc() && vp8x.flags.has_exif());
        assert!(!vp8x.flags.has_xmp());
        assert_eq!(vp8x.canvas, dims);
    }

    #[test]
    fn wrap_vp8_extended_without_alpha_clears_the_flag() {
        use super::wrap_vp8_extended;
        use crate::image::{Dimensions, Metadata};
        let file = wrap_vp8_extended(
            &[0x10, 0x20],
            None,
            Dimensions::new(2, 2).unwrap(),
            &Metadata::none(),
        );
        assert!(!file.windows(4).any(|w| w == b"ALPH"));
    }

    #[test]
    fn wrap_vp8_writes_a_lossy_chunk_with_the_space_fourcc() {
        // The lossy `VP8 ` fourcc has a trailing space; an odd payload is padded.
        let file = super::wrap_vp8(&[0xaa, 0xbb, 0xcc]);
        let expected = [
            b'R', b'I', b'F', b'F', 16, 0, 0, 0, // RIFF + LE size (4 + 8 + 3 + 1 pad)
            b'W', b'E', b'B', b'P', // form type
            b'V', b'P', b'8', b' ', 3, 0, 0, 0, // "VP8 " + LE payload length (3)
            0xaa, 0xbb, 0xcc, 0x00, // payload + pad byte
        ];
        assert_eq!(file, expected);
    }

    #[test]
    fn wrap_without_metadata_is_bare_vp8l() {
        use super::wrap;
        use crate::image::{Dimensions, Metadata};
        let payload = [0x2fu8, 1, 2, 3];
        let file = wrap(
            &payload,
            Dimensions::new(2, 2).unwrap(),
            &Metadata::none(),
            false,
        );
        assert_eq!(file, wrap_vp8l(&payload));
    }

    #[test]
    fn wrap_with_metadata_round_trips_through_parse_container() {
        use super::wrap;
        use crate::container::reader::parse_container;
        use crate::image::{Dimensions, Metadata};
        let metadata = Metadata {
            icc_profile: Some(vec![1, 2, 3]), // odd length -> pad byte
            exif: Some(vec![4, 5]),
            xmp: Some(vec![6, 7, 8, 9]),
        };
        let payload = [0x2fu8, 0xAA, 0xBB];
        let dims = Dimensions::new(4, 4).unwrap();
        let file = wrap(&payload, dims, &metadata, true);
        let parsed = parse_container(&file, true).unwrap();
        assert_eq!(parsed.vp8l, &payload);
        assert_eq!(parsed.metadata, metadata);
        let vp8x = parsed.vp8x.unwrap();
        assert!(vp8x.flags.has_icc() && vp8x.flags.has_exif());
        assert!(vp8x.flags.has_xmp() && vp8x.flags.has_alpha());
        assert_eq!(vp8x.canvas, dims);
    }
}
