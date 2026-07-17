//! RIFF chunk parsing: iterate a WebP file's chunks and locate the VP8L payload.

use super::fourcc::FourCc;
use super::vp8x::Vp8xInfo;
use crate::image::Metadata;
use crate::{Error, Result};

/// A single RIFF chunk: its `FourCC` tag and payload (the odd-size pad byte, if
/// any, is excluded from `data`).
pub struct Chunk<'a> {
    /// The chunk's four-character identifier.
    pub id: FourCc,
    /// The chunk payload (excluding the header and any odd-size pad byte).
    pub data: &'a [u8],
}

/// Iterator over the chunks in a RIFF body. Each item is a [`Chunk`] or the
/// first (and only) [`Error::Truncated`] encountered, after which iteration ends.
#[derive(Debug)]
pub struct Chunks<'a> {
    rest: &'a [u8],
    done: bool,
}

impl<'a> Chunks<'a> {
    /// Iterate a bare chunk sequence (no `RIFF....WEBP` header) — e.g. the frame
    /// data inside an `ANMF` chunk. [`chunks`] wraps this after validating the
    /// 12-byte header.
    #[must_use]
    pub const fn walk(body: &'a [u8]) -> Self {
        Self {
            rest: body,
            done: false,
        }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match parse_chunk(self.rest) {
            Ok(None) => None,
            Ok(Some((chunk, advance))) => {
                self.rest = self.rest.get(advance..).unwrap_or(&[]);
                Some(Ok(chunk))
            },
            Err(err) => {
                self.done = true;
                Some(Err(err))
            },
        }
    }
}

/// Parse the chunk at the front of `data`, returning it and the number of bytes
/// it occupies (`8` header + payload + even-pad). `Ok(None)` marks the end of a
/// chunk sequence (empty `data`); [`Error::Truncated`] a short/oversized chunk.
///
/// Taking `size` from a real slice via `get(..size)` avoids the `8 + size`
/// overflow a hostile u32 size would cause on a 32-bit `usize`.
fn parse_chunk(data: &[u8]) -> Result<Option<(Chunk<'_>, usize)>> {
    if data.is_empty() {
        return Ok(None);
    }
    // Every chunk begins with a 4-byte FourCC + 4-byte little-endian size.
    if data.len() < 8 {
        return Err(Error::Truncated);
    }
    let id = FourCc([data[0], data[1], data[2], data[3]]);
    let size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let body = &data[8..];
    let Some(payload) = body.get(..size) else {
        return Err(Error::Truncated);
    };
    // `payload` exists, so `size <= body.len()`; `8 + size + pad <= data.len() + 1`
    // cannot overflow. Chunks are padded to even size; the pad is not in `size`.
    let advance = 8 + size + (size & 1);
    Ok(Some((Chunk { id, data: payload }, advance)))
}

/// Read the single chunk starting at `offset` in `data`, returning it and the
/// offset of the following chunk.
///
/// `Ok(None)` marks the end of the sequence (`offset` at or past the end). Lets
/// an animation frame walker step `ANMF` chunks lazily by cursor without
/// re-scanning from the start.
///
/// # Errors
///
/// [`Error::Truncated`] on a short or oversized chunk header at `offset`.
pub fn read_chunk_at(data: &[u8], offset: usize) -> Result<Option<(Chunk<'_>, usize)>> {
    let Some(rest) = data.get(offset..) else {
        return Ok(None);
    };
    match parse_chunk(rest)? {
        None => Ok(None),
        Some((chunk, advance)) => Ok(Some((chunk, offset + advance))),
    }
}

