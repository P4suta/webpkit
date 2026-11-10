//! Auditable real-image comparison against pinned libwebp (`surpass`).
//!
//! The report contains category rollups only.
//! Input paths, file names, and per-image measurements never cross the command boundary or enter the JSON.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::bench::{RealImage, RealPrep, prepare_real_images_max_edge};
use crate::common::{fnv1a64, workspace_root};
use crate::fixtures::write_png;
use crate::libwebp::{REQUIRED_LIBWEBP, check_version, cwebp_bin, verify_cwebp_image_hint};
use crate::metrics::{extract_vp8_chunk, psnr_rgb, ssim_rgb};

#[cfg(feature = "scoreboard-libwebp")]
mod lw {
    pub(super) const AVAILABLE: bool = true;
    pub(super) use webpkit_libwebp_timing::{
        decode_rgba_len, decoder_version, encode_lossless_best_len, encode_lossy_bytes,
        encode_lossy_rgba_len,
    };
}

#[cfg(not(feature = "scoreboard-libwebp"))]
mod lw {
    pub(super) const AVAILABLE: bool = false;
    pub(super) const fn decoder_version() -> String {
        String::new()
    }
    pub(super) const fn encode_lossless_best_len(_: &[u8], _: u32, _: u32) -> usize {
        0
    }
    pub(super) const fn encode_lossy_bytes(_: &[u8], _: u32, _: u32, _: f32) -> Vec<u8> {
        Vec::new()
    }
    pub(super) const fn encode_lossy_rgba_len(_: &[u8], _: u32, _: u32, _: f32) -> usize {
        0
    }
    pub(super) const fn decode_rgba_len(_: &[u8]) -> usize {
        0
    }
}

const LOSSY_BANDS: [u8; 3] = [50, 75, 90];
const CORPUS_WORKERS: usize = 4;
// Each image gets one deterministic warm-up per implementation followed by three AB/BA pairs (six timed samples per implementation).
const TIMING_PAIRS: u32 = 3;
const TIMING_WARMUPS_PER_IMPLEMENTATION: u32 = 1;
#[cfg(target_os = "windows")]
const WINDOWS_PEAK_OBSERVER: &str = r#"
$ErrorActionPreference = 'Stop'
$exePath = $env:WEBPKIT_SURPASS_WORKER_EXE
$p = Start-Process -FilePath $exePath -ArgumentList 'surpass-worker-env' -WindowStyle Hidden -PassThru
$peak = 0L
while (-not $p.HasExited) {
  try {
    $p.Refresh()
    $peak = [Math]::Max($peak, $p.PeakWorkingSet64)
  } catch {}
  Start-Sleep -Milliseconds 1
}
$p.WaitForExit()
Write-Output ("{0} {1}" -f $p.ExitCode, $peak)
exit $p.ExitCode
"#;

fn ensure_scalar_product_features() -> Result<()> {
    ensure!(
        !webpkit::INTERNAL_RAYON_ENABLED && !webpkit::INTERNAL_WORK_COUNT_ENABLED,
        "formal surpass measurement rejects resolved webpkit rayon and work-count features"
    );
    Ok(())
}

fn normalized_metadata(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(target_os = "windows")]
fn cpu_model() -> String {
    std::env::var("PROCESSOR_IDENTIFIER").map_or_else(
        |_| "unavailable".to_owned(),
        |value| normalized_metadata(&value),
    )
}

#[cfg(target_os = "linux")]
fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                matches!(key.trim(), "model name" | "Hardware").then(|| normalized_metadata(value))
            })
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn cpu_model() -> String {
    std::env::consts::ARCH.to_owned()
}

fn rust_toolchain() -> Result<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .arg("--version")
        .output()
        .context("querying rustc version")?;
    ensure!(output.status.success(), "rustc --version failed");
    let version = String::from_utf8(output.stdout).context("rustc version was not UTF-8")?;
    let version = normalized_metadata(&version);
    ensure!(!version.is_empty(), "rustc --version returned no version");
    Ok(version)
}

fn source_metadata(root: &Path) -> Result<(String, bool)> {
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("querying source revision")?;
    ensure!(revision.status.success(), "git rev-parse HEAD failed");
    let revision =
        normalized_metadata(&String::from_utf8(revision.stdout).context("revision was not UTF-8")?);
    ensure!(!revision.is_empty(), "git returned an empty revision");

    let status = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .context("querying source state")?;
    ensure!(status.status.success(), "git status failed");
    Ok((revision, !status.stdout.is_empty()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    FlatIcon,
    Transparent,
    Graphic,
    Photo,
}

impl Category {
    const ALL: [Self; 4] = [
        Self::FlatIcon,
        Self::Transparent,
        Self::Graphic,
        Self::Photo,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::FlatIcon => "flat-icon",
            Self::Transparent => "transparent",
            Self::Graphic => "graphic",
            Self::Photo => "photo",
        }
    }
}

fn classify(rgba: &[u8]) -> Category {
    let transparent = rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] < 255);
    let cap = if transparent { 4096 } else { 8192 };
    let mut colors = BTreeSet::new();
    let mut over = false;
    for pixel in rgba.as_chunks::<4>().0 {
        colors.insert([pixel[0], pixel[1], pixel[2]]);
        if colors.len() > cap {
            over = true;
            break;
        }
    }
    match (transparent, over) {
        (true, false) => Category::FlatIcon,
        (true, true) => Category::Transparent,
        (false, false) => Category::Graphic,
        (false, true) => Category::Photo,
    }
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

fn opaque_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut opaque = rgba.to_vec();
    for pixel in opaque.as_chunks_mut::<4>().0.iter_mut() {
        pixel[3] = 255;
    }
    opaque
}

fn wrap_vp8(payload: &[u8]) -> Vec<u8> {
    let pad = payload.len() & 1;
    let chunk_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let riff_len = u32::try_from(4 + 8 + payload.len() + pad).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(8 + usize::try_from(riff_len).unwrap_or(0));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WEBPVP8 ");
    out.extend_from_slice(&chunk_len.to_le_bytes());
    out.extend_from_slice(payload);
    if pad != 0 {
        out.push(0);
    }
    out
}

