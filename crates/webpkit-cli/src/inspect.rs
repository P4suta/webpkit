//! What a WebP file *is*, read from its container rather than guessed.
//!
//! Two things this deliberately does not do.
//!
//! It does not search the file for the bytes `VP8L`. A four-byte string can occur
//! anywhere in a compressed payload or an Exif blob, so a search reports the
//! codec of whatever it happens to hit first. The chunk walk asks the container.
//!
//! It does not decode pixels. Dimensions, alpha, and metadata all live in the
//! headers, so a report costs a few hundred bytes of parsing no matter how large
//! the image — and still works when the pixel data is truncated or corrupt,
//! which is exactly when someone runs `info`.

use serde::Serialize;
use webpkit::container::{
    anim::ANMF_HEADER_LEN,
    fourcc::FourCc,
    reader::{chunks, read_chunk_at},
};

use crate::error::CliError;

/// Which container form a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Container {
    /// A bare `VP8 `/`VP8L` chunk with no `VP8X` header.
    Simple,
    /// `VP8X` plus its advertised chunks (alpha, ICC, Exif, XMP).
    Extended,
    /// `VP8X` with `ANIM`/`ANMF` frames.
    Animation,
}

/// One RIFF chunk, as reported.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChunkInfo {
    /// The four-character tag, e.g. `VP8L`.
    pub(crate) fourcc: String,
    /// Payload length, excluding the header and any pad byte.
    pub(crate) bytes: usize,
}

/// Which metadata a file carries, and how much.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct Field {
    /// Whether the chunk is present.
    pub(crate) present: bool,
    /// Its payload length.
    pub(crate) bytes: usize,
}

/// The metadata chunks, whether or not each is present.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct MetadataInfo {
    /// `ICCP` — an ICC color profile.
    pub(crate) icc: Field,
    /// `EXIF`.
    pub(crate) exif: Field,
    /// `XMP `.
    pub(crate) xmp: Field,
}

impl MetadataInfo {
    /// Whether any metadata is present.
    pub(crate) const fn any(self) -> bool {
        self.icc.present || self.exif.present || self.xmp.present
    }
}

/// Animation-specific facts, absent for a still.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct AnimationInfo {
    /// Number of `ANMF` frames.
    pub(crate) frames: usize,
    /// `0` means forever.
    pub(crate) loop_count: u16,
    /// Sum of every frame's duration.
    pub(crate) duration_ms: u32,
}

/// Everything `webp info` knows about a file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    /// Bumped when a field changes meaning or disappears, so a script can pin it.
    pub(crate) schema: u32,
    /// The source label (a path, or `<stdin>`).
    pub(crate) path: String,
    /// File size in bytes.
    pub(crate) bytes: usize,
    /// Which container form.
    pub(crate) container: Container,
    /// `lossless` (VP8L) or `lossy` (VP8), from the container's image chunk.
    pub(crate) codec: &'static str,
    /// Canvas width.
    pub(crate) width: u32,
    /// Canvas height.
    pub(crate) height: u32,
    /// Whether alpha is used.
    pub(crate) alpha: bool,
    /// Metadata chunks.
    pub(crate) metadata: MetadataInfo,
    /// Animation facts, or `null` for a still.
    pub(crate) animation: Option<AnimationInfo>,
    /// Every chunk, in file order.
    pub(crate) chunks: Vec<ChunkInfo>,
}

/// Read `bytes` and describe it, without decoding any pixels.
///
/// # Errors
///
/// [`CliError::Codec`] if the file is not a readable WebP container.
pub(crate) fn report(bytes: &[u8], label: String) -> Result<Report, CliError> {
    // The probe comes first and is the only fallible step: it reads the header
    // without decoding, so it answers for a file whose body is damaged. Everything
    // after it degrades rather than fails, because a half-downloaded image is
    // precisely what someone runs `info` to understand.
    let info = webpkit::probe(bytes)?;
    let chunk_list = chunk_list(bytes);
    let animated = chunk_list.iter().any(|c| c.fourcc == "ANIM");

    Ok(Report {
        schema: 1,
        path: label,
        bytes: bytes.len(),
        container: container_of(&chunk_list, animated),
        codec: codec_name(bytes, &chunk_list, animated),
        width: info.dimensions.width(),
        height: info.dimensions.height(),
        alpha: info.has_alpha,
        metadata: metadata_of(&chunk_list),
        animation: animated.then(|| animation_of(bytes)).transpose()?,
        chunks: chunk_list,
    })
}