/// Validate the 12-byte `RIFF....WEBP` header and return the absolute
/// `[start, end)` byte range of the chunk body.
///
/// The end is clamped to the declared RIFF size and never past the real buffer;
/// `start` is the 12-byte header length (or `end` when a tiny `riff_size` leaves
/// no body).
///
/// Shared by [`chunks`] and animation walkers, which step `ANMF` chunks by
/// absolute cursor via [`read_chunk_at`].
///
/// # Errors
///
/// [`Error::Truncated`] if the header is shorter than 12 bytes, or
/// [`Error::NotWebp`] if the `RIFF`/`WEBP` magic is wrong.
pub fn body_range(input: &[u8]) -> Result<(usize, usize)> {
    if input.len() < 12 {
        return Err(Error::Truncated);
    }
    if input[0..4] != FourCc::RIFF.0 || input[8..12] != FourCc::WEBP.0 {
        return Err(Error::NotWebp);
    }
    // The RIFF size counts everything after the size field ("WEBP" + chunks);
    // clamp the chunk region to the declared size but never past the real buffer.
    let riff_size = u32::from_le_bytes([input[4], input[5], input[6], input[7]]) as usize;
    let end = 8usize.saturating_add(riff_size).min(input.len());
    Ok((12.min(end), end))
}

/// Validate the header and return an iterator over the body chunks.
///
/// # Errors
///
/// The errors of [`body_range`].
pub fn chunks(input: &[u8]) -> Result<Chunks<'_>> {
    let (start, end) = body_range(input)?;
    Ok(Chunks::walk(&input[start..end]))
}

/// A parsed still-image WebP: the `VP8L` payload plus any sidecar metadata and
/// the extended-header info.
#[derive(Debug)]
pub struct ParsedWebp<'a> {
    /// The raw `VP8L` bitstream payload (starting at the `0x2f` signature).
    pub vp8l: &'a [u8],
    /// Sidecar metadata (empty unless `read_metadata` was set and chunks exist).
    pub metadata: Metadata,
    /// The VP8X extended header, if the file used the extended format.
    pub vp8x: Option<Vp8xInfo>,
}

/// Parse a still-image WebP into its VP8L payload and (optionally) metadata.
///
/// Handles the simple (`VP8L`) and extended (`VP8X` + chunks) forms. Animation
/// (`ANIM`/`ANMF`) and lossy (`VP8 `) files are rejected with
/// [`Error::UnsupportedFeature`]; unknown chunks are skipped.
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] for animation/lossy, [`Error::InvalidContainer`]
/// for a malformed VP8X, [`Error::MissingImage`] when no `VP8L` chunk is present,
/// or [`chunks`] errors on a truncated file.
pub fn parse_container(input: &[u8], read_metadata: bool) -> Result<ParsedWebp<'_>> {
    let mut image: Option<&[u8]> = None;
    let mut extended: Option<Vp8xInfo> = None;
    let mut metadata = Metadata::none();
    for chunk in chunks(input)? {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8X => {
                let info = Vp8xInfo::parse(chunk.data)?;
                if info.flags.is_animated() {
                    return Err(Error::UnsupportedFeature);
                }
                extended = Some(info);
            },
            FourCc::VP8L if image.is_none() => image = Some(chunk.data),
            FourCc::ICCP if read_metadata => metadata.icc_profile = Some(chunk.data.to_vec()),
            FourCc::EXIF if read_metadata => metadata.exif = Some(chunk.data.to_vec()),
            FourCc::XMP if read_metadata => metadata.xmp = Some(chunk.data.to_vec()),
            FourCc::ANIM | FourCc::ANMF | FourCc::VP8 => return Err(Error::UnsupportedFeature),
            _ => {},
        }
    }
    let vp8l = image.ok_or(Error::MissingImage)?;
    Ok(ParsedWebp {
        vp8l,
        metadata,
        vp8x: extended,
    })
}

/// Collect any `ICCP`/`EXIF`/`XMP` sidecar metadata from a WebP container.
///
/// Unlike [`parse_container`], which is VP8L-only, this also serves lossy (`VP8 `)
/// files so their metadata survives a decode → re-encode round trip. Best-effort: a
/// malformed container (or one with no metadata chunks, e.g. a bare `VP8 `/`VP8L`)
/// yields [`Metadata::none`].
#[must_use]
pub fn extract_metadata(input: &[u8]) -> Metadata {
    let mut metadata = Metadata::none();
    let Ok(walk) = chunks(input) else {
        return metadata;
    };
    for chunk in walk {
        let Ok(chunk) = chunk else { break };
        match chunk.id {
            FourCc::ICCP => metadata.icc_profile = Some(chunk.data.to_vec()),
            FourCc::EXIF => metadata.exif = Some(chunk.data.to_vec()),
            FourCc::XMP => metadata.xmp = Some(chunk.data.to_vec()),
            _ => {},
        }
    }
    metadata
}

