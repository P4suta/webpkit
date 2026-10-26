//! Exercise revision parsing, normalization, and bounded prefixes.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = webpkit_ci_fuzz::exercise(data);
});