fn cwebp_lossy_reference(
    cwebp: &str,
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>> {
    let directory = tempfile::tempdir().context("creating anonymous lossy reference directory")?;
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.webp");
    let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
    for pixel in rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    write_png(&input, width, height, &rgba).context("writing anonymous lossy reference input")?;
    let quality = quality.to_string();
    let result = Command::new(cwebp)
        .args(["-quiet", "-q", &quality, "-noalpha"])
        .arg(&input)
        .args(["-o"])
        .arg(&output)
        .output()
        .context("running pinned cwebp lossy reference")?;
    ensure!(
        result.status.success(),
        "pinned cwebp lossy reference failed"
    );
    std::fs::read(output).context("reading anonymous cwebp lossy reference")
}

fn prepare_cwebp_lossy_references(cwebp: &str, image: &RealImage) -> Result<Vec<Vec<u8>>> {
    let rgb = rgba_to_rgb(&image.rgba);
    LOSSY_BANDS
        .into_iter()
        .map(|quality| {
            cwebp_lossy_reference(
                cwebp,
                &rgb,
                image.dims.width(),
                image.dims.height(),
                quality,
            )
        })
        .collect()
}

fn elapsed_ns(mut operation: impl FnMut() -> Result<usize>) -> Result<u64> {
    let start = Instant::now();
    std::hint::black_box(operation()?);
    Ok(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX))
}

/// Time paired A/B operations in alternating order and return each median.
fn paired_medians(
    mut ours: impl FnMut() -> Result<usize>,
    mut reference: impl FnMut() -> Result<usize>,
) -> Result<(u64, u64)> {
    for _ in 0..TIMING_WARMUPS_PER_IMPLEMENTATION {
        std::hint::black_box(ours()?);
        std::hint::black_box(reference()?);
    }
    let mut ours_ns = Vec::with_capacity((TIMING_PAIRS * 2) as usize);
    let mut reference_ns = Vec::with_capacity((TIMING_PAIRS * 2) as usize);
    for _ in 0..TIMING_PAIRS {
        ours_ns.push(elapsed_ns(&mut ours)?);
        reference_ns.push(elapsed_ns(&mut reference)?);
        reference_ns.push(elapsed_ns(&mut reference)?);
        ours_ns.push(elapsed_ns(&mut ours)?);
    }
    ours_ns.sort_unstable();
    reference_ns.sort_unstable();
    Ok((
        ours_ns[ours_ns.len() / 2],
        reference_ns[reference_ns.len() / 2],
    ))
}

fn ratio_permille(ours: u64, reference: u64) -> u32 {
    if reference == 0 {
        return u32::MAX;
    }
    let numerator = u128::from(ours) * 1000;
    let rounded_up = numerator.div_ceil(u128::from(reference));
    u32::try_from(rounded_up).unwrap_or(u32::MAX)
}

fn median(mut values: Vec<u32>) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

#[derive(Clone)]
struct LosslessMeasurement {
    ours_bytes: u64,
    reference_bytes: u64,
    ours_payload: Vec<u8>,
    encode_ratio_permille: Option<u32>,
    decode_ratio_permille: Option<u32>,
    ours_encode_peak_bytes: Option<u64>,
    ours_decode_peak_bytes: Option<u64>,
    reference_encode_peak_bytes: Option<u64>,
    reference_decode_peak_bytes: Option<u64>,
}

#[derive(Clone)]
struct LossyMeasurement {
    quality: u8,
    selected_quality: Option<u8>,
    ours_bytes: Option<u64>,
    reference_bytes: u64,
    selected_hash: Option<u64>,
    encode_ratio_permille: Option<u32>,
    decode_ratio_permille: Option<u32>,
    ours_encode_peak_bytes: Option<u64>,
    ours_decode_peak_bytes: Option<u64>,
    reference_encode_peak_bytes: Option<u64>,
    reference_decode_peak_bytes: Option<u64>,
}

#[derive(Clone)]
struct ImageMeasurement {
    lossless: LosslessMeasurement,
    lossy: Vec<LossyMeasurement>,
}

#[derive(Deserialize, Serialize)]
struct WorkerRequest {
    kind: String,
    input: PathBuf,
    width: u32,
    height: u32,
    quality: u8,
}

impl WorkerRequest {
    fn new(kind: &str, input: &Path, width: u32, height: u32, quality: u8) -> Self {
        Self {
            kind: kind.to_owned(),
            input: input.to_owned(),
            width,
            height,
            quality,
        }
    }

    #[cfg(target_os = "linux")]
    fn command_args(&self) -> Vec<String> {
        vec![
            "surpass-worker".to_owned(),
            "--kind".to_owned(),
            self.kind.clone(),
            "--input".to_owned(),
            self.input.as_os_str().to_string_lossy().into_owned(),
            "--width".to_owned(),
            self.width.to_string(),
            "--height".to_owned(),
            self.height.to_string(),
            "--quality".to_owned(),
            self.quality.to_string(),
        ]
    }
}

#[cfg(target_os = "windows")]
fn subprocess_peak_bytes(request: &WorkerRequest) -> Result<u64> {
    let exe = std::env::current_exe().context("resolving current xtask executable")?;
    // Poll the OS-maintained peak while the worker is alive.
    // The worker retains itself briefly after the measured operation, so even a fast decode has an observation window.
    // Both implementations use this exact executable and wrapper, making process baseline and sampling basis identical.
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_PEAK_OBSERVER,
        ])
        .env("WEBPKIT_SURPASS_WORKER_EXE", exe)
        .env(
            "WEBPKIT_SURPASS_WORKER_ARGS",
            serde_json::to_string(request).context("serializing worker request")?,
        )
        .output()
        .context("launching Windows peak-working-set observer")?;
    ensure!(
        output.status.success(),
        "surpass worker failed under peak-working-set observer: stdout=`{}` stderr=`{}`",
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim(),
    );
    let text = String::from_utf8(output.stdout).context("worker observer emitted non-UTF-8")?;
    let peak = text
        .split_whitespace()
        .last()
        .context("worker observer emitted no peak")?
        .parse::<u64>()
        .context("worker observer emitted an invalid peak")?;
    ensure!(peak > 0, "worker observer reported a zero peak");
    Ok(peak)
}

