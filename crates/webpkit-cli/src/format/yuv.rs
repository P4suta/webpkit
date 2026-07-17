//! YUV 4:2:0 plane output for `dwebp -yuv`/`-pgm`, matching libwebp's layouts.
//!
//! [`write_yuv`] emits the raw planar form (`Y`, then `U`, then `V`, no header);
//! [`write_pgm`] stacks the same planes into a grayscale PGM using libwebp's IMC4
//! layout (luma on top, `U`/`V` side by side below), so an image viewer can show
//! the samples directly. Both consume the lossy decoder's native [`YuvImage`].

use webpkit::YuvImage;

/// Encode a [`YuvImage`] as raw planar YUV: the packed `Y` plane, then `U`, then
/// `V`, with no header or row padding (libwebp's `-yuv` output).
#[must_use]
pub(crate) fn write_yuv(yuv: &YuvImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(yuv.y().len() + yuv.u().len() + yuv.v().len());
    out.extend_from_slice(yuv.y());
    out.extend_from_slice(yuv.u());
    out.extend_from_slice(yuv.v());
    out
}

/// Encode a [`YuvImage`] as a grayscale PGM in libwebp's IMC4 layout: the `Y`
/// plane on top (each row padded to an even width), then `uv_height` rows each
/// holding one `U` row immediately followed by its `V` row (libwebp's `-pgm`).
#[must_use]
pub(crate) fn write_pgm(yuv: &YuvImage) -> Vec<u8> {
    let width = yuv.width() as usize;
    let height = yuv.height() as usize;
    let uv_width = yuv.chroma_width() as usize;
    let uv_height = yuv.chroma_height() as usize;
    // A `U`+`V` row is `2 * uv_width` = `width` rounded up to even; the luma rows
    // pad to the same width so every PGM row is `out_width` bytes.
    let out_width = (width + 1) & !1;
    let mut out = format!("P5\n{out_width} {}\n255\n", height + uv_height).into_bytes();
    out.reserve(out_width * (height + uv_height));
    for row in yuv.y().chunks_exact(width) {
        out.extend_from_slice(row);
        if width & 1 == 1 {
            out.push(0);
        }
    }
    for (u_row, v_row) in yuv
        .u()
        .chunks_exact(uv_width)
        .zip(yuv.v().chunks_exact(uv_width))
    {
        out.extend_from_slice(u_row);
        out.extend_from_slice(v_row);
    }
    out
}
