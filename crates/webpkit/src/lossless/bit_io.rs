//! LSB-first bit-level I/O primitives shared by the VP8L decoder and encoder.
//!
//! VP8L is an LSB-first bitstream: within each byte the least-significant bit is
//! consumed first, and a multi-bit value is assembled with the first-read bit as
//! its least-significant bit. Getting this order wrong corrupts every symbol, so
//! the primitives live in one place with focused known-answer tests.
pub(crate) mod reader;
pub(crate) mod writer;
