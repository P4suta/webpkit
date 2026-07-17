//! Public RIFF chunk inspection: walk a WebP file's top-level chunks (and an
//! animation frame's inner chunks) in file order without decoding any pixels.
//!
//! Thin, stable wrappers over the internal [`container::reader`] walker so a
//! consumer can list a file's structure — the `webp info -v` chunk table, a
//! per-frame codec probe — without reaching into the codec internals.
//!
//! [`container::reader`]: crate::container::reader

use crate::container::anim::ANMF_HEADER_LEN;
use crate::container::fourcc::FourCc;
use crate::container::reader;
use crate::error::Result;

/// A single RIFF chunk of a WebP file: its four-byte tag and its payload.
///
/// Yielded by [`chunks`] (top-level chunks) and by [`Chunk::frame_chunks`] (the
/// inner chunks of an animation frame). It borrows the input buffer and copies
/// freely; it decodes nothing.
#[derive(Clone, Copy, Debug)]
pub struct Chunk<'a> {
    id: [u8; 4],
    payload: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// The chunk's four-byte `FourCC` tag, with the trailing space preserved for
    /// `VP8 ` and `XMP ` — it is part of the tag, not padding.
    #[must_use]
    pub const fn fourcc(self) -> [u8; 4] {
        self.id
    }

    /// The chunk payload, excluding the 8-byte header and any odd-size pad byte.
    #[must_use]
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }

    /// For an `ANMF` animation-frame chunk, iterate that frame's **own** inner
    /// chunks — an optional `ALPH` followed by the frame's image chunk — in file
    /// order, skipping the fixed frame header internally.
    ///
    /// `None` when this chunk is not `ANMF`, or when its payload is too short to
    /// hold the frame header. Like [`chunks`], the iterator yields
    /// `Result<Chunk>` so a truncated frame ends the walk as an error item.
    #[must_use]
    pub fn frame_chunks(self) -> Option<Chunks<'a>> {
        if self.id != FourCc::ANMF.0 {
            return None;
        }
        let body = self.payload.get(ANMF_HEADER_LEN..)?;
        Some(Chunks {
            inner: reader::Chunks::walk(body),
        })
    }
}

/// An iterator over RIFF chunks, in file order.
///
/// Each item is a [`Chunk`] or the first [`Error::Truncated`] encountered, after
/// which iteration ends. Obtain one from [`chunks`] (a file's top-level chunks) or
/// [`Chunk::frame_chunks`] (an animation frame's inner chunks).
///
/// [`Error::Truncated`]: crate::Error::Truncated
#[derive(Debug)]
pub struct Chunks<'a> {
    inner: reader::Chunks<'a>,
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        Some(item.map(|chunk| Chunk {
            id: chunk.id.0,
            payload: chunk.data,
        }))
    }
}

