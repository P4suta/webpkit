//! libwebp CLI orchestration: binary resolution, the pinned-version guard, and
//! the `cwebp`/`dwebp`/`webpmux`/`img2webp` invocations the fixture and
//! comparison paths shell out to.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Result, anyhow, bail};

/// The libwebp version every golden fixture must be produced with.
pub(crate) const REQUIRED_LIBWEBP: &str = "1.6.0";

/// Resolve the `cwebp` binary: `WEBPKIT_CWEBP` if set, else `cwebp` on PATH.
pub(crate) fn cwebp_bin() -> String {
    std::env::var("WEBPKIT_CWEBP").unwrap_or_else(|_| "cwebp".to_owned())
}

/// Resolve the `dwebp` binary: `WEBPKIT_DWEBP` if set, else `dwebp` on PATH.
pub(crate) fn dwebp_bin() -> String {
    std::env::var("WEBPKIT_DWEBP").unwrap_or_else(|_| "dwebp".to_owned())
}

/// Resolve the `webpmux` binary: `WEBPKIT_WEBPMUX` if set, else `webpmux` on PATH.
pub(crate) fn webpmux_bin() -> String {
    std::env::var("WEBPKIT_WEBPMUX").unwrap_or_else(|_| "webpmux".to_owned())
}

/// Resolve the `img2webp` binary: `WEBPKIT_IMG2WEBP` if set, else `img2webp` on PATH.
pub(crate) fn img2webp_bin() -> String {
    std::env::var("WEBPKIT_IMG2WEBP").unwrap_or_else(|_| "img2webp".to_owned())
}

/// The bail message used when a required libwebp tool is not on PATH.
fn not_found_msg(tool: &str, env: &str) -> String {
    format!("{tool} not found. Run via 'mise exec -- cargo xtask gen-fixtures' or set {env}.")
}

/// Run `cmd`, mapping a spawn `NotFound` error to the friendly `notfound_msg`.
fn output_or_notfound(cmd: &mut Command, notfound_msg: &str) -> Result<Output> {
    cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!("{notfound_msg}")
        } else {
            anyhow::Error::new(e).context("spawning external tool")
        }
    })
}

/// Assert `bin` reports libwebp `REQUIRED_LIBWEBP` on the first `-version` line.
///
/// `cwebp`/`dwebp`/`webpmux` print a bare `1.6.0`, while `img2webp` prints
/// `WebP Encoder version: 1.6.0`; comparing the last whitespace-separated token
/// of the first line accepts both.
pub(crate) fn check_version(bin: &str, tool: &str, env: &str) -> Result<()> {
    let mut cmd = Command::new(bin);
    cmd.arg("-version");
    let out = output_or_notfound(&mut cmd, &not_found_msg(tool, env))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or("").trim();
    let version = first.split_whitespace().next_back().unwrap_or(first);
    if version != REQUIRED_LIBWEBP {
        bail!(
            "{tool} reports version `{version}`, but webpkit::lossless fixtures require libwebp \
             {REQUIRED_LIBWEBP}. Use the mise-pinned toolchain, e.g. \
             'mise exec -- cargo xtask gen-fixtures', or point {env} at a \
             {REQUIRED_LIBWEBP} binary."
        );
    }
    Ok(())
}

