//! PNG input/output via the `png` crate, normalized to RGBA8.
//!
//! Reading expands palette / low-bit grayscale and strips 16-bit down to 8-bit,
//! then converts grayscale / grayscale-alpha / RGB up to RGBA8 so the codec
//! always sees a uniform buffer. ICC / Exif / XMP metadata is bridged to and
//! from the WebP `VP8X` container.

use std::borrow::Cow;

use png::{BitDepth, ColorType, Info, Transformations, text_metadata::ITXtChunk};
use webpkit::lossless::{Dimensions, Image, Metadata, PixelLayout};

use crate::{error::CliError, format::to_rgba8};

/// The iTXt keyword under which XMP travels in a PNG.
const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

/// Decode a PNG file into an RGBA8 [`Image`], carrying any ICC/Exif/XMP metadata.
///
/// # Errors
///
/// [`CliError::Format`] if the bytes are not a decodable PNG.
pub fn read(bytes: &[u8]) -> Result<Image, CliError> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|err| CliError::Format(format!("invalid PNG: {err}")))?;
    let metadata = extract_metadata(reader.info(), bytes);
    let mut buf = vec![0_u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|err| CliError::Format(format!("invalid PNG: {err}")))?;
    buf.truncate(info.buffer_size());

    if info.bit_depth != BitDepth::Eight {
        return Err(CliError::Format(format!(
            "unsupported PNG bit depth {:?} (expected 8-bit after normalization)",
            info.bit_depth,
        )));
    }
    let rgba = expand_to_rgba8(&buf, info.color_type)?;
    let dims = Dimensions::new(info.width, info.height)?;
    let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 0xff);
    Ok(Image::from_parts(
        dims,
        PixelLayout::Rgba8,
        rgba,
        has_alpha,
        metadata,
    ))
}

/// Scan raw PNG chunks for an `eXIf` chunk's payload.
///
/// The `png` crate writes `eXIf` but its decoder does not surface it, so this
/// recovers Exif for the PNG-to-WebP path.
fn find_exif_chunk(png: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 8; // skip the 8-byte PNG signature
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &png[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len)?;
        if data_end.checked_add(4)? > png.len() || kind == b"IEND" {
            break;
        }
        if kind == b"eXIf" {
            return Some(png[data_start..data_end].to_vec());
        }
        pos = data_end + 4; // skip the 4-byte CRC
    }
    None
}

/// Encode an [`Image`] as an 8-bit RGBA PNG file, embedding `metadata`.
///
/// # Errors
///
/// [`CliError::Format`] if PNG encoding fails.
pub fn write(image: &Image, metadata: &Metadata) -> Result<Vec<u8>, CliError> {
    let rgba = to_rgba8(image);
    let mut info = Info::with_size(image.width(), image.height());
    info.color_type = ColorType::Rgba;
    info.bit_depth = BitDepth::Eight;
    if let Some(icc) = &metadata.icc_profile {
        info.icc_profile = Some(Cow::Borrowed(icc));
    }
    if let Some(exif) = &metadata.exif {
        info.exif_metadata = Some(Cow::Borrowed(exif));
    }
    if let Some(xmp) = &metadata.xmp {
        let text = String::from_utf8_lossy(xmp).into_owned();
        info.utf8_text.push(ITXtChunk::new(XMP_KEYWORD, text));
    }

    let mut out = Vec::new();
    {
        let encoder = png::Encoder::with_info(&mut out, info)
            .map_err(|err| CliError::Format(format!("PNG encode failed: {err}")))?;
        let mut writer = encoder
            .write_header()
            .map_err(|err| CliError::Format(format!("PNG encode failed: {err}")))?;
        writer
            .write_image_data(&rgba)
            .map_err(|err| CliError::Format(format!("PNG encode failed: {err}")))?;
    }
    Ok(out)
}

/// Pull ICC / Exif / XMP out of a decoded PNG's [`Info`] (plus the raw bytes,
/// for the `eXIf` chunk the `png` decoder skips).
fn extract_metadata(info: &Info<'_>, png: &[u8]) -> Metadata {
    let xmp = info.utf8_text.iter().find_map(|chunk| {
        (chunk.keyword == XMP_KEYWORD)
            .then(|| chunk.get_text().ok())
            .flatten()
            .map(String::into_bytes)
    });
    let exif = info
        .exif_metadata
        .as_ref()
        .map(|exif| exif.to_vec())
        .or_else(|| find_exif_chunk(png));
    Metadata {
        icc_profile: info.icc_profile.as_ref().map(|icc| icc.to_vec()),
        exif,
        xmp,
    }
}

/// Expand an 8-bit PNG frame of any color type to RGBA8.
fn expand_to_rgba8(buf: &[u8], color: ColorType) -> Result<Vec<u8>, CliError> {
    let rgba = match color {
        ColorType::Rgba => buf.to_vec(),
        ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 0xff])
            .collect(),
        ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 0xff]).collect(),
        ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|px| [px[0], px[0], px[0], px[1]])
            .collect(),
        ColorType::Indexed => {
            return Err(CliError::Format(
                "indexed PNG was not expanded to a direct color type".to_owned(),
            ));
        },
    };
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use webpkit::lossless::{Dimensions, Image, Metadata, PixelLayout};

    use super::{read, write};

    fn image_2x2() -> Image {
        Image::from_parts(
            Dimensions::new(2, 2).unwrap(),
            PixelLayout::Rgba8,
            (0..16).collect(),
            true,
            Metadata::none(),
        )
    }

    #[test]
    fn pixels_round_trip() {
        let png = write(&image_2x2(), &Metadata::none()).unwrap();
        let back = read(&png).unwrap();
        assert_eq!(back.width(), 2);
        assert_eq!(back.as_bytes(), (0..16).collect::<Vec<u8>>());
    }

    #[test]
    fn metadata_round_trips_through_png_chunks() {
        let meta = Metadata {
            icc_profile: Some(b"a fake icc profile".to_vec()),
            exif: Some(b"MM\x00*exif-bytes".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
        };
        let png = write(&image_2x2(), &meta).unwrap();
        let back = read(&png).unwrap();
        assert_eq!(back.metadata().icc_profile, meta.icc_profile);
        assert_eq!(back.metadata().exif, meta.exif);
        assert_eq!(back.metadata().xmp, meta.xmp);
    }
}
