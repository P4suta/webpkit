//! In-process libwebp shims for the xtask `scoreboard` command's speed plane.
//!
//! The `unsafe` libwebp-sys FFI is isolated in this leaf crate so `xtask` keeps `#![forbid(unsafe_code)]` (the same split `webpkit-alloc-count` uses for its allocator).
//! Built empty unless the `enabled` feature links `libwebp-sys`; the scoreboard's libwebp columns are absent otherwise.

#[cfg(feature = "enabled")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "libwebp FFI takes/returns c_int dimensions and strides; the scoreboard \
              caps every edge via --max-edge so these bounded conversions cannot wrap"
)]
mod shims {
    /// libwebp's reported decoder version as `"major.minor.patch"` (decoded from the packed hex `WebPGetDecoderVersion`), so the scoreboard states exactly which libwebp its in-process timings ran against.
    #[must_use]
    pub fn decoder_version() -> String {
        // SAFETY: a nullary libwebp accessor returning a packed version int.
        let v = unsafe { libwebp_sys::WebPGetDecoderVersion() } as u32;
        format!("{}.{}.{}", (v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff)
    }

    fn checked_layout(
        len: usize,
        width: u32,
        height: u32,
        channels: usize,
    ) -> Option<(i32, i32, i32)> {
        if width == 0 || height == 0 {
            return None;
        }
        let width_usize = usize::try_from(width).ok()?;
        let height_usize = usize::try_from(height).ok()?;
        let expected = width_usize
            .checked_mul(height_usize)?
            .checked_mul(channels)?;
        if len != expected {
            return None;
        }
        let width_i32 = i32::try_from(width).ok()?;
        let height_i32 = i32::try_from(height).ok()?;
        let stride = i32::try_from(width_usize.checked_mul(channels)?).ok()?;
        Some((width_i32, height_i32, stride))
    }

    struct MemoryWriterGuard {
        raw: libwebp_sys::WebPMemoryWriter,
    }

    impl MemoryWriterGuard {
        fn initialized() -> Self {
            let mut raw = core::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
            // SAFETY: libwebp initializes every field in the provided writer storage.
            unsafe { libwebp_sys::WebPMemoryWriterInit(raw.as_mut_ptr()) };
            // SAFETY: the preceding call initialized every writer field.
            let raw = unsafe { raw.assume_init() };
            Self { raw }
        }
    }

    impl Drop for MemoryWriterGuard {
        fn drop(&mut self) {
            // SAFETY: `raw` was initialized by libwebp and this guard clears it exactly once.
            unsafe { libwebp_sys::WebPMemoryWriterClear(&raw mut self.raw) };
        }
    }

    struct PictureGuard {
        raw: libwebp_sys::WebPPicture,
    }

    impl PictureGuard {
        fn initialized() -> Result<Self, ()> {
            libwebp_sys::WebPPicture::new().map(|raw| Self { raw })
        }
    }

    impl Drop for PictureGuard {
        fn drop(&mut self) {
            // SAFETY: `raw` was initialized by libwebp and this guard frees it exactly once.
            unsafe { libwebp_sys::WebPPictureFree(&raw mut self.raw) };
        }
    }

    /// Losslessly encode `rgba` with libwebp's simple-API default.
    ///
    /// Returns the encoded byte length (the buffer is freed here) — for the encode-speed loop and the size column; lossless fidelity is exact, so the bytes themselves are never needed.
    #[must_use]
    pub fn encode_lossless_len(rgba: &[u8], width: u32, height: u32) -> usize {
        let Some((width, height, stride)) = checked_layout(rgba.len(), width, height, 4) else {
            return 0;
        };
        let mut out: *mut u8 = core::ptr::null_mut();
        // SAFETY: `rgba` holds `width*height*4` bytes at `stride`; libwebp writes the
        // freshly allocated stream pointer into `out`.
        let size = unsafe {
            libwebp_sys::WebPEncodeLosslessRGBA(rgba.as_ptr(), width, height, stride, &raw mut out)
        };
        if !out.is_null() {
            // SAFETY: `out` was allocated by libwebp and is freed exactly once here.
            unsafe { libwebp_sys::WebPFree(out.cast()) };
        }
        size
    }

    fn encode_lossless_best_impl(rgba: &[u8], width: u32, height: u32) -> usize {
        let Some((width_i32, height_i32, stride)) = checked_layout(rgba.len(), width, height, 4)
        else {
            return 0;
        };
        let Ok(mut config) = libwebp_sys::WebPConfig::new() else {
            return 0;
        };
        config.lossless = 1;
        config.quality = 100.0;
        config.method = 6;
        config.thread_level = 0;
        config.exact = 1;
        // SAFETY: `config` is initialized by libwebp and all assigned fields are
        // within the documented ranges.
        if unsafe { libwebp_sys::WebPValidateConfig(&raw const config) } == 0 {
            return 0;
        }

        // Declaration order is intentional: locals drop in reverse order, so the picture releases its imported planes before the writer clears its buffer.
        let mut writer = MemoryWriterGuard::initialized();
        let Ok(mut picture) = PictureGuard::initialized() else {
            return 0;
        };
        picture.raw.width = width_i32;
        picture.raw.height = height_i32;
        picture.raw.use_argb = 1;
        // SAFETY: `rgba` contains `width * height * 4` bytes at `stride`.
        if unsafe {
            libwebp_sys::WebPPictureImportRGBA(&raw mut picture.raw, rgba.as_ptr(), stride)
        } == 0
        {
            return 0;
        }

        picture.raw.writer = Some(libwebp_sys::WebPMemoryWrite);
        // `writer` was declared before `picture` and is not moved after this pointer capture, so `custom_ptr` remains valid through the synchronous encode.
        picture.raw.custom_ptr = (&raw mut writer.raw).cast();
        // SAFETY: config, picture, imported pixels, and writer remain live for this
        // synchronous call. The callback uses `custom_ptr` only while `WebPEncode`
        // is running and does not retain it.
        let ok = unsafe { libwebp_sys::WebPEncode(&raw const config, &raw mut picture.raw) };
        if ok != 0 && !writer.raw.mem.is_null() {
            writer.raw.size
        } else {
            0
        }
    }

    /// Losslessly encode `rgba` with the campaign reference settings:
    /// `-lossless -exact -m 6 -q 100`, with `thread_level = 0`.
    ///
    /// It reads the libwebp memory-writer length before clearing that buffer and performs no Rust output copy, so timing and process-peak measurements use the same output ownership basis as webpkit.
    #[must_use]
    pub fn encode_lossless_best_len(rgba: &[u8], width: u32, height: u32) -> usize {
        encode_lossless_best_impl(rgba, width, height)
    }

    /// Lossily encode `rgb` (`width*height*3` bytes) with libwebp's simple-API default shaping at `quality`, returning the full WebP container bytes (for the size + fidelity pass).
    /// Empty on encode failure.
    #[must_use]
    pub fn encode_lossy_bytes(rgb: &[u8], width: u32, height: u32, quality: f32) -> Vec<u8> {
        let Some((width, height, stride)) = checked_layout(rgb.len(), width, height, 3) else {
            return Vec::new();
        };
        if !quality.is_finite() || !(0.0..=100.0).contains(&quality) {
            return Vec::new();
        }
        let mut out: *mut u8 = core::ptr::null_mut();
        // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; libwebp writes the
        // freshly allocated stream pointer into `out`.
        let size = unsafe {
            libwebp_sys::WebPEncodeRGB(rgb.as_ptr(), width, height, stride, quality, &raw mut out)
        };
        if out.is_null() || size == 0 {
            return Vec::new();
        }
        // SAFETY: on success `out` points at `size` valid bytes owned by libwebp.
        let bytes = unsafe { core::slice::from_raw_parts(out, size) }.to_vec();
        // SAFETY: `out` was allocated by libwebp and is freed exactly once here.
        unsafe { libwebp_sys::WebPFree(out.cast()) };
        bytes
    }

    /// Lossily encode `rgb` at `quality`, returning the encoded byte length (the buffer is freed here).
    /// The allocation-free variant of [`encode_lossy_bytes`] for the encode-speed loop.
    #[must_use]
    pub fn encode_lossy_len(rgb: &[u8], width: u32, height: u32, quality: f32) -> usize {
        let Some((width, height, stride)) = checked_layout(rgb.len(), width, height, 3) else {
            return 0;
        };
        if !quality.is_finite() || !(0.0..=100.0).contains(&quality) {
            return 0;
        }
        let mut out: *mut u8 = core::ptr::null_mut();
        // SAFETY: `rgb` holds `width*height*3` bytes at `stride`; libwebp writes the
        // freshly allocated stream pointer into `out`.
        let size = unsafe {
            libwebp_sys::WebPEncodeRGB(rgb.as_ptr(), width, height, stride, quality, &raw mut out)
        };
        if !out.is_null() {
            // SAFETY: `out` was allocated by libwebp and is freed exactly once here.
            unsafe { libwebp_sys::WebPFree(out.cast()) };
        }
        size
    }

    /// Lossily encode opaque-or-alpha-bearing RGBA input and return only the libwebp-owned output length.
    ///
    /// The surpass resource gate supplies an all-opaque buffer to both codecs so their timed input layout and retained input bytes are identical.
    #[must_use]
    pub fn encode_lossy_rgba_len(rgba: &[u8], width: u32, height: u32, quality: f32) -> usize {
        let Some((width, height, stride)) = checked_layout(rgba.len(), width, height, 4) else {
            return 0;
        };
        if !quality.is_finite() || !(0.0..=100.0).contains(&quality) {
            return 0;
        }
        let mut out: *mut u8 = core::ptr::null_mut();
        // SAFETY: `rgba` holds exactly `width*height*4` bytes at `stride`;
        // libwebp writes the freshly allocated stream pointer into `out`.
        let size = unsafe {
            libwebp_sys::WebPEncodeRGBA(rgba.as_ptr(), width, height, stride, quality, &raw mut out)
        };
        if !out.is_null() {
            // SAFETY: `out` was allocated by libwebp and is freed exactly once.
            unsafe { libwebp_sys::WebPFree(out.cast()) };
        }
        size
    }

    /// Decode `webp` with libwebp to RGBA, returning the decoded byte length (the buffer is freed here).
    /// For the decode-speed loop; `0` on decode failure.
    #[must_use]
    pub fn decode_rgba_len(webp: &[u8]) -> usize {
        let (mut w, mut h) = (0i32, 0i32);
        // SAFETY: `webp`/`len` describe a valid buffer; libwebp allocates the output
        // and writes the dimensions through the out-pointers.
        let ptr = unsafe {
            libwebp_sys::WebPDecodeRGBA(webp.as_ptr(), webp.len(), &raw mut w, &raw mut h)
        };
        if ptr.is_null() {
            return 0;
        }
        let len = usize::try_from(w)
            .ok()
            .zip(usize::try_from(h).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .unwrap_or(0);
        // SAFETY: `ptr` was allocated by libwebp and is freed exactly once here.
        unsafe { libwebp_sys::WebPFree(ptr.cast()) };
        len
    }
}

#[cfg(feature = "enabled")]
pub use shims::{
    decode_rgba_len, decoder_version, encode_lossless_best_len, encode_lossless_len,
    encode_lossy_bytes, encode_lossy_len, encode_lossy_rgba_len,
};

#[cfg(all(test, feature = "enabled"))]
mod tests {
    use super::{
        encode_lossless_best_len, encode_lossless_len, encode_lossy_bytes, encode_lossy_len,
        encode_lossy_rgba_len,
    };

    #[test]
    fn lossless_best_rejects_invalid_rgba_dimensions_before_ffi() {
        assert_eq!(encode_lossless_best_len(&[], 1, 1), 0);
        assert_eq!(encode_lossless_best_len(&[0; 4], 0, 1), 0);
        assert_eq!(encode_lossless_best_len(&[0; 4], u32::MAX, 1), 0);
    }

    #[test]
    fn length_only_lossless_best_is_nonzero_and_deterministic() {
        let rgba = [17, 29, 41, 255, 53, 67, 79, 255];
        let len = encode_lossless_best_len(&rgba, 2, 1);
        assert_ne!(len, 0);
        assert_eq!(encode_lossless_best_len(&rgba, 2, 1), len);
    }

    #[test]
    fn simple_shims_reject_invalid_layouts_before_ffi() {
        assert_eq!(encode_lossless_len(&[0; 3], 1, 1), 0);
        assert!(encode_lossy_bytes(&[0; 2], 1, 1, 75.0).is_empty());
        assert_eq!(encode_lossy_len(&[0; 3], 0, 1, 75.0), 0);
        assert_eq!(encode_lossy_len(&[0; 3], 1, 1, f32::NAN), 0);
        assert_eq!(encode_lossy_rgba_len(&[0; 3], 1, 1, 75.0), 0);
    }

    #[test]
    fn opaque_rgba_and_rgb_lossy_inputs_have_the_same_encoded_length() {
        let rgb = [17, 29, 41, 53, 67, 79];
        let rgba = [17, 29, 41, 255, 53, 67, 79, 255];
        assert_eq!(
            encode_lossy_rgba_len(&rgba, 2, 1, 75.0),
            encode_lossy_len(&rgb, 2, 1, 75.0)
        );
    }
}