/// The container's chunks, in file order, for as far as they can be read.
///
/// Best-effort by design. A damaged envelope yields nothing and a truncated tail
/// ends the walk, but neither is an error: the chunks before the break are the
/// evidence, and refusing to print them because the last one is short would fail
/// exactly the file worth inspecting.
fn chunk_list(bytes: &[u8]) -> Vec<ChunkInfo> {
    let Ok(walk) = chunks(bytes) else {
        return Vec::new();
    };
    walk.map_while(Result::ok)
        .map(|chunk| ChunkInfo {
            fourcc: fourcc_str(chunk.id),
            bytes: chunk.data.len(),
        })
        .collect()
}

/// A `FourCc` as text, with the trailing space `VP8 ` and `XMP ` carry preserved
/// — it is part of the tag, and a chunk dump that trims it is lying about bytes.
fn fourcc_str(id: FourCc) -> String {
    String::from_utf8_lossy(&id.0).into_owned()
}

/// `lossless`, `lossy`, `mixed`, or `unknown`, from the container's image chunks.
///
/// `VP8L` and `VP8 ` are the only image chunks WebP defines, so their presence in
/// the walk is the answer — no search of the file body, which is what made the
/// old check report the codec of whatever four bytes it happened to hit first.
fn codec_name(bytes: &[u8], chunk_list: &[ChunkInfo], animated: bool) -> &'static str {
    if animated {
        return animation_codec(bytes);
    }
    let has = |tag: &str| chunk_list.iter().any(|c| c.fourcc == tag);
    match (has("VP8L"), has("VP8 ")) {
        (true, false) => "lossless",
        (false, true) => "lossy",
        // Both, or neither: the file is malformed or its image chunk was past a
        // truncation. `info` exists to report that, not to refuse.
        _ => "unknown",
    }
}

/// The codec used across an animation's frames.
///
/// Frames each carry their own image chunk and nothing requires them to agree —
/// libwebp will happily mux a lossless frame next to a lossy one — so this reads
/// every frame and says `mixed` rather than reporting whichever came first.
fn animation_codec(bytes: &[u8]) -> &'static str {
    let mut lossless = false;
    let mut lossy = false;
    let Ok(walk) = chunks(bytes) else {
        return "unknown";
    };
    for chunk in walk.map_while(Result::ok) {
        if fourcc_str(chunk.id) != "ANMF" {
            continue;
        }
        // An ANMF payload is a fixed frame header followed by that frame's own
        // chunks: an optional ALPH, then the image.
        let mut offset = ANMF_HEADER_LEN;
        while let Ok(Some((sub, next))) = read_chunk_at(chunk.data, offset) {
            match fourcc_str(sub.id).as_str() {
                "VP8L" => lossless = true,
                "VP8 " => lossy = true,
                _ => {},
            }
            offset = next;
        }
    }
    match (lossless, lossy) {
        (true, true) => "mixed",
        (true, false) => "lossless",
        (false, true) => "lossy",
        // No frame carried an image chunk: malformed, or truncated before the
        // first one. Say so rather than guess.
        (false, false) => "unknown",
    }
}

fn container_of(chunk_list: &[ChunkInfo], animated: bool) -> Container {
    if animated {
        Container::Animation
    } else if chunk_list.iter().any(|c| c.fourcc == "VP8X") {
        Container::Extended
    } else {
        Container::Simple
    }
}

fn metadata_of(chunk_list: &[ChunkInfo]) -> MetadataInfo {
    let find = |tag: &str| {
        chunk_list
            .iter()
            .find(|c| c.fourcc == tag)
            .map_or_else(Field::default, |c| Field {
                present: true,
                bytes: c.bytes,
            })
    };
    MetadataInfo {
        icc: find("ICCP"),
        exif: find("EXIF"),
        xmp: find("XMP "),
    }
}

fn animation_of(bytes: &[u8]) -> Result<AnimationInfo, CliError> {
    let frames = webpkit::decode_frames(bytes)?;
    let loop_count = frames.anim_info().loop_count;
    let mut count = 0;
    let mut duration_ms = 0;
    for frame in frames {
        duration_ms += frame?.meta().duration_ms;
        count += 1;
    }
    Ok(AnimationInfo {
        frames: count,
        loop_count,
        duration_ms,
    })
}