#[cfg(target_os = "linux")]
fn subprocess_peak_bytes(request: &WorkerRequest) -> Result<u64> {
    let exe = std::env::current_exe().context("resolving current xtask executable")?;
    let metrics = tempfile::NamedTempFile::new().context("creating peak-RSS output")?;
    let status = Command::new("/usr/bin/time")
        .args(["-f", "%x %M", "-o"])
        .arg(metrics.path())
        .arg(exe)
        .args(request.command_args())
        .status()
        .context("launching Linux peak-RSS observer")?;
    ensure!(
        status.success(),
        "surpass worker failed under peak-RSS observer"
    );
    let text = std::fs::read_to_string(metrics.path()).context("reading peak-RSS output")?;
    let kib = text
        .split_whitespace()
        .last()
        .context("peak-RSS observer emitted no peak")?
        .parse::<u64>()
        .context("peak-RSS observer emitted an invalid peak")?;
    ensure!(kib > 0, "peak-RSS observer reported a zero peak");
    Ok(kib.saturating_mul(1024))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn subprocess_peak_bytes(_: &WorkerRequest) -> Result<u64> {
    bail!("surpass OS peak memory is currently supported on Windows and Linux")
}

fn measure_worker_peak(
    kind: &str,
    input: &Path,
    width: u32,
    height: u32,
    quality: u8,
) -> Result<u64> {
    subprocess_peak_bytes(&WorkerRequest::new(kind, input, width, height, quality))
}

/// Execute a Windows peak-observer request transported as JSON in the environment.
/// Only a fixed, space-free subcommand crosses `Start-Process`'s lossy `-ArgumentList` boundary, so paths containing spaces remain one value.
#[cfg(target_os = "windows")]
pub(crate) fn worker_from_environment() -> Result<()> {
    let json = std::env::var("WEBPKIT_SURPASS_WORKER_ARGS")
        .context("WEBPKIT_SURPASS_WORKER_ARGS is missing")?;
    let request: WorkerRequest =
        serde_json::from_str(&json).context("deserializing worker request")?;
    worker(
        &request.kind,
        &request.input,
        request.width,
        request.height,
        request.quality,
    )
}

/// Execute one isolated operation for the parent `surpass` memory observer.
///
/// Both the safe-Rust and libwebp variants run in this same executable.
/// The brief hold after the operation leaves enough time for the parent to read the OS-maintained peak even for a very fast decode.
pub(crate) fn worker(kind: &str, input: &Path, width: u32, height: u32, quality: u8) -> Result<()> {
    ensure!(
        lw::AVAILABLE,
        "surpass worker requires the scoreboard-libwebp feature"
    );
    let bytes = std::fs::read(input).context("reading anonymous worker input")?;
    let measured = match kind {
        "webpkit-lossless-encode" => {
            let dims = webpkit::lossless::Dimensions::new(width, height)?;
            let image = webpkit::lossless::ImageRef::new(
                dims,
                webpkit::lossless::PixelLayout::Rgba8,
                &bytes,
            )?;
            let config = webpkit::lossless::EncoderConfig::new()
                .with_effort(webpkit::lossless::Effort::level(9));
            webpkit::lossless::encode(image, &config)?.len()
        },
        "libwebp-lossless-encode" => lw::encode_lossless_best_len(&bytes, width, height),
        "webpkit-lossy-encode" => {
            let dims = webpkit::lossy::Dimensions::new(width, height)?;
            let image =
                webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &bytes)?;
            let config = webpkit::lossy::LossyConfig::new()
                .with_quality(quality)
                .with_effort(webpkit::lossy::Effort::level(9));
            webpkit::lossy::encode_vp8(image, &config)?.1.len()
        },
        "libwebp-lossy-encode" => {
            lw::encode_lossy_rgba_len(&bytes, width, height, f32::from(quality))
        },
        "webpkit-lossless-decode" => webpkit::lossless::decode_rgba(&bytes)?.1.len(),
        "webpkit-lossy-decode" => webpkit::decode(&bytes)?.as_bytes().len(),
        "libwebp-decode" => lw::decode_rgba_len(&bytes),
        _ => bail!("unknown surpass worker operation"),
    };
    ensure!(measured > 0, "surpass worker operation failed");
    std::hint::black_box(measured);
    std::thread::sleep(Duration::from_millis(25));
    Ok(())
}

fn measure_lossless(image: &RealImage) -> Result<LosslessMeasurement> {
    let (width, height) = (image.dims.width(), image.dims.height());
    let input = webpkit::lossless::ImageRef::new(
        image.dims,
        webpkit::lossless::PixelLayout::Rgba8,
        &image.rgba,
    )?;
    let config =
        webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::level(9));

    let ours = webpkit::lossless::encode(input, &config)?;
    let reference_len = lw::encode_lossless_best_len(&image.rgba, width, height);
    ensure!(
        reference_len > 0,
        "libwebp lossless reference encode failed"
    );
    ensure!(
        u64::try_from(reference_len).unwrap_or(u64::MAX) == image.cwebp_size,
        "libwebp in-process and cwebp lossless settings disagree"
    );

    let decoded = webpkit::lossless::decode_rgba(&ours)?;
    std::hint::black_box(decoded.1.len());

    Ok(LosslessMeasurement {
        ours_bytes: u64::try_from(ours.len()).unwrap_or(u64::MAX),
        reference_bytes: image.cwebp_size,
        ours_payload: ours,
        encode_ratio_permille: None,
        decode_ratio_permille: None,
        ours_encode_peak_bytes: None,
        ours_decode_peak_bytes: None,
        reference_encode_peak_bytes: None,
        reference_decode_peak_bytes: None,
    })
}

