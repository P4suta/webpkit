//! The VP8L lossless bitstream codec (container-agnostic).
//!
//! Input is a raw VP8L payload (the bytes of a `VP8L` chunk, starting at the
//! `0x2f` signature); output is decoded ARGB pixels. This layer knows nothing
//! about RIFF framing.

pub(crate) mod backref;
pub(crate) mod decode;
pub(crate) mod decode_incr;
pub(crate) mod encode;
pub(crate) mod meta;
