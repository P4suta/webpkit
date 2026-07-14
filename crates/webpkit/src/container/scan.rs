//! Streaming (partial-buffer) RIFF scanning.
//!
//! The primitives a push-based decoder uses to classify and locate chunks
//! *before the whole file has arrived*. Unlike [`super::reader::chunks`], which
//! walks a complete RIFF body and requires each chunk's payload to be present,
//! these operate on a growing prefix: [`scan_chunks`] yields a chunk as soon as
//! its 8-byte header is buffered (its declared payload may still be incomplete),
//! and [`declared_len`] / [`is_complete`] report the RIFF envelope's size.
//!
//! This is the single source of truth for the overflow-safe `cursor <= len - 8`
//! chunk-header walk that the lossless/lossy incremental decoders and the umbrella
//! streaming classifier all share.

use super::fourcc::FourCc;

/// The total file length declared by the RIFF header (`8 + riff_size`), or `None`
/// if the 8-byte `RIFF<size>` prefix is not yet buffered.
#[must_use]
pub fn declared_len(bytes: &[u8]) -> Option<usize> {
    let s = bytes.get(4..8)?;
    Some(8usize.saturating_add(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize))
}

/// Whether `bytes` holds the whole declared RIFF file (a valid envelope whose
/// declared length is fully buffered).
#[must_use]
pub fn is_complete(bytes: &[u8]) -> bool {
    declared_len(bytes).is_some_and(|total| total >= 12 && bytes.len() >= total)
}

/// One chunk header located in a (possibly partial) RIFF buffer by [`scan_chunks`].
///
/// `payload_end`/`next` are *declared* offsets computed from the little-endian
/// size field; they may point past the buffered bytes when the chunk body has not
/// fully arrived. They are saturated, so a hostile size never overflows `usize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartialChunk {
    /// The chunk's four-character identifier.
    pub id: FourCc,
    /// Byte offset of the chunk payload (just past its 8-byte header).
    pub payload_start: usize,
    /// Declared payload end (`payload_start + size`); may exceed the buffer.
    pub payload_end: usize,
    /// Offset of the next chunk header (`payload_end + even-pad`), saturated.
    pub next: usize,
}

/// Walk the top-level chunk *headers* of a partial RIFF buffer, yielding each as
/// soon as its 8-byte header is buffered.
///
/// Uses the overflow-safe `cursor <= len - 8` bound (never `cursor + 8 <= len`, so
/// a saturated cursor from a hostile size cannot overflow) and assumes the 12-byte
/// `RIFF....WEBP` header, starting the walk at cursor 12 — the caller validates the
/// magic first. It does **not** require a chunk's payload to be present, so a
/// push-based decoder can classify/locate a chunk before its body arrives.
pub fn scan_chunks(bytes: &[u8]) -> impl Iterator<Item = PartialChunk> + '_ {
    let mut cursor = 12usize;
    core::iter::from_fn(move || {
        // `cursor <= len - 8` (via saturating_sub) keeps the four header reads and
        // `cursor + 8` in bounds for any buffer and any prior saturated cursor.
        if cursor > bytes.len().saturating_sub(8) {
            return None;
        }
        let id = FourCc([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]);
        let size = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        let payload_start = cursor + 8; // cursor <= len - 8, so this cannot overflow
        let payload_end = payload_start.saturating_add(size);
        let next = payload_end.saturating_add(size & 1);
        cursor = next;
        Some(PartialChunk {
            id,
            payload_start,
            payload_end,
            next,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{PartialChunk, declared_len, is_complete, scan_chunks};
    use crate::container::fourcc::FourCc;

    fn webp(body: &[u8]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&u32::try_from(4 + body.len()).unwrap().to_le_bytes());
        v.extend_from_slice(b"WEBP");
        v.extend_from_slice(body);
        v
    }

    fn chunk(id: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend_from_slice(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    #[test]
    fn declared_len_needs_eight_bytes_and_adds_the_header() {
        assert_eq!(declared_len(&[0u8; 7]), None);
        // riff_size = 4 -> total 12; saturating add never overflows on a hostile size.
        assert_eq!(declared_len(&webp(&[])), Some(12));
    }

    #[test]
    fn is_complete_gates_on_the_declared_size() {
        let file = webp(&chunk(*b"VP8L", &[0x2f, 1, 2, 3]));
        assert!(is_complete(&file));
        assert!(!is_complete(&file[..file.len() - 1]));
        assert!(!is_complete(&[0u8; 4]));
    }

    #[test]
    fn scan_chunks_yields_headers_before_payloads_arrive() {
        let mut body = chunk(*b"VP8X", &[0u8; 10]);
        body.extend_from_slice(&chunk(*b"VP8 ", &[9, 8, 7]));
        let file = webp(&body);
        let ids: Vec<FourCc> = scan_chunks(&file).map(|c| c.id).collect();
        assert_eq!(ids, vec![FourCc::VP8X, FourCc::VP8]);

        // Truncate to just past the VP8 header (its 3-byte payload absent): the
        // header is still yielded, with a declared end past the buffer.
        let vp8_header_end = file.len() - 3; // drop the 3 payload bytes + pad
        let partial = &file[..vp8_header_end];
        let last = scan_chunks(partial).last().unwrap();
        assert_eq!(last.id, FourCc::VP8);
        assert!(
            last.payload_end > partial.len(),
            "declared end past the buffer"
        );
    }

    #[test]
    fn scan_chunks_is_overflow_safe_on_a_hostile_size() {
        // A chunk header declaring a ~4 GiB payload must not overflow the cursor.
        let mut body = b"VP8 ".to_vec();
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&[1, 2, 3]);
        let file = webp(&body);
        let chunks: Vec<PartialChunk> = scan_chunks(&file).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, FourCc::VP8);
        // The saturated end/next never wrapped.
        assert!(chunks[0].payload_end >= file.len());
    }
}