/// Which coded image a still WebP carries, paired with its raw bitstream payload.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageChunk<'a> {
    /// A `VP8L` lossless bitstream payload (starts at the `0x2f` signature).
    Lossless(&'a [u8]),
    /// A `VP8 ` lossy bitstream payload.
    Lossy(&'a [u8]),
}

/// A located still image plus any sibling `ALPH` alpha chunk and VP8X info.
#[derive(Debug)]
pub struct LocatedImage<'a> {
    /// The coded image chunk (lossless or lossy).
    pub image: ImageChunk<'a>,
    /// The raw `ALPH` chunk payload (INCLUDING its 1-byte header), if present.
    pub alpha: Option<&'a [u8]>,
    /// The VP8X extended-header info, if the file used the extended format.
    pub vp8x: Option<Vp8xInfo>,
}

/// Like [`locate_image`], but also returns any sibling `ALPH` chunk and VP8X info.
///
/// Collects `ALPH` regardless of chunk order (it precedes `VP8 ` in well-formed
/// extended files). Still rejects animation with [`Error::UnsupportedFeature`].
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] for an animation, [`Error::InvalidContainer`]
/// for a malformed VP8X, [`Error::MissingImage`] when no image chunk is present,
/// or [`chunks`] errors on a truncated file.
pub fn locate_image_with_alpha(input: &[u8]) -> Result<LocatedImage<'_>> {
    let mut image: Option<ImageChunk<'_>> = None;
    let mut alpha: Option<&[u8]> = None;
    let mut vp8x: Option<Vp8xInfo> = None;
    for chunk in chunks(input)? {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8X => {
                let info = Vp8xInfo::parse(chunk.data)?;
                if info.flags.is_animated() {
                    return Err(Error::UnsupportedFeature);
                }
                vp8x = Some(info);
            },
            FourCc::VP8L if image.is_none() => image = Some(ImageChunk::Lossless(chunk.data)),
            FourCc::VP8 if image.is_none() => image = Some(ImageChunk::Lossy(chunk.data)),
            FourCc::ALPH => alpha = Some(chunk.data),
            FourCc::ANIM | FourCc::ANMF => return Err(Error::UnsupportedFeature),
            _ => {},
        }
    }
    let image = image.ok_or(Error::MissingImage)?;
    Ok(LocatedImage { image, alpha, vp8x })
}

/// Everything a single walk of a WebP container yields: the coded image chunk
/// (if any), a sibling `ALPH`, the `VP8X` header, sidecar metadata, and whether
/// the file is animated.
#[derive(Debug)]
pub struct ContainerContents<'a> {
    /// The first coded image chunk (`VP8L`/`VP8 `). `None` for an animation
    /// (whose frames live in `ANMF` chunks) or a file with no image chunk.
    pub image: Option<ImageChunk<'a>>,
    /// The raw `ALPH` chunk payload (INCLUDING its 1-byte header), if present.
    pub alpha: Option<&'a [u8]>,
    /// The `VP8X` extended-header info, if the file used the extended format.
    pub vp8x: Option<Vp8xInfo>,
    /// Sidecar metadata (empty unless `read_metadata` and the chunks exist).
    pub metadata: Metadata,
    /// Whether the file is animated — a `VP8X` animation flag, or an `ANIM`/`ANMF`
    /// chunk. When set, `image` is `None` and the frames are walked separately.
    pub animated: bool,
}