fn populate_lossless_resources(
    image: &RealImage,
    measurement: &mut LosslessMeasurement,
) -> Result<()> {
    let (width, height) = (image.dims.width(), image.dims.height());
    let config =
        webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::level(9));
    let ours = &measurement.ours_payload;
    let memory_dir = tempfile::tempdir().context("creating anonymous memory worker directory")?;
    let raw_path = memory_dir.path().join("input.rgba");
    let webp_path = memory_dir.path().join("image.webp");
    std::fs::write(&raw_path, &image.rgba).context("writing anonymous RGBA worker input")?;
    std::fs::write(&webp_path, ours).context("writing anonymous WebP worker input")?;
    let ours_encode_peak_bytes =
        measure_worker_peak("webpkit-lossless-encode", &raw_path, width, height, 0)?;
    let reference_encode_peak_bytes =
        measure_worker_peak("libwebp-lossless-encode", &raw_path, width, height, 0)?;
    let ours_decode_peak_bytes =
        measure_worker_peak("webpkit-lossless-decode", &webp_path, width, height, 0)?;
    let reference_decode_peak_bytes =
        measure_worker_peak("libwebp-decode", &webp_path, width, height, 0)?;

    let (ours_encode_ns, reference_encode_ns) = paired_medians(
        || {
            let input = webpkit::lossless::ImageRef::new(
                image.dims,
                webpkit::lossless::PixelLayout::Rgba8,
                &image.rgba,
            )
            .expect("validated real image");
            Ok(webpkit::lossless::encode(input, &config)?.len())
        },
        || {
            let len = lw::encode_lossless_best_len(&image.rgba, width, height);
            ensure!(len > 0, "timed libwebp lossless encode failed");
            Ok(len)
        },
    )?;
    let (ours_decode_ns, reference_decode_ns) = paired_medians(
        || Ok(webpkit::lossless::decode_rgba(ours)?.1.len()),
        || {
            let len = lw::decode_rgba_len(ours);
            ensure!(len > 0, "timed libwebp lossless decode failed");
            Ok(len)
        },
    )?;

    measurement.encode_ratio_permille = Some(ratio_permille(ours_encode_ns, reference_encode_ns));
    measurement.decode_ratio_permille = Some(ratio_permille(ours_decode_ns, reference_decode_ns));
    measurement.ours_encode_peak_bytes = Some(ours_encode_peak_bytes);
    measurement.ours_decode_peak_bytes = Some(ours_decode_peak_bytes);
    measurement.reference_encode_peak_bytes = Some(reference_encode_peak_bytes);
    measurement.reference_decode_peak_bytes = Some(reference_decode_peak_bytes);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one RD transaction builds the complete curve and applies every matched-fidelity \
              reference band to it without relabelling quality"
)]
fn measure_lossy(image: &RealImage, cwebp_references: &[Vec<u8>]) -> Result<Vec<LossyMeasurement>> {
    #[derive(Clone, Copy)]
    struct RdPoint {
        quality: u8,
        bytes: usize,
        hash: u64,
        psnr: f64,
        ssim: f64,
    }

    let (width, height) = (image.dims.width(), image.dims.height());
    let (w, h) = (
        usize::try_from(width).unwrap_or(0),
        usize::try_from(height).unwrap_or(0),
    );
    let dims = webpkit::lossy::Dimensions::new(width, height)?;
    let rgb = rgba_to_rgb(&image.rgba);
    let opaque = opaque_rgba(&image.rgba);
    let mut bands = Vec::with_capacity(LOSSY_BANDS.len());

    // Build the complete webpkit RD curve once, before consulting any reference band.
    // The three q50/q75/q90 comparisons then select from the same points;
    // quality labels are never compared directly.
    let mut curve = Vec::with_capacity(101);
    for quality in 0..=100u8 {
        let config = webpkit::lossy::LossyConfig::new()
            .with_quality(quality)
            .with_effort(webpkit::lossy::Effort::level(9));
        let input =
            webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &opaque)?;
        let (_, payload) = webpkit::lossy::encode_vp8(input, &config)?;
        let decoded = webpkit::lossy::decode(&payload)?;
        curve.push(RdPoint {
            quality,
            bytes: payload.len(),
            hash: fnv1a64(&payload),
            psnr: psnr_rgb(&image.rgba, decoded.as_bytes()),
            ssim: ssim_rgb(&image.rgba, decoded.as_bytes(), w, h),
        });
    }

    ensure!(
        cwebp_references.len() == LOSSY_BANDS.len(),
        "incomplete anonymous cwebp lossy reference set"
    );
    for (quality, reference_file) in LOSSY_BANDS.into_iter().zip(cwebp_references) {
        let in_process_reference = lw::encode_lossy_bytes(&rgb, width, height, f32::from(quality));
        ensure!(
            !reference_file.is_empty() && !in_process_reference.is_empty(),
            "libwebp lossy reference encode failed"
        );
        let reference_payload = extract_vp8_chunk(reference_file)?;
        let in_process_payload = extract_vp8_chunk(&in_process_reference)?;
        ensure!(
            reference_payload == in_process_payload,
            "pinned cwebp and in-process libwebp lossy settings disagree"
        );
        let reference_decoded = webpkit::lossy::decode(reference_payload)?;
        let reference_psnr = psnr_rgb(&image.rgba, reference_decoded.as_bytes());
        let reference_ssim = ssim_rgb(&image.rgba, reference_decoded.as_bytes(), w, h);

        let selected = curve
            .iter()
            .copied()
            .filter(|point| point.psnr >= reference_psnr && point.ssim >= reference_ssim)
            .min_by_key(|point| (point.bytes, point.quality));
        let Some(selected) = selected else {
            bands.push(LossyMeasurement {
                quality,
                selected_quality: None,
                ours_bytes: None,
                reference_bytes: u64::try_from(reference_payload.len()).unwrap_or(u64::MAX),
                selected_hash: None,
                encode_ratio_permille: None,
                decode_ratio_permille: None,
                ours_encode_peak_bytes: None,
                ours_decode_peak_bytes: None,
                reference_encode_peak_bytes: None,
                reference_decode_peak_bytes: None,
            });
            continue;
        };

        let selected_quality = selected.quality;
        bands.push(LossyMeasurement {
            quality,
            selected_quality: Some(selected_quality),
            ours_bytes: Some(u64::try_from(selected.bytes).unwrap_or(u64::MAX)),
            reference_bytes: u64::try_from(reference_payload.len()).unwrap_or(u64::MAX),
            selected_hash: Some(selected.hash),
            encode_ratio_permille: None,
            decode_ratio_permille: None,
            ours_encode_peak_bytes: None,
            ours_decode_peak_bytes: None,
            reference_encode_peak_bytes: None,
            reference_decode_peak_bytes: None,
        });
    }
    Ok(bands)
}