/// Encode `src` losslessly to `out_webp` via cwebp (identity: `-lossless -exact`).
///
/// `resize` optionally caps the encoded width via cwebp `-resize <w> 0` (height
/// auto, aspect preserved); `None` encodes at the source's native resolution.
pub(crate) fn run_cwebp(
    cwebp: &str,
    src: &Path,
    out_webp: &Path,
    resize: Option<u32>,
) -> Result<()> {
    let mut cmd = Command::new(cwebp);
    cmd.arg("-lossless")
        .arg("-exact")
        .arg("-m")
        .arg("6")
        .arg("-q")
        .arg("100")
        .arg("-mt")
        .arg("0")
        .arg("-quiet");
    if let Some(width) = resize {
        cmd.arg("-resize").arg(width.to_string()).arg("0");
    }
    cmd.arg(src).arg("-o").arg(out_webp);
    let out = output_or_notfound(&mut cmd, &not_found_msg("cwebp", "WEBPKIT_CWEBP"))?;
    if !out.status.success() {
        bail!(
            "cwebp failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Decode `in_webp` to a PAM golden at `out_pam` via dwebp.
pub(crate) fn run_dwebp(dwebp: &str, in_webp: &Path, out_pam: &Path) -> Result<()> {
    let mut cmd = Command::new(dwebp);
    cmd.arg("-quiet")
        .arg(in_webp)
        .arg("-pam")
        .arg("-o")
        .arg(out_pam);
    let out = output_or_notfound(&mut cmd, &not_found_msg("dwebp", "WEBPKIT_DWEBP"))?;
    if !out.status.success() {
        bail!(
            "dwebp failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Attach one metadata chunk to `input` via `webpmux -set <kind> <data>`,
/// producing `output`. Each `-set` (re)wraps the file as an extended VP8X.
pub(crate) fn webpmux_set(
    webpmux: &str,
    kind: &str,
    data: &Path,
    input: &Path,
    output: &Path,
) -> Result<()> {
    let mut cmd = Command::new(webpmux);
    cmd.arg("-set")
        .arg(kind)
        .arg(data)
        .arg(input)
        .arg("-o")
        .arg(output);
    let out = output_or_notfound(&mut cmd, &not_found_msg("webpmux", "WEBPKIT_WEBPMUX"))?;
    if !out.status.success() {
        bail!(
            "webpmux -set {kind} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Build an animation from `frames` (PNG paths) via `img2webp`, lossless, 100 ms
/// per frame, looping forever. Each input is a full frame (no `-min_size`), so
/// libwebp keeps them as full-canvas `ANMF` frames.
pub(crate) fn run_img2webp(img2webp: &str, frames: &[PathBuf], out_webp: &Path) -> Result<()> {
    let mut cmd = Command::new(img2webp);
    cmd.arg("-loop")
        .arg("0")
        .arg("-lossless")
        .arg("-d")
        .arg("100");
    for frame in frames {
        cmd.arg(frame);
    }
    cmd.arg("-o").arg(out_webp);
    let out = output_or_notfound(&mut cmd, &not_found_msg("img2webp", "WEBPKIT_IMG2WEBP"))?;
    if !out.status.success() {
        bail!(
            "img2webp failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Extract the 1-based frame `index` from an animation via `webpmux -get frame`,
/// producing a standalone still WebP at `output`.
pub(crate) fn webpmux_get_frame(
    webpmux: &str,
    index: u32,
    input: &Path,
    output: &Path,
) -> Result<()> {
    let mut cmd = Command::new(webpmux);
    cmd.arg("-get")
        .arg("frame")
        .arg(index.to_string())
        .arg(input)
        .arg("-o")
        .arg(output);
    let out = output_or_notfound(&mut cmd, &not_found_msg("webpmux", "WEBPKIT_WEBPMUX"))?;
    if !out.status.success() {
        bail!(
            "webpmux -get frame {index} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Encode `src` to `out_webp` with libwebp `cwebp -q Q` (lossy). `-noalpha` keeps
/// the output a bare VP8 container (our encoder likewise drops alpha), and
/// `-mt 0`/`-quiet` keep it deterministic and silent.
pub(crate) fn run_cwebp_lossy(cwebp: &str, src: &Path, out_webp: &Path, quality: u8) -> Result<()> {
    let mut cmd = Command::new(cwebp);
    cmd.arg("-q")
        .arg(quality.to_string())
        .arg("-noalpha")
        .arg("-mt")
        .arg("0")
        .arg("-quiet")
        .arg(src)
        .arg("-o")
        .arg(out_webp);
    let out = output_or_notfound(&mut cmd, &not_found_msg("cwebp", "WEBPKIT_CWEBP"))?;
    if !out.status.success() {
        bail!(
            "cwebp (lossy) failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