/// Walk a WebP container **once**, collecting the image chunk, any `ALPH`, the
/// `VP8X` header, sidecar metadata, and the animation marker.
///
/// This is the single primitive the umbrella decode path uses so a still file is
/// parsed exactly once (no separate locate + metadata-extract + codec re-parse).
/// Unlike [`parse_container`] / [`locate_image_with_alpha`], it does **not** reject
/// an animation: it reports `animated = true` and leaves `image = None`, letting
/// the caller route to the frame walker.
///
/// # Errors
///
/// [`Error::InvalidContainer`] for a malformed `VP8X`, or [`chunks`] errors on a
/// truncated file.
pub fn read_container(input: &[u8], read_metadata: bool) -> Result<ContainerContents<'_>> {
    let mut image: Option<ImageChunk<'_>> = None;
    let mut alpha: Option<&[u8]> = None;
    let mut vp8x: Option<Vp8xInfo> = None;
    let mut metadata = Metadata::none();
    let mut animated = false;
    for chunk in chunks(input)? {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8X => {
                let info = Vp8xInfo::parse(chunk.data)?;
                animated |= info.flags.is_animated();
                vp8x = Some(info);
            },
            FourCc::VP8L if image.is_none() => image = Some(ImageChunk::Lossless(chunk.data)),
            FourCc::VP8 if image.is_none() => image = Some(ImageChunk::Lossy(chunk.data)),
            FourCc::ALPH => alpha = Some(chunk.data),
            FourCc::ANIM | FourCc::ANMF => animated = true,
            FourCc::ICCP if read_metadata => metadata.icc_profile = Some(chunk.data.to_vec()),
            FourCc::EXIF if read_metadata => metadata.exif = Some(chunk.data.to_vec()),
            FourCc::XMP if read_metadata => metadata.xmp = Some(chunk.data.to_vec()),
            _ => {},
        }
    }
    Ok(ContainerContents {
        image,
        alpha,
        vp8x,
        metadata,
        animated,
    })
}

/// Locate the coded image of a still WebP and report whether it is lossless
/// (`VP8L`) or lossy (`VP8 `), returning its raw payload without decoding it.
///
/// This is the dispatch primitive the umbrella `webp` crate uses to route a file
/// to the `lossless` or `lossy` decoder. Unlike
/// [`parse_container`], it accepts both image kinds instead of rejecting `VP8 `,
/// and it does not read sidecar metadata. Animation is still rejected.
///
/// # Errors
///
/// [`Error::UnsupportedFeature`] for an animation, [`Error::InvalidContainer`]
/// for a malformed VP8X, [`Error::MissingImage`] when no image chunk is present,
/// or [`chunks`] errors on a truncated file.
pub fn locate_image(input: &[u8]) -> Result<ImageChunk<'_>> {
    locate_image_with_alpha(input).map(|located| located.image)
}