fn populate_lossy_resources(image: &RealImage, bands: &mut [LossyMeasurement]) -> Result<()> {
    let (width, height) = (image.dims.width(), image.dims.height());
    let dims = webpkit::lossy::Dimensions::new(width, height)?;
    let opaque = opaque_rgba(&image.rgba);
    let memory_dir = tempfile::tempdir().context("creating anonymous memory worker directory")?;
    let raw_path = memory_dir.path().join("input.rgba");
    let webp_path = memory_dir.path().join("image.webp");
    std::fs::write(&raw_path, &opaque).context("writing anonymous opaque RGBA worker input")?;

    for band in bands {
        let (Some(selected_quality), Some(expected_bytes), Some(selected_hash)) =
            (band.selected_quality, band.ours_bytes, band.selected_hash)
        else {
            continue;
        };
        let config = webpkit::lossy::LossyConfig::new()
            .with_quality(selected_quality)
            .with_effort(webpkit::lossy::Effort::level(9));
        let input =
            webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, &opaque)?;
        let (_, payload) = webpkit::lossy::encode_vp8(input, &config)?;
        ensure!(
            u64::try_from(payload.len()).unwrap_or(u64::MAX) == expected_bytes
                && fnv1a64(&payload) == selected_hash,
            "lossy selected encode is not byte-deterministic",
        );
        let decoded = webpkit::lossy::decode(&payload)?;
        std::hint::black_box(decoded.as_bytes().len());

        let container = wrap_vp8(&payload);
        std::fs::write(&webp_path, &container).context("writing anonymous WebP worker input")?;
        let ours_encode_peak_bytes = measure_worker_peak(
            "webpkit-lossy-encode",
            &raw_path,
            width,
            height,
            selected_quality,
        )?;
        let reference_encode_peak_bytes = measure_worker_peak(
            "libwebp-lossy-encode",
            &raw_path,
            width,
            height,
            band.quality,
        )?;
        let ours_decode_peak_bytes =
            measure_worker_peak("webpkit-lossy-decode", &webp_path, width, height, 0)?;
        let reference_decode_peak_bytes =
            measure_worker_peak("libwebp-decode", &webp_path, width, height, 0)?;

        let (ours_encode_ns, reference_encode_ns) = paired_medians(
            || {
                let input = webpkit::lossy::ImageRef::new(
                    dims,
                    webpkit::lossy::PixelLayout::Rgba8,
                    &opaque,
                )
                .expect("validated real image");
                Ok(webpkit::lossy::encode_vp8(input, &config)?.1.len())
            },
            || {
                let len =
                    lw::encode_lossy_rgba_len(&opaque, width, height, f32::from(band.quality));
                ensure!(len > 0, "timed libwebp lossy encode failed");
                Ok(len)
            },
        )?;
        let (ours_decode_ns, reference_decode_ns) = paired_medians(
            || Ok(webpkit::decode(&container)?.as_bytes().len()),
            || {
                let len = lw::decode_rgba_len(&container);
                ensure!(len > 0, "timed libwebp lossy decode failed");
                Ok(len)
            },
        )?;

        band.encode_ratio_permille = Some(ratio_permille(ours_encode_ns, reference_encode_ns));
        band.decode_ratio_permille = Some(ratio_permille(ours_decode_ns, reference_decode_ns));
        band.ours_encode_peak_bytes = Some(ours_encode_peak_bytes);
        band.ours_decode_peak_bytes = Some(ours_decode_peak_bytes);
        band.reference_encode_peak_bytes = Some(reference_encode_peak_bytes);
        band.reference_decode_peak_bytes = Some(reference_decode_peak_bytes);
    }
    Ok(())
}

fn measure_image(
    category: Category,
    image: &RealImage,
    cwebp_references: &[Vec<u8>],
) -> Result<ImageMeasurement> {
    let lossless = measure_lossless(image)
        .with_context(|| format!("lossless {} category measurement", category.label()))?;
    let lossy = measure_lossy(image, cwebp_references)
        .with_context(|| format!("lossy {} category measurement", category.label()))?;
    Ok(ImageMeasurement { lossless, lossy })
}

fn populate_image_resources(
    category: Category,
    image: &RealImage,
    measurement: &mut ImageMeasurement,
) -> Result<()> {
    populate_lossless_resources(image, &mut measurement.lossless)
        .with_context(|| format!("lossless {} category resources", category.label()))?;
    populate_lossy_resources(image, &mut measurement.lossy)
        .with_context(|| format!("lossy {} category resources", category.label()))
}

#[derive(Serialize)]
struct Environment {
    os: &'static str,
    architecture: &'static str,
    cpu_model: String,
    rust_toolchain: String,
    source_revision: String,
    source_dirty: bool,
    debug_assertions: bool,
    logical_cpus: usize,
    corpus_workers: usize,
    timing_pairs: u32,
    timing_warmups_per_implementation: u32,
    timing_samples_per_implementation: u32,
    enabled_product_features: &'static str,
    webpkit_path: &'static str,
}

#[derive(Serialize)]
struct LosslessSummary {
    images: usize,
    resource_samples: usize,
    ratio_permille_median: Option<u32>,
    at_or_below_reference: usize,
    ours_bytes: u64,
    reference_bytes: u64,
    encode_time_ratio_permille_median: Option<u32>,
    decode_time_ratio_permille_median: Option<u32>,
    ours_encode_peak_bytes_max: Option<u64>,
    ours_decode_peak_bytes_max: Option<u64>,
    reference_encode_peak_bytes_max: Option<u64>,
    reference_decode_peak_bytes_max: Option<u64>,
    encode_peak_ratio_permille_max: Option<u32>,
    decode_peak_ratio_permille_max: Option<u32>,
}

#[derive(Serialize)]
struct LossySummary {
    reference_quality: u8,
    matched: usize,
    unmatched: usize,
    resource_samples: usize,
    ratio_permille_median: Option<u32>,
    ours_bytes: u64,
    reference_bytes: u64,
    selected_quality_median: Option<u32>,
    encode_time_ratio_permille_median: Option<u32>,
    decode_time_ratio_permille_median: Option<u32>,
    ours_encode_peak_bytes_max: Option<u64>,
    ours_decode_peak_bytes_max: Option<u64>,
    reference_encode_peak_bytes_max: Option<u64>,
    reference_decode_peak_bytes_max: Option<u64>,
    encode_peak_ratio_permille_max: Option<u32>,
    decode_peak_ratio_permille_max: Option<u32>,
}

#[derive(Serialize)]
struct CategorySummary {
    category: &'static str,
    images: usize,
    lossless: LosslessSummary,
    lossy: Vec<LossySummary>,
}