/// Iterate the top-level RIFF chunks of a WebP file, in file order, without
/// decoding any pixels.
///
/// The outer [`Result`] rejects a bad `RIFF`/`WEBP` envelope; the iterator then
/// yields `Result<Chunk>` so a truncated tail ends the walk as an error item —
/// use `.map_while(Result::ok)` to take the chunks up to the first break.
///
/// This is the structural counterpart to [`probe`](crate::probe): where `probe`
/// answers *what image is this*, `chunks` answers *how is the container laid out*.
///
/// # Examples
///
/// ```
/// let rgba = vec![0u8; 4 * 4 * 4]; // a 4x4 RGBA image
/// let webp = webpkit::encode_lossless_rgba(4, 4, &rgba)?;
///
/// let tags: Vec<[u8; 4]> = webpkit::chunks(&webp)?
///     .map_while(Result::ok)
///     .map(|chunk| chunk.fourcc())
///     .collect();
/// assert!(tags.contains(b"VP8L"));
///
/// // A non-WebP envelope is rejected outright.
/// assert!(webpkit::chunks(b"not a webp file").is_err());
/// # Ok::<(), webpkit::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::Truncated`] if the input is shorter than the 12-byte header, or
/// [`Error::NotWebp`] if the `RIFF`/`WEBP` magic is wrong.
///
/// [`Error::Truncated`]: crate::Error::Truncated
/// [`Error::NotWebp`]: crate::Error::NotWebp
pub fn chunks(input: &[u8]) -> Result<Chunks<'_>> {
    Ok(Chunks {
        inner: reader::chunks(input)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{Chunk, chunks};
    use crate::error::Error;
    use crate::{
        AnimationEncoder, BlendMode, Dimensions, DisposalMode, FrameMeta, ImageRef, PixelLayout,
        encode_lossless_rgba,
    };

    /// A small lossless still, encoded to a real WebP file.
    fn lossless_still() -> Vec<u8> {
        let rgba: Vec<u8> = (0..4u32 * 4 * 4).map(|i| (i * 7 % 251) as u8).collect();
        encode_lossless_rgba(4, 4, &rgba).unwrap()
    }

    #[test]
    fn walks_a_still_files_top_level_chunks() {
        let webp = lossless_still();
        let tags: Vec<[u8; 4]> = chunks(&webp)
            .unwrap()
            .map_while(Result::ok)
            .map(Chunk::fourcc)
            .collect();
        assert!(
            tags.contains(b"VP8L"),
            "expected a VP8L chunk, got {tags:?}"
        );
    }

    #[test]
    fn fourcc_preserves_the_trailing_space() {
        // `XMP ` carries a trailing space that is part of the tag: a chunk walk
        // must not trim it.
        let dims = Dimensions::new(4, 4).unwrap();
        let rgba = vec![0x33u8; 4 * 4 * 4];
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let webp = crate::Encoder::lossless()
            .metadata(crate::Metadata {
                xmp: Some(vec![b'<', b'x', b'>']),
                ..crate::Metadata::none()
            })
            .encode_ref(img)
            .unwrap();
        let has_xmp = chunks(&webp)
            .unwrap()
            .map_while(Result::ok)
            .any(|chunk| chunk.fourcc() == *b"XMP ");
        assert!(has_xmp, "the XMP chunk tag must keep its trailing space");
    }

    #[test]
    fn payload_excludes_header_and_pad() {
        let webp = lossless_still();
        let vp8l = chunks(&webp)
            .unwrap()
            .map_while(Result::ok)
            .find(|chunk| chunk.fourcc() == *b"VP8L")
            .unwrap();
        // A VP8L payload opens with the `0x2f` signature byte.
        assert_eq!(vp8l.payload().first(), Some(&0x2f));
    }

    #[test]
    fn rejects_a_bad_envelope() {
        assert_eq!(chunks(b"not a webp file").unwrap_err(), Error::NotWebp);
        assert_eq!(chunks(&[0u8; 4]).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn a_truncated_tail_ends_the_walk_as_an_error() {
        // A valid envelope with one chunk header declaring far more bytes than the
        // buffer holds: the walk yields the error item, not a panic.
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&100u32.to_le_bytes());
        file.extend_from_slice(b"WEBP");
        file.extend_from_slice(b"VP8L");
        file.extend_from_slice(&50u32.to_le_bytes()); // claims 50, supplies 3
        file.extend_from_slice(&[0x2f, 0x00, 0x00]);
        let items: Vec<_> = chunks(&file).unwrap().collect();
        assert!(matches!(items.last(), Some(Err(Error::Truncated))));
    }

    /// A one-frame animation, so `frame_chunks` has an `ANMF` to descend into.
    fn animation() -> Vec<u8> {
        let canvas = Dimensions::new(8, 6).unwrap();
        let rgba = vec![0x40u8; 8 * 6 * 4];
        let frame = ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap();
        AnimationEncoder::new(canvas)
            .add_frame(
                frame,
                FrameMeta::new(0, 0, canvas, 100, BlendMode::Blend, DisposalMode::Keep),
            )
            .unwrap()
            .finish()
    }

    #[test]
    fn frame_chunks_yields_a_frames_image_chunk() {
        let webp = animation();
        let anmf: Chunk<'_> = chunks(&webp)
            .unwrap()
            .map_while(Result::ok)
            .find(|chunk| chunk.fourcc() == *b"ANMF")
            .expect("an animation has an ANMF chunk");
        let inner: Vec<[u8; 4]> = anmf
            .frame_chunks()
            .expect("ANMF descends into its inner chunks")
            .map_while(Result::ok)
            .map(Chunk::fourcc)
            .collect();
        // The lossless animation encoder writes a VP8L image chunk per frame.
        assert!(
            inner.contains(b"VP8L"),
            "expected the frame's VP8L image chunk, got {inner:?}"
        );
    }

    #[test]
    fn frame_chunks_is_none_for_a_non_anmf_chunk() {
        let webp = lossless_still();
        let vp8l = chunks(&webp)
            .unwrap()
            .map_while(Result::ok)
            .find(|chunk| chunk.fourcc() == *b"VP8L")
            .unwrap();
        assert!(vp8l.frame_chunks().is_none());
    }
}
