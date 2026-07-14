//! Local, opt-in golden check: decode a real `.webp` with the `lossless` codec and compare it
//! byte-for-byte against libwebp `dwebp`'s output.
//!
//! `#[ignore]`d so it never runs in normal `cargo test`/CI (it needs local files
//! and libwebp). Run it explicitly with the golden files supplied via env:
//!
//! ```text
//! dwebp in.webp -pam -o golden.pam
//! WEBPKIT_WEBP=in.webp WEBPKIT_PAM=golden.pam \
//!   cargo test -p webpkit --test golden_local -- --ignored --exact decode_matches_dwebp
//! ```

#[test]
#[ignore = "opt-in: set WEBPKIT_WEBP and WEBPKIT_PAM to a .webp and its `dwebp -pam` output"]
fn decode_matches_dwebp() {
    let webp_path = std::env::var("WEBPKIT_WEBP").expect("WEBPKIT_WEBP must be set");
    let pam_path = std::env::var("WEBPKIT_PAM").expect("WEBPKIT_PAM must be set");

    let webp = std::fs::read(&webp_path).expect("read webp");
    let (dims, rgba) = webpkit::lossless::decode_rgba(&webp).expect("webpkit::lossless decode");
    let (width, height) = (dims.width(), dims.height());

    // Strip the PAM (P7) header, up to and including the "ENDHDR\n" line.
    let pam = std::fs::read(&pam_path).expect("read pam");
    let marker = b"ENDHDR\n";
    let header_end = pam
        .windows(marker.len())
        .position(|w| w == marker)
        .map(|p| p + marker.len())
        .expect("PAM ENDHDR marker");
    let golden = &pam[header_end..];

    assert_eq!(
        rgba.len(),
        golden.len(),
        "pixel buffer size mismatch: webpkit::lossless {}x{} = {} bytes, dwebp {} bytes",
        width,
        height,
        rgba.len(),
        golden.len()
    );
    if let Some(i) = rgba.iter().zip(golden).position(|(a, b)| a != b) {
        panic!(
            "first byte mismatch at offset {i} (pixel {}, channel {}): webpkit::lossless={} dwebp={}",
            i / 4,
            i % 4,
            rgba[i],
            golden[i]
        );
    }
}
