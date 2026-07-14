//! Fuzz target: feed arbitrary bytes to the webpkit-lossy VP8 row-streaming decoder in
//! data-derived chunk sizes.
//!
//! Run: `cargo +nightly fuzz run stream --fuzz-dir crates/webpkit-lossy-fuzz --features fuzzing`.
//! Gated on the `fuzzing` feature (which pulls in `libfuzzer-sys`); a normal
//! `cargo build --workspace` compiles this to an inert binary so the libFuzzer
//! runtime is never linked outside a fuzzing build.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Push the input in chunks whose sizes come from the data itself, so libFuzzer
    // explores many suspend/resume boundaries. No split may panic or hang, and
    // draining rows / finishing must stay sound on hostile input.
    let mut dec = webpkit::lossy::IncrementalDecoder::new();
    let mut off = 0;
    while off < data.len() {
        let step = 1 + (data[off] as usize & 0x0f);
        let end = (off + step).min(data.len());
        if dec.push(&data[off..end]).is_err() {
            break;
        }
        let _ = dec.drain_rows();
        off = end;
    }
    let _ = dec.into_image();
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