/// Whether `input` is an animated WebP: true iff the first `VP8X` chunk has the
/// animation flag set (or an `ANIM`/`ANMF` chunk appears before any `VP8X`).
///
/// A cheap header probe — it does not decode pixels. The umbrella `webp` crate
/// calls this to route an animated file to the animation path before falling
/// through to [`locate_image`] / [`parse_container`], which reject animation.
///
/// # Errors
///
/// [`chunks`] errors on a truncated file, or [`Error::InvalidContainer`] for a
/// malformed `VP8X`.
pub fn is_animated(input: &[u8]) -> Result<bool> {
    for chunk in chunks(input)? {
        let chunk = chunk?;
        match chunk.id {
            FourCc::VP8X => return Ok(Vp8xInfo::parse(chunk.data)?.flags.is_animated()),
            FourCc::ANIM | FourCc::ANMF => return Ok(true),
            _ => {},
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{
        ImageChunk, chunks, is_animated, locate_image, locate_image_with_alpha, parse_container,
    };
    use crate::Error;
    use crate::container::fourcc::FourCc;
    use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
    use crate::image::Dimensions;

    /// Locate the VP8L payload via `parse_container`, for the tests that exercise
    /// chunk iteration.
    fn vp8l_of(file: &[u8]) -> crate::Result<&[u8]> {
        parse_container(file, false).map(|p| p.vp8l)
    }

    /// Build a RIFF chunk: `FourCC` + LE size + payload + pad byte when odd.
    fn chunk(id: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&id);
        v.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    /// Wrap chunk bytes in a `RIFF....WEBP` envelope.
    fn webp(body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&u32::try_from(4 + body.len()).unwrap().to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn finds_vp8l_in_a_simple_file() {
        let file = webp(&chunk(*b"VP8L", &[1, 2, 3, 4]));
        assert_eq!(vp8l_of(&file).unwrap(), &[1, 2, 3, 4]);
    }

    #[test]
    fn handles_odd_size_padding_between_chunks() {
        let mut body = chunk(*b"VP8L", &[1, 2, 3]); // odd -> padded
        body.extend_from_slice(&chunk(*b"EXIF", &[9]));
        let file = webp(&body);
        assert_eq!(vp8l_of(&file).unwrap(), &[1, 2, 3]);
        let count = chunks(&file).unwrap().filter_map(Result::ok).count();
        assert_eq!(count, 2);
    }

    #[test]
    fn rejects_non_webp_magic() {
        let mut file = webp(&chunk(*b"VP8L", &[0]));
        file[0] = b'X'; // corrupt RIFF magic
        assert_eq!(vp8l_of(&file), Err(Error::NotWebp));
    }

    #[test]
    fn reports_truncated_chunk() {
        // Declare a 100-byte VP8L payload but supply only a few bytes.
        let mut body = Vec::new();
        body.extend_from_slice(b"VP8L");
        body.extend_from_slice(&100u32.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3]);
        let file = webp(&body);
        assert_eq!(vp8l_of(&file), Err(Error::Truncated));
    }

    #[test]
    fn fourcc_constants_match_ascii() {
        assert_eq!(FourCc::VP8L, FourCc(*b"VP8L"));
        assert_eq!(FourCc::RIFF, FourCc(*b"RIFF"));
    }

    #[test]
    fn parse_container_bare_vp8l_has_no_metadata() {
        let file = webp(&chunk(*b"VP8L", &[1, 2, 3, 4]));
        let parsed = parse_container(&file, true).unwrap();
        assert_eq!(parsed.vp8l, &[1, 2, 3, 4]);
        assert!(parsed.metadata.is_empty());
        assert!(parsed.vp8x.is_none());
    }

    #[test]
    fn parse_container_extracts_vp8x_metadata() {
        let flags = Vp8xFlags(0x20 | 0x08 | 0x04); // ICC + EXIF + XMP
        let vp8x = Vp8xInfo::build(flags, Dimensions::new(4, 4).unwrap());
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"ICCP", &[0xAA, 0xBB]));
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f]));
        body.extend_from_slice(&chunk(*b"EXIF", &[1, 2, 3]));
        body.extend_from_slice(&chunk(*b"XMP ", &[9]));
        let file = webp(&body);
        let parsed = parse_container(&file, true).unwrap();
        assert_eq!(parsed.vp8l, &[0x2f]);
        assert_eq!(
            parsed.metadata.icc_profile.as_deref(),
            Some(&[0xAA, 0xBB][..])
        );
        assert_eq!(parsed.metadata.exif.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(parsed.metadata.xmp.as_deref(), Some(&[9][..]));
        assert!(parsed.vp8x.unwrap().flags.has_icc());
    }

    #[test]
    fn parse_container_skips_metadata_when_disabled() {
        let mut body = chunk(*b"ICCP", &[1]);
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f]));
        let file = webp(&body);
        let parsed = parse_container(&file, false).unwrap();
        assert!(parsed.metadata.is_empty());
    }

    #[test]
    fn parse_container_rejects_animation_and_lossy() {
        let anim = webp(&chunk(*b"ANIM", &[0u8; 6]));
        assert_eq!(
            parse_container(&anim, true).unwrap_err(),
            Error::UnsupportedFeature
        );
        let lossy = webp(&chunk(*b"VP8 ", &[0, 1, 2, 3]));
        assert_eq!(
            parse_container(&lossy, true).unwrap_err(),
            Error::UnsupportedFeature
        );
    }

    #[test]
    fn read_chunk_at_returns_chunk_and_next_offset() {
        use super::read_chunk_at;
        let mut body = chunk(*b"VP8L", &[1, 2]); // header(8) + payload(2), even
        body.extend_from_slice(&chunk(*b"EXIF", &[9, 9]));
        // Read the second chunk, starting at the first chunk's length.
        let (c, next) = read_chunk_at(&body, 10).unwrap().unwrap();
        assert_eq!(c.id, FourCc::EXIF);
        assert_eq!(c.data, &[9, 9]);
        // offset + advance — kills `+ -> -`/`+ -> *` and the `-> Ok(None)` body.
        assert_eq!(next, body.len());
        // At/past the end: None.
        assert!(read_chunk_at(&body, body.len()).unwrap().is_none());
    }

    #[test]
    fn body_range_boundary_at_twelve_bytes() {
        use super::body_range;
        // 11 bytes is too short for the 12-byte header -> Truncated. Kills
        // `< -> ==` (which would index past an 11-byte buffer on the magic check).
        assert_eq!(body_range(&[0u8; 11]).unwrap_err(), Error::Truncated);
        // A valid 12-byte RIFF/WEBP with an empty body -> Ok((12, 12)). Kills
        // `< -> <=`, which would reject a valid 12-byte header as truncated.
        let empty = webp(&[]);
        assert_eq!(empty.len(), 12);
        assert_eq!(body_range(&empty).unwrap(), (12, 12));
    }

    #[test]
    fn parse_container_keeps_first_vp8l_and_honors_read_metadata() {
        let mut body = chunk(*b"VP8L", &[0x2f, 1]);
        body.extend_from_slice(&chunk(*b"EXIF", &[7]));
        body.extend_from_slice(&chunk(*b"XMP ", &[8]));
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f, 2]));
        let file = webp(&body);
        // First VP8L wins — kills the `image.is_none()` guard -> true (which would
        // keep the last).
        let parsed = parse_container(&file, true).unwrap();
        assert_eq!(parsed.vp8l, &[0x2f, 1]);
        assert_eq!(parsed.metadata.exif.as_deref(), Some(&[7][..]));
        assert_eq!(parsed.metadata.xmp.as_deref(), Some(&[8][..]));
        // read_metadata = false drops EXIF/XMP — kills those guards -> true.
        let parsed = parse_container(&file, false).unwrap();
        assert!(parsed.metadata.exif.is_none());
        assert!(parsed.metadata.xmp.is_none());
    }

    #[test]
    fn extract_metadata_collects_every_sidecar() {
        // Kills `extract_metadata -> Default` and each deleted ICCP/EXIF/XMP arm.
        let mut body = chunk(*b"ICCP", &[1, 1]);
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f]));
        body.extend_from_slice(&chunk(*b"EXIF", &[2, 2]));
        body.extend_from_slice(&chunk(*b"XMP ", &[3, 3]));
        let md = super::extract_metadata(&webp(&body));
        assert_eq!(md.icc_profile.as_deref(), Some(&[1, 1][..]));
        assert_eq!(md.exif.as_deref(), Some(&[2, 2][..]));
        assert_eq!(md.xmp.as_deref(), Some(&[3, 3][..]));
    }

    #[test]
    fn locate_image_with_alpha_keeps_the_first_image() {
        // Multiple image chunks: the first wins. A second VP8L exercises the
        // VP8L guard (kills its `image.is_none()` -> true, which would keep the
        // last), and a trailing VP8 exercises the VP8 guard likewise.
        let mut body = chunk(*b"VP8L", &[0x2f, 1]);
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f, 2]));
        body.extend_from_slice(&chunk(*b"VP8 ", &[3]));
        let file = webp(&body);
        let located = locate_image_with_alpha(&file).unwrap();
        assert_eq!(located.image, ImageChunk::Lossless(&[0x2f, 1][..]));
    }

    #[test]
    fn is_animated_detects_anmf_without_vp8x() {
        // An ANMF chunk before any VP8X means animated — kills the deleted arm.
        assert!(is_animated(&webp(&chunk(*b"ANMF", &[0u8; 8]))).unwrap());
        // A bare VP8L still is not animated.
        assert!(!is_animated(&webp(&chunk(*b"VP8L", &[0x2f]))).unwrap());
    }

    #[test]
    fn parse_container_rejects_vp8x_animation_flag() {
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x02), Dimensions::new(2, 2).unwrap());
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f]));
        assert_eq!(
            parse_container(&webp(&body), true).unwrap_err(),
            Error::UnsupportedFeature
        );
    }

    #[test]
    fn parse_container_missing_image() {
        let file = webp(&chunk(*b"ICCP", &[1, 2]));
        assert_eq!(
            parse_container(&file, true).unwrap_err(),
            Error::MissingImage
        );
    }

    #[test]
    fn locate_image_distinguishes_lossless_and_lossy() {
        let lossless = webp(&chunk(*b"VP8L", &[0x2f, 1, 2, 3]));
        assert_eq!(
            locate_image(&lossless).unwrap(),
            ImageChunk::Lossless(&[0x2f, 1, 2, 3])
        );
        let lossy = webp(&chunk(*b"VP8 ", &[9, 8, 7, 6]));
        assert_eq!(
            locate_image(&lossy).unwrap(),
            ImageChunk::Lossy(&[9, 8, 7, 6])
        );
    }

    #[test]
    fn locate_image_rejects_animation_but_reports_missing_and_lossy() {
        let anim = webp(&chunk(*b"ANIM", &[0u8; 6]));
        assert_eq!(locate_image(&anim).unwrap_err(), Error::UnsupportedFeature);
        let none = webp(&chunk(*b"ICCP", &[1, 2]));
        assert_eq!(locate_image(&none).unwrap_err(), Error::MissingImage);
    }

    #[test]
    fn locate_image_reads_lossy_from_an_extended_file() {
        // VP8X + VP8 payload: locate_image finds the lossy image and ignores VP8X.
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x10), Dimensions::new(4, 4).unwrap());
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"VP8 ", &[0xAA, 0xBB]));
        let file = webp(&body);
        assert_eq!(
            locate_image(&file).unwrap(),
            ImageChunk::Lossy(&[0xAA, 0xBB])
        );
    }

    #[test]
    fn locate_with_alpha_captures_alph_before_vp8() {
        // Well-formed extended order: VP8X, then ALPH, then the VP8 image.
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x10), Dimensions::new(4, 4).unwrap());
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"ALPH", &[0x01, 0xAA, 0xBB])); // header + data
        body.extend_from_slice(&chunk(*b"VP8 ", &[9, 8, 7]));
        let file = webp(&body);
        let located = locate_image_with_alpha(&file).unwrap();
        assert_eq!(located.image, ImageChunk::Lossy(&[9, 8, 7]));
        assert_eq!(located.alpha, Some(&[0x01, 0xAA, 0xBB][..]));
        assert!(located.vp8x.unwrap().flags.has_alpha());
    }

    #[test]
    fn locate_with_alpha_captures_alph_after_vp8() {
        // ALPH trailing the image chunk is still collected (order-independent).
        let mut body = chunk(*b"VP8 ", &[1, 2, 3, 4]);
        body.extend_from_slice(&chunk(*b"ALPH", &[0x00, 0x55]));
        let file = webp(&body);
        let located = locate_image_with_alpha(&file).unwrap();
        assert_eq!(located.image, ImageChunk::Lossy(&[1, 2, 3, 4]));
        assert_eq!(located.alpha, Some(&[0x00, 0x55][..]));
        assert!(located.vp8x.is_none());
    }

    #[test]
    fn locate_with_alpha_none_when_absent() {
        let file = webp(&chunk(*b"VP8L", &[0x2f, 1, 2, 3]));
        let located = locate_image_with_alpha(&file).unwrap();
        assert_eq!(located.image, ImageChunk::Lossless(&[0x2f, 1, 2, 3]));
        assert!(located.alpha.is_none());
        assert!(located.vp8x.is_none());
    }

    #[test]
    fn locate_with_alpha_still_rejects_animation() {
        let anim = webp(&chunk(*b"ANIM", &[0u8; 6]));
        assert_eq!(
            locate_image_with_alpha(&anim).unwrap_err(),
            Error::UnsupportedFeature
        );
    }

    #[test]
    fn is_animated_true_for_vp8x_animation_flag() {
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x02), Dimensions::new(4, 4).unwrap());
        let file = webp(&chunk(*b"VP8X", &vp8x));
        assert!(is_animated(&file).unwrap());
    }

    #[test]
    fn is_animated_false_for_still_vp8x() {
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x10), Dimensions::new(4, 4).unwrap()); // alpha, not anim
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"VP8L", &[0x2f]));
        let file = webp(&body);
        assert!(!is_animated(&file).unwrap());
    }

    #[test]
    fn is_animated_false_for_bare_vp8l() {
        let file = webp(&chunk(*b"VP8L", &[0x2f, 1, 2, 3]));
        assert!(!is_animated(&file).unwrap());
    }

    #[test]
    fn is_animated_errors_on_truncated() {
        // Declare a 100-byte VP8L payload but supply only a few bytes.
        let mut body = Vec::new();
        body.extend_from_slice(b"VP8L");
        body.extend_from_slice(&100u32.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3]);
        let file = webp(&body);
        assert_eq!(is_animated(&file).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn read_container_bare_vp8l_is_a_still_lossless_image() {
        use super::read_container;
        let file = webp(&chunk(*b"VP8L", &[0x2f, 1, 2, 3]));
        let c = read_container(&file, true).unwrap();
        assert_eq!(c.image, Some(ImageChunk::Lossless(&[0x2f, 1, 2, 3][..])));
        assert!(c.alpha.is_none());
        assert!(c.vp8x.is_none());
        assert!(c.metadata.is_empty());
        assert!(!c.animated);
    }

    #[test]
    fn read_container_collects_extended_lossy_alpha_and_metadata() {
        use super::read_container;
        // VP8X (alpha flag) + ALPH + VP8 + EXIF: one walk yields all of them.
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x10 | 0x08), Dimensions::new(4, 4).unwrap());
        let mut body = chunk(*b"VP8X", &vp8x);
        body.extend_from_slice(&chunk(*b"ALPH", &[0x01, 0xAA]));
        body.extend_from_slice(&chunk(*b"VP8 ", &[9, 8, 7]));
        body.extend_from_slice(&chunk(*b"EXIF", &[1, 2]));
        let file = webp(&body);
        let c = read_container(&file, true).unwrap();
        assert_eq!(c.image, Some(ImageChunk::Lossy(&[9, 8, 7][..])));
        assert_eq!(c.alpha, Some(&[0x01, 0xAA][..]));
        assert!(c.vp8x.unwrap().flags.has_alpha());
        assert_eq!(c.metadata.exif.as_deref(), Some(&[1, 2][..]));
        assert!(!c.animated);
        // read_metadata = false drops the sidecars but keeps the image/alpha/vp8x.
        let c = read_container(&file, false).unwrap();
        assert!(c.metadata.is_empty());
        assert_eq!(c.alpha, Some(&[0x01, 0xAA][..]));
    }

    #[test]
    fn read_container_marks_animation_without_erroring() {
        use super::read_container;
        // An ANMF chunk (no VP8X) marks the file animated and yields no still image,
        // unlike parse_container/locate which reject animation.
        let anmf = webp(&chunk(*b"ANMF", &[0u8; 8]));
        let c = read_container(&anmf, true).unwrap();
        assert!(c.animated);
        assert!(c.image.is_none());
        // A VP8X animation flag likewise marks it animated.
        let vp8x = Vp8xInfo::build(Vp8xFlags(0x02), Dimensions::new(4, 4).unwrap());
        let anim_vp8x = webp(&chunk(*b"VP8X", &vp8x));
        let c = read_container(&anim_vp8x, true).unwrap();
        assert!(c.animated);
    }

    #[test]
    fn chunks_reject_oversized_chunk_without_overflow() {
        // A chunk header declaring a ~4 GiB payload must yield Truncated, never
        // overflow `8 + size` (a panic on 32-bit debug builds).
        let mut body = Vec::new();
        body.extend_from_slice(b"VP8L");
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3]);
        let file = webp(&body);
        assert_eq!(parse_container(&file, false).unwrap_err(), Error::Truncated);
    }
}
