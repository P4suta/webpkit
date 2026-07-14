//! Canonical Huffman decoding for VP8L.
//!
//! VP8L prefix codes are read LSB-first; libwebp builds a two-level, bit-reversed
//! lookup table so the decoder can index directly by the raw peeked bits. This
//! module ports that construction (`BuildHuffmanTable`) and the symbol read, plus
//! the simple/normal prefix-code and code-length-code readers.
pub(crate) mod build;
pub(crate) mod canonical;
pub(crate) mod decode;
pub(crate) mod serialize;
