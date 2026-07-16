//! Netpbm binary PPM (`P6`, RGB) and PAM (`P7`, RGBA) I/O — dependency-free, for
//! `cwebp` PPM/PAM inputs and `dwebp -ppm`/`-pam` outputs.

use webpkit::{Dimensions, Image, Metadata, PixelLayout};

use crate::{error::CliError, format::to_rgba8};

/// Decode a binary PPM (`P6`) or PAM (`P7`) image into an RGBA8 [`Image`].
///
/// # Errors
///
/// [`CliError::Format`] if the header or body is malformed or uses an
/// unsupported feature (only 8-bit `MAXVAL 255` is supported).
pub(crate) fn read(bytes: &[u8]) -> Result<Image, CliError> {
    if bytes.starts_with(b"P6") {
        read_ppm(bytes)
    } else if bytes.starts_with(b"P7") {
        read_pam(bytes)
    } else {
        Err(CliError::Format(
            "not a binary PPM/PAM (P6/P7) file".to_owned(),
        ))
    }
}

/// Encode an [`Image`] as a binary PPM (`P6`, RGB; alpha dropped).
#[must_use]
pub(crate) fn write_ppm(image: &Image) -> Vec<u8> {
    let rgba = to_rgba8(image);
    let mut out = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
    out.reserve(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    out
}

/// Encode an [`Image`] as a binary PAM (`P7`, RGBA).
#[must_use]
pub(crate) fn write_pam(image: &Image) -> Vec<u8> {
    let rgba = to_rgba8(image);
    let header = format!(
        "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
        image.width(),
        image.height(),
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(&rgba);
    out
}

fn read_ppm(bytes: &[u8]) -> Result<Image, CliError> {
    let mut pos = 2;
    let width = next_uint(bytes, &mut pos)?;
    let height = next_uint(bytes, &mut pos)?;
    let maxval = next_uint(bytes, &mut pos)?;
    require_maxval_255(maxval)?;
    // A single whitespace byte separates the header from the pixel data.
    pos += 1;
    let data = bytes.get(pos..).unwrap_or(&[]);
    let rgba = pack_rgba(data, width, height, 3)?;
    finish(width, height, rgba)
}

fn read_pam(bytes: &[u8]) -> Result<Image, CliError> {
    let end = find(bytes, b"ENDHDR").ok_or_else(|| bad("PAM header missing ENDHDR"))?;
    let header = core::str::from_utf8(&bytes[..end]).map_err(|_| bad("PAM header is not UTF-8"))?;
    let (mut width, mut height, mut depth, mut maxval) = (None, None, None, None);
    for line in header.lines() {
        let mut it = line.split_ascii_whitespace();
        match (it.next(), it.next()) {
            (Some("WIDTH"), Some(v)) => width = Some(parse_uint(v)?),
            (Some("HEIGHT"), Some(v)) => height = Some(parse_uint(v)?),
            (Some("DEPTH"), Some(v)) => depth = Some(parse_uint(v)?),
            (Some("MAXVAL"), Some(v)) => maxval = Some(parse_uint(v)?),
            _ => {},
        }
    }
    let width = width.ok_or_else(|| bad("PAM header missing WIDTH"))?;
    let height = height.ok_or_else(|| bad("PAM header missing HEIGHT"))?;
    let depth = depth.ok_or_else(|| bad("PAM header missing DEPTH"))?;
    require_maxval_255(maxval.ok_or_else(|| bad("PAM header missing MAXVAL"))?)?;
    if depth != 3 && depth != 4 {
        return Err(bad("PAM DEPTH must be 3 (RGB) or 4 (RGBA)"));
    }
    // Data begins after "ENDHDR" and its trailing newline.
    let data = bytes.get(end + "ENDHDR".len() + 1..).unwrap_or(&[]);
    let rgba = pack_rgba(data, width, height, depth as usize)?;
    finish(width, height, rgba)
}

/// Pack `channels`-per-pixel 8-bit data (3=RGB, 4=RGBA) into RGBA8.
fn pack_rgba(data: &[u8], width: u32, height: u32, channels: usize) -> Result<Vec<u8>, CliError> {
    let pixels = width as usize * height as usize;
    let expected = pixels * channels;
    if data.len() < expected {
        return Err(CliError::Format(format!(
            "netpbm body is {} bytes but {width}x{height} at depth {channels} needs {expected}",
            data.len(),
        )));
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for px in data[..expected].chunks_exact(channels) {
        match channels {
            3 => rgba.extend_from_slice(&[px[0], px[1], px[2], 0xff]),
            _ => rgba.extend_from_slice(&[px[0], px[1], px[2], px[3]]),
        }
    }
    Ok(rgba)
}

fn finish(width: u32, height: u32, rgba: Vec<u8>) -> Result<Image, CliError> {
    let dims = Dimensions::new(width, height)?;
    let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 0xff);
    Ok(Image::from_parts(
        dims,
        PixelLayout::Rgba8,
        rgba,
        has_alpha,
        Metadata::none(),
    ))
}

/// Read the next unsigned integer, skipping ASCII whitespace and `#` comments,
/// leaving `pos` at the first byte after the digits.
fn next_uint(bytes: &[u8], pos: &mut usize) -> Result<u32, CliError> {
    while let Some(&b) = bytes.get(*pos) {
        if b == b'#' {
            while let Some(&c) = bytes.get(*pos) {
                *pos += 1;
                if c == b'\n' {
                    break;
                }
            }
        } else if b.is_ascii_whitespace() {
            *pos += 1;
        } else {
            break;
        }
    }
    let start = *pos;
    let mut value: u64 = 0;
    while let Some(&b) = bytes.get(*pos) {
        if b.is_ascii_digit() {
            value = value * 10 + u64::from(b - b'0');
            if value > u64::from(u32::MAX) {
                return Err(bad("netpbm integer out of range"));
            }
            *pos += 1;
        } else {
            break;
        }
    }
    if *pos == start {
        return Err(bad("expected an integer in the netpbm header"));
    }
    u32::try_from(value).map_err(|_| bad("netpbm integer out of range"))
}

fn parse_uint(text: &str) -> Result<u32, CliError> {
    text.parse()
        .map_err(|_| bad("invalid integer in PAM header"))
}

fn require_maxval_255(maxval: u32) -> Result<(), CliError> {
    if maxval == 255 {
        Ok(())
    } else {
        Err(bad("only 8-bit netpbm (MAXVAL 255) is supported"))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn bad(message: &str) -> CliError {
    CliError::Format(message.to_owned())
}