#[allow(
    clippy::too_many_lines,
    reason = "the serialized category schema is assembled in one place so every aggregate \
              is visibly derived from anonymous rows only"
)]
fn summarize(category: &'static str, rows: &[&ImageMeasurement]) -> CategorySummary {
    let lossless = LosslessSummary {
        images: rows.len(),
        resource_samples: rows
            .iter()
            .filter(|row| row.lossless.encode_ratio_permille.is_some())
            .count(),
        ratio_permille_median: median(
            rows.iter()
                .map(|row| ratio_permille(row.lossless.ours_bytes, row.lossless.reference_bytes))
                .collect(),
        ),
        at_or_below_reference: rows
            .iter()
            .filter(|row| row.lossless.ours_bytes <= row.lossless.reference_bytes)
            .count(),
        ours_bytes: rows.iter().map(|row| row.lossless.ours_bytes).sum(),
        reference_bytes: rows.iter().map(|row| row.lossless.reference_bytes).sum(),
        encode_time_ratio_permille_median: median(
            rows.iter()
                .filter_map(|row| row.lossless.encode_ratio_permille)
                .collect(),
        ),
        decode_time_ratio_permille_median: median(
            rows.iter()
                .filter_map(|row| row.lossless.decode_ratio_permille)
                .collect(),
        ),
        ours_encode_peak_bytes_max: rows
            .iter()
            .filter_map(|row| row.lossless.ours_encode_peak_bytes)
            .max(),
        ours_decode_peak_bytes_max: rows
            .iter()
            .filter_map(|row| row.lossless.ours_decode_peak_bytes)
            .max(),
        reference_encode_peak_bytes_max: rows
            .iter()
            .filter_map(|row| row.lossless.reference_encode_peak_bytes)
            .max(),
        reference_decode_peak_bytes_max: rows
            .iter()
            .filter_map(|row| row.lossless.reference_decode_peak_bytes)
            .max(),
        encode_peak_ratio_permille_max: rows
            .iter()
            .filter_map(|row| {
                row.lossless
                    .ours_encode_peak_bytes
                    .zip(row.lossless.reference_encode_peak_bytes)
                    .map(|(ours, reference)| ratio_permille(ours, reference))
            })
            .max(),
        decode_peak_ratio_permille_max: rows
            .iter()
            .filter_map(|row| {
                row.lossless
                    .ours_decode_peak_bytes
                    .zip(row.lossless.reference_decode_peak_bytes)
                    .map(|(ours, reference)| ratio_permille(ours, reference))
            })
            .max(),
    };

    let lossy = LOSSY_BANDS
        .into_iter()
        .map(|quality| {
            let measurements: Vec<&LossyMeasurement> = rows
                .iter()
                .filter_map(|row| row.lossy.iter().find(|band| band.quality == quality))
                .collect();
            let matched: Vec<&LossyMeasurement> = measurements
                .iter()
                .copied()
                .filter(|band| band.ours_bytes.is_some())
                .collect();
            LossySummary {
                reference_quality: quality,
                matched: matched.len(),
                unmatched: measurements.len() - matched.len(),
                resource_samples: matched
                    .iter()
                    .filter(|band| band.encode_ratio_permille.is_some())
                    .count(),
                ratio_permille_median: median(
                    matched
                        .iter()
                        .filter_map(|band| {
                            band.ours_bytes
                                .map(|ours| ratio_permille(ours, band.reference_bytes))
                        })
                        .collect(),
                ),
                ours_bytes: matched.iter().filter_map(|band| band.ours_bytes).sum(),
                reference_bytes: matched.iter().map(|band| band.reference_bytes).sum(),
                selected_quality_median: median(
                    matched
                        .iter()
                        .filter_map(|band| band.selected_quality.map(u32::from))
                        .collect(),
                ),
                encode_time_ratio_permille_median: median(
                    matched
                        .iter()
                        .filter_map(|band| band.encode_ratio_permille)
                        .collect(),
                ),
                decode_time_ratio_permille_median: median(
                    matched
                        .iter()
                        .filter_map(|band| band.decode_ratio_permille)
                        .collect(),
                ),
                ours_encode_peak_bytes_max: matched
                    .iter()
                    .filter_map(|band| band.ours_encode_peak_bytes)
                    .max(),
                ours_decode_peak_bytes_max: matched
                    .iter()
                    .filter_map(|band| band.ours_decode_peak_bytes)
                    .max(),
                reference_encode_peak_bytes_max: matched
                    .iter()
                    .filter_map(|band| band.reference_encode_peak_bytes)
                    .max(),
                reference_decode_peak_bytes_max: matched
                    .iter()
                    .filter_map(|band| band.reference_decode_peak_bytes)
                    .max(),
                encode_peak_ratio_permille_max: matched
                    .iter()
                    .filter_map(|band| {
                        band.ours_encode_peak_bytes
                            .zip(band.reference_encode_peak_bytes)
                            .map(|(ours, reference)| ratio_permille(ours, reference))
                    })
                    .max(),
                decode_peak_ratio_permille_max: matched
                    .iter()
                    .filter_map(|band| {
                        band.ours_decode_peak_bytes
                            .zip(band.reference_decode_peak_bytes)
                            .map(|(ours, reference)| ratio_permille(ours, reference))
                    })
                    .max(),
            }
        })
        .collect();
    CategorySummary {
        category,
        images: rows.len(),
        lossless,
        lossy,
    }
}

#[derive(Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent externally reported campaign gate outcomes, not states"
)]
struct Gates {
    lossless_size: bool,
    lossy_size_and_fidelity: bool,
    encode_speed: bool,
    decode_speed: bool,
    same_executable_peak_memory: Option<bool>,
    measurement_complete: bool,
}

const fn measurement_complete(skipped: usize, gates: [bool; 5]) -> bool {
    skipped == 0 && gates[0] && gates[1] && gates[2] && gates[3] && gates[4]
}

const fn sample_set_complete(images: usize, resource_samples: usize) -> bool {
    resource_samples == images
}

const fn lossy_sample_set_complete(
    images: usize,
    matched: usize,
    unmatched: usize,
    resource_samples: usize,
) -> bool {
    unmatched == 0 && matched == images && resource_samples == images
}

fn resource_coverage_complete(overall: &CategorySummary, categories: &[CategorySummary]) -> bool {
    sample_set_complete(overall.lossless.images, overall.lossless.resource_samples)
        && overall.lossy.iter().all(|band| {
            lossy_sample_set_complete(
                overall.images,
                band.matched,
                band.unmatched,
                band.resource_samples,
            )
        })
        && categories.iter().all(|category| {
            sample_set_complete(category.lossless.images, category.lossless.resource_samples)
                && category.lossy.iter().all(|band| {
                    lossy_sample_set_complete(
                        category.images,
                        band.matched,
                        band.unmatched,
                        band.resource_samples,
                    )
                })
        })
}

#[derive(Serialize)]
struct Report {
    schema: u32,
    libwebp_version: &'static str,
    reference_lossless: &'static str,
    reference_lossy: &'static str,
    fidelity: &'static str,
    privacy: &'static str,
    max_edge: u32,
    images: usize,
    skipped_entries: usize,
    resource_sampling: &'static str,
    resource_samples: usize,
    lossy_input_layout: &'static str,
    environment: Environment,
    categories: Vec<CategorySummary>,
    overall: CategorySummary,
    gates: Gates,
}

/// Run the anonymous, matched-fidelity real-image comparison and save its latest local-only report under `target/surpass/latest.json`.
#[allow(
    clippy::too_many_lines,
    reason = "campaign orchestration keeps reference validation, anonymous aggregation, and all \
              completion gates in one auditable transaction"
)]
pub(crate) fn surpass(real: &Path, max_edge: u32) -> Result<()> {
    ensure!(
        max_edge == 1024,
        "formal surpass measurement requires --max-edge 1024"
    );
    ensure!(
        !cfg!(debug_assertions),
        "formal surpass measurement requires a --release build"
    );
    ensure_scalar_product_features()?;
    let root = workspace_root()?;
    let path = root.join("target/surpass/latest.json");
    let parent = path.parent().context("surpass report has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    match std::fs::remove_file(&path) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => {
            return Err(error).with_context(|| format!("invalidating {}", path.display()));
        },
    }
    if !lw::AVAILABLE {
        bail!(
            "surpass requires the pinned in-process libwebp reference; rerun with \
             `--features scoreboard-libwebp`"
        );
    }
    let cwebp = cwebp_bin();
    check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP")?;
    verify_cwebp_image_hint(&cwebp)?;
    ensure!(
        lw::decoder_version() == REQUIRED_LIBWEBP,
        "linked libwebp reports {}, required {REQUIRED_LIBWEBP}",
        lw::decoder_version()
    );
    let rust_toolchain = rust_toolchain()?;
    let (source_revision, source_dirty) = source_metadata(&root)?;

    let prepared = prepare_real_images_max_edge(&cwebp, real, max_edge)
        .map_err(|_| anyhow!("surpass failed to prepare the anonymous input corpus"))?;
    let mut images: Vec<(Category, RealImage)> = Vec::new();
    let mut skipped = 0usize;
    for item in prepared {
        let RealPrep::Ready(image) = item else {
            skipped += 1;
            continue;
        };
        images.push((classify(&image.rgba), image));
    }
    ensure!(!images.is_empty(), "surpass found no decodable images");

    // Validate every external reference before starting any expensive 101-point RD curve.
    // A missing/failed cwebp transaction therefore fails early instead of discarding hours of otherwise valid anonymous corpus work.
    let mut cwebp_references = Vec::with_capacity(images.len());
    for (_, image) in &images {
        cwebp_references.push(
            prepare_cwebp_lossy_references(&cwebp, image)
                .map_err(|_| anyhow!("surpass failed to validate anonymous cwebp references"))?,
        );
    }

    // Independent image size/RD transactions run in a fixed four-worker wave.
    // Each codec invocation remains portable scalar; this only shortens corpus wall time and cannot affect bytes, fidelity selection, or row ordering.
    let workers = images.len().min(CORPUS_WORKERS);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let sender = sender.clone();
            let images = &images;
            let cwebp_references = &cwebp_references;
            scope.spawn(move || {
                for index in (worker..images.len()).step_by(workers) {
                    let (category, image) = &images[index];
                    if sender
                        .send((
                            index,
                            measure_image(*category, image, &cwebp_references[index]),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    drop(sender);

    let mut measured = Vec::with_capacity(images.len());
    measured.resize_with(images.len(), || None);
    for _ in 0..images.len() {
        let (index, measurement) = receiver
            .recv()
            .context("receiving anonymous corpus measurement")?;
        measured[index] = Some(measurement?);
    }

    // Populate timing and process peaks for every image without rebuilding its 101-point RD curve.
    // These transactions are serial so one sample cannot perturb another sample's paired A/B timing or OS peak observation.
    for index in 0..images.len() {
        let (category, image) = &images[index];
        let measurement = measured[index]
            .as_mut()
            .expect("every anonymous image completed size/RD measurement");
        populate_image_resources(*category, image, measurement)?;
    }
    let rows: Vec<(Category, ImageMeasurement)> = images
        .iter()
        .zip(measured)
        .map(|((category, _), measurement)| {
            (
                *category,
                measurement.expect("every anonymous image is measured"),
            )
        })
        .collect();

    let categories: Vec<CategorySummary> = Category::ALL
        .into_iter()
        .filter_map(|category| {
            let selected: Vec<&ImageMeasurement> = rows
                .iter()
                .filter_map(|(row_category, row)| (*row_category == category).then_some(row))
                .collect();
            (!selected.is_empty()).then(|| summarize(category.label(), &selected))
        })
        .collect();
    let all: Vec<&ImageMeasurement> = rows.iter().map(|(_, row)| row).collect();
    let overall = summarize("overall", &all);

    let lossless_size = overall
        .lossless
        .ratio_permille_median
        .is_some_and(|v| v <= 1005)
        && overall.lossless.at_or_below_reference * 2 >= overall.lossless.images;
    let lossy_overall = overall
        .lossy
        .iter()
        .all(|band| band.unmatched == 0 && band.ratio_permille_median.is_some_and(|v| v <= 1050));
    let lossy_photo = categories
        .iter()
        .find(|category| category.category == "photo")
        .is_some_and(|photo| {
            photo.lossy.iter().all(|band| {
                band.unmatched == 0 && band.ratio_permille_median.is_some_and(|v| v <= 1030)
            })
        });
    let resource_complete = resource_coverage_complete(&overall, &categories);
    let encode_speed = resource_complete
        && overall
            .lossless
            .encode_time_ratio_permille_median
            .is_some_and(|v| v <= 2000)
        && overall.lossy.iter().all(|band| {
            band.encode_time_ratio_permille_median
                .is_some_and(|v| v <= 2000)
        });
    let decode_speed = resource_complete
        && overall
            .lossless
            .decode_time_ratio_permille_median
            .is_some_and(|v| v <= 1500)
        && overall.lossy.iter().all(|band| {
            band.decode_time_ratio_permille_median
                .is_some_and(|v| v <= 1500)
        });
    let peak_memory = resource_complete
        && overall
            .lossless
            .encode_peak_ratio_permille_max
            .is_some_and(|ratio| ratio <= 1500)
        && overall
            .lossless
            .decode_peak_ratio_permille_max
            .is_some_and(|ratio| ratio <= 1500)
        && overall.lossy.iter().all(|band| {
            band.encode_peak_ratio_permille_max
                .is_some_and(|ratio| ratio <= 1500)
                && band
                    .decode_peak_ratio_permille_max
                    .is_some_and(|ratio| ratio <= 1500)
        });
    let lossy_size_and_fidelity = lossy_overall && lossy_photo;
    let complete = measurement_complete(
        skipped,
        [
            lossless_size,
            lossy_size_and_fidelity,
            encode_speed,
            decode_speed,
            peak_memory,
        ],
    );
    let gates = Gates {
        lossless_size,
        lossy_size_and_fidelity,
        encode_speed,
        decode_speed,
        same_executable_peak_memory: Some(peak_memory),
        measurement_complete: complete,
    };
    let report = Report {
        schema: 4,
        libwebp_version: REQUIRED_LIBWEBP,
        reference_lossless: "cwebp -lossless -exact -m 6 -q 100 (single-threaded: -mt omitted)",
        reference_lossy: "cwebp -q {50,75,90} -noalpha (single-threaded: -mt omitted)",
        fidelity: "minimum webpkit bytes with RGB-PSNR and SSIM both >= libwebp",
        privacy: "category aggregates only; no path, file name, or per-image result",
        max_edge,
        images: rows.len(),
        skipped_entries: skipped,
        resource_sampling: "every recognized image, serially after the fixed four-worker size/RD phase",
        resource_samples: rows.len(),
        lossy_input_layout: "opaque RGBA8 for both encoders; alpha excluded",
        environment: Environment {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            cpu_model: cpu_model(),
            rust_toolchain,
            source_revision,
            source_dirty,
            debug_assertions: cfg!(debug_assertions),
            logical_cpus: std::thread::available_parallelism().map_or(1, usize::from),
            corpus_workers: workers,
            timing_pairs: TIMING_PAIRS,
            timing_warmups_per_implementation: TIMING_WARMUPS_PER_IMPLEMENTATION,
            timing_samples_per_implementation: TIMING_PAIRS * 2,
            enabled_product_features: "none (rayon and work-count rejected)",
            webpkit_path: "safe Rust portable scalar (rayon disabled)",
        },
        categories,
        overall,
        gates,
    };

    let json = format!("{}\n", serde_json::to_string_pretty(&report)?);
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("creating temporary surpass report")?;
    temporary
        .write_all(json.as_bytes())
        .context("writing temporary surpass report")?;
    temporary
        .as_file()
        .sync_all()
        .context("syncing temporary surpass report")?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("publishing {}", path.display()))?;

    println!(
        "surpass: {} image(s), {} skipped; anonymous report -> {}",
        rows.len(),
        skipped,
        path.display()
    );
    println!(
        "  gates: lossless-size={} lossy-matched={} encode-speed={} decode-speed={} memory={} measurement-complete={}",
        report.gates.lossless_size,
        report.gates.lossy_size_and_fidelity,
        report.gates.encode_speed,
        report.gates.decode_speed,
        report.gates.same_executable_peak_memory.unwrap_or(false),
        report.gates.measurement_complete,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use anyhow::anyhow;

    use super::{
        Category, WorkerRequest, classify, ensure_scalar_product_features,
        lossy_sample_set_complete, measurement_complete, median, opaque_rgba, paired_medians,
        ratio_permille, sample_set_complete, wrap_vp8,
    };

    #[test]
    fn anonymous_helpers_are_deterministic() {
        assert_eq!(median(vec![9, 1, 5]), Some(5));
        assert_eq!(ratio_permille(1005, 1000), 1005);
        assert_eq!(ratio_permille(10_059, 10_000), 1006);
        assert_eq!(ratio_permille(10_050, 10_000), 1005);
        let rgba = [1, 2, 3, 255].repeat(4);
        assert!(matches!(classify(&rgba), Category::Graphic));
        let mut many_colors = Vec::with_capacity(8193 * 4);
        for value in 0..8193u32 {
            let bytes = value.to_le_bytes();
            many_colors.extend_from_slice(&[bytes[0], bytes[1], bytes[2], 255]);
        }
        assert!(matches!(classify(&many_colors), Category::Photo));
        many_colors[3] = 0;
        assert!(matches!(classify(&many_colors), Category::Transparent));
        assert!(matches!(
            classify(&[1, 2, 3, 0].repeat(4)),
            Category::FlatIcon
        ));
        let wrapped = wrap_vp8(&[1, 2, 3]);
        assert_eq!(&wrapped[..4], b"RIFF");
        assert_eq!(&wrapped[12..16], b"VP8 ");
        assert_eq!(wrapped.len() & 1, 0);
    }

    #[test]
    fn complete_requires_every_measurement_gate_and_no_skips() {
        assert!(measurement_complete(0, [true; 5]));
        assert!(!measurement_complete(1, [true; 5]));
        for failed in 0..5 {
            let mut gates = [true; 5];
            gates[failed] = false;
            assert!(!measurement_complete(0, gates));
        }
    }

    #[test]
    fn resource_coverage_requires_every_image_and_lossy_match() {
        assert!(sample_set_complete(39, 39));
        assert!(!sample_set_complete(39, 38));
        assert!(lossy_sample_set_complete(39, 39, 0, 39));
        assert!(!lossy_sample_set_complete(39, 38, 1, 38));
        assert!(!lossy_sample_set_complete(39, 39, 0, 38));
    }

    #[test]
    fn opaque_rgba_discards_alpha_only() {
        assert_eq!(
            opaque_rgba(&[1, 2, 3, 0, 4, 5, 6, 127]),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn paired_timing_warms_once_and_collects_six_samples_each() {
        let mut ours_calls = 0;
        let mut reference_calls = 0;
        let trace = RefCell::new(Vec::new());
        paired_medians(
            || {
                ours_calls += 1;
                trace.borrow_mut().push('A');
                Ok(ours_calls)
            },
            || {
                reference_calls += 1;
                trace.borrow_mut().push('B');
                Ok(reference_calls)
            },
        )
        .expect("paired timing");
        assert_eq!(ours_calls, 7);
        assert_eq!(reference_calls, 7);
        assert_eq!(
            trace.into_inner().into_iter().collect::<String>(),
            "ABABBAABBAABBA"
        );

        let error = paired_medians(|| Err(anyhow!("warm-up failed")), || Ok(1))
            .expect_err("operation failure must propagate");
        assert_eq!(error.to_string(), "warm-up failed");
    }

    #[test]
    fn worker_request_preserves_a_path_containing_spaces() {
        let request = WorkerRequest::new(
            "webpkit-lossless-encode",
            Path::new(r"C:\temporary files\anonymous input.rgba"),
            17,
            19,
            75,
        );
        let json = serde_json::to_string(&request).unwrap();
        let decoded: WorkerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.input, request.input);
        assert_eq!(decoded.kind, request.kind);
        assert_eq!(decoded.width, 17);
        assert_eq!(decoded.height, 19);
        assert_eq!(decoded.quality, 75);
    }

    #[test]
    fn scalar_gate_observes_dependency_side_feature_unification() {
        assert_eq!(
            ensure_scalar_product_features().is_ok(),
            !webpkit::INTERNAL_RAYON_ENABLED && !webpkit::INTERNAL_WORK_COUNT_ENABLED
        );
    }
}
