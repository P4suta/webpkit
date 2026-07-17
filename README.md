# webpkit

[![CI](https://github.com/P4suta/webpkit/actions/workflows/ci.yml/badge.svg)](https://github.com/P4suta/webpkit/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](#minimum-supported-rust-version)
[![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance/)

A pure-Rust WebP codec — **VP8L** (lossless) and **VP8** (lossy), decode and
encode, behind one API. `#![forbid(unsafe_code)]`, zero required runtime
dependencies, and `no_std`-friendly (with `alloc`).

> Pre-1.0 and under active, test-first development, but functional end to end —
> see [Status](#status).

## Status

### Lossless (VP8L)

- **Decoder — complete, bit-exact.** All VP8L transforms, meta-Huffman, color
  cache and LZ77, plus the extended (`VP8X`) container and its ICC/Exif/XMP
  metadata. Matches libwebp `dwebp` byte-for-byte on the conformance goldens.
- **Encoder — three effort tiers.** `Method::Fast` (literal + subtract-green),
  `Balanced` (the default: LZ77 + color cache + an integer entropy cost model),
  and `Best` (adds the predictor / cross-color / palette transforms and
  meta-Huffman, keeping the smallest real-byte stream). Emits VP8L (bare or
  `VP8X` with metadata) that this decoder and libwebp round-trip.

### Lossy (VP8)

- **Decoder — baseline key frames, bit-exact.** The full VP8 key-frame pipeline
  (boolean decoder, intra prediction, dequant + inverse DCT/WHT, in-loop filter,
  YUV 4:2:0 → RGBA) plus the sibling `ALPH` alpha plane. Reconstructed YUV and
  RGBA match libwebp's `WebPDecodeYUV` / `WebPDecodeRGBA`. Inter (non-key) frames
  do not occur in the still-image format and are out of scope.
- **Encoder — baseline key frames.** Writes a valid `VP8 ` key frame across the
  same Fast / Balanced / Best effort levels (RD whole-block intra-mode search,
  coefficient-probability optimization, trellis quantization, segmentation, and
  4×4 `B_PRED` search at `Best`), with a lossless `ALPH` alpha plane and `VP8X`
  metadata. `dwebp` reads its output, and `decode` of it equals the encoder's own
  reconstruction.

### Shared

- **One API (`webpkit`).** [`decode`] reads any still WebP — routing the
  container's `VP8L` payload to the lossless decoder and `VP8 ` to the lossy one —
  and returns the shared [`Image`]. The type-state [`Encoder`] writes either
  codec: `Encoder::lossless().effort(Best).encode(&img)` or
  `Encoder::lossy().quality(90).encode(&img)` (only the lossy builder has a
  `quality`). [`IncrementalDecoder`] streams push-based input; animation
  (`ANIM`/`ANMF`) is read and written (`decode_frames` / `AnimationEncoder`;
  `decode` returns the first composited frame). One shared `Effort` /
  `MetadataPolicy` / `Error` is defined once in the crate-root shell modules.
- **Beyond the pixel path.** `read_metadata` / `write_metadata` inspect and rewrite
  a file's ICC/Exif/XMP sidecar chunks without touching the image bitstream (the
  `webpmux` half), `decode_yuv` recovers a lossy still's native YUV 4:2:0 planes,
  and `Encoder::lossless().near_lossless(level)` trades a bounded per-channel error
  for a smaller VP8L payload.

Still to come: encoder size/speed tuning and broader real-image benchmarking.

[`Image`]: https://docs.rs/webpkit
[`decode`]: https://docs.rs/webpkit
[`Encoder`]: https://docs.rs/webpkit
[`IncrementalDecoder`]: https://docs.rs/webpkit

## What it is

- **Both WebP codecs.** Lossless (VP8L) and lossy (VP8), decode and encode, with
  animation (`ANIM`/`ANMF`) read and written — through one API
  (`webpkit::decode` / `webpkit::Encoder`) and one set of CLIs.
- **Safe.** `#![forbid(unsafe_code)]` across the core.
- **Dependency-free core.** The `webpkit` crate has no required runtime deps;
  parallelism (`rayon`) and the test oracle (`libwebp-sys`) are opt-in features.
- **Portable.** Builds for `no_std` targets with the `alloc` feature.

### Cargo features

| Feature  | Default | Effect                                                        |
|----------|---------|---------------------------------------------------------------|
| `std`    | yes     | `std` conveniences + `std::error::Error` impl                 |
| `alloc`  | no      | `no_std` + `alloc` build                                      |
| `rayon`  | no      | encoder parallelism                                          |
| `image`  | no      | `TryFrom` interop with the [`image`](https://crates.io/crates/image) crate |
| `oracle` | no      | **dev/test only** — links `libwebp-sys` for differential tests |

## Minimum Supported Rust Version

MSRV is **1.88** (the `image` feature also requires 1.88). It is held until a
feature genuinely needs a newer toolchain; a raise is treated as a minor version
bump. CI verifies it over the published crates' consumer surface.

## Library quick start

```rust
use webpkit::{decode, encode_lossless_rgba, Encoder};

fn roundtrip(width: u32, height: u32, rgba: &[u8]) -> webpkit::Result<()> {
    // One-call encode of raw RGBA8 (4 bytes/pixel) to a lossless (VP8L) file...
    let webp = encode_lossless_rgba(width, height, rgba)?;

    // ...and one-call decode of any still WebP (VP8L or VP8, auto-dispatched).
    let image = decode(&webp)?;
    assert_eq!((image.width(), image.height()), (width, height));

    // Or the type-state builder. Only the lossy builder exposes `quality`, so
    // `Encoder::lossless().quality(90)` is a compile error, not a runtime one.
    let _lossy = Encoder::lossy().quality(90).encode(&image)?;
    Ok(())
}
```

`decode` is **safe on untrusted input by default**: it caps the canvas at
`DEFAULT_MAX_PIXELS` before allocating anything. Choose a different cap with
`decode_with(bytes, &DecodeOptions::default().max_pixels(n))`, or lift it for
trusted input with `.unbounded()`. Full API on
[docs.rs](https://docs.rs/webpkit); a runnable version is
[`examples/roundtrip.rs`](crates/webpkit/examples/roundtrip.rs).

## Building from source

```
mise install        # pinned tools, incl. cwebp/dwebp
just build          # cargo build --workspace --all-targets
just test           # nextest + doctests
just lint           # fmt-check + clippy + cargo-deny + typos + actionlint
```

## Command-line tools

The `webpkit-cli` crate builds three binaries:

- **`cwebp`** / **`dwebp`** — libwebp-style CLIs that speak the same single-dash
  flags. Like libwebp, `cwebp` is **lossy (`VP8 `) by default**; `-lossless` (or
  `-z`) switches to lossless (`VP8L`), where `-q`/`-m`/`-z` select an effort tier
  instead of a quality. `dwebp` decodes **either** codec. Unsupported
  preprocessing knobs are rejected with a clear message rather than silently
  ignored.
- **`webp`** — a brand tool that **auto-detects direction from the file's
  content**: `webp photo.png` writes `photo.webp`, `webp photo.webp` writes
  `photo.png`, and the output name is derived (use `encode`/`decode` to force a
  direction). It reads PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw, turns a **GIF into an
  animated WebP** (loop count and per-frame codec preserved; `--lossy` for VP8
  frames), and its `decode`/`info`/`meta` handle lossless, lossy, and animated
  input alike.

Input for JPEG/GIF/TIFF/BMP comes from the [`image`](https://crates.io/crates/image)
crate (default `formats` feature); PNG keeps its own decoder for metadata
fidelity. The library stays zero-dependency — this is a CLI-only dependency.

```
cargo install --path crates/webpkit-cli --locked   # installs cwebp, dwebp, webp
```

> **PATH note.** `cwebp`/`dwebp` share libwebp's names. If you also have libwebp
> installed, whichever comes first on `PATH` wins — install these deliberately.
> The `webp` command never collides.

```
# cwebp / dwebp, like libwebp:
cwebp in.png -o out.webp -q 80           # lossy by default (-q = quality)
cwebp in.jpg -o out.webp -lossless       # reads JPEG now (was rejected); -lossless keeps it exact
cwebp in.png -o out.webp -lossless -m 6  # -lossless (or -z) -> VP8L; -m/-z/-q = effort
cwebp in.png -o out.webp -near_lossless 60  # near-lossless preprocessing (implies -lossless)
dwebp out.webp -o back.png               # decodes VP8L or VP8; default output is PNG
dwebp out.webp -o planes.yuv -yuv        # native YUV 4:2:0 planes (also -pgm, -bmp, -tiff)
cat in.png | cwebp - -o - | dwebp - -o - > roundtrip.png   # `-` reads stdin

# the webp brand tool — direction is auto-detected, output implicit:
webp photo.png                           # -> photo.webp (lossless, from PNG)
webp photo.jpg                           # -> photo.webp (lossy q75, from JPEG source)
webp photo.webp                          # -> photo.png  (refuses to clobber; --force to overwrite)
webp loop.gif                            # -> loop.webp  (animated; --lossy 80 for VP8 frames)
webp *.jpg -o ./out                      # batch into a directory
webp photo.png -q 80                     # -q is QUALITY now (selects lossy)
webp photo.png --near-lossless 60        # near-lossless preprocessing (implies lossless)
webp info out.webp                       # codec, dimensions, alpha, metadata, animation
webp encode in.png -o out.webp           # force the encode direction
webp encode photo.jpg -o small.webp --target-size 200k -v   # bisect quality to a byte budget
webp photo.png --crop 0,0,512,512 --resize 256x256   # crop then resize before encoding
webp decode anim.webp -o f.png --frames all   # f-000.png, f-001.png, ...
webp meta show tagged.webp               # print ICC/Exif/XMP without decoding a pixel
webp meta set in.webp -o out.webp --icc profile.icc   # rewrite metadata (webpmux-style)
webp meta strip in.webp -o clean.webp    # drop all sidecar metadata
```

Metadata (ICC/Exif/XMP) is **preserved by default** — kinder than cwebp, which
strips it. Use `-metadata none` (or `--metadata none`) to strip. A GIF-derived
animation carries no metadata (the animation encoder does not model it).

**Preprocessing and size targets** (`-crop`/`-resize`/`-size` on `cwebp`,
`--crop`/`--resize`/`--target-size` on `webp`) are done tool-side on the decoded
pixels, exactly where libwebp's `cwebp` does them. Output **dimensions** match
libwebp for the same arguments; **pixels do not** — crop is exact, but resize uses
our own resampler, not libwebp's rescaler. `--target-size` bisects lossy quality
(shown under `-v`), a transparent alternative to libwebp's opaque internal
multi-pass; `-pass`/`--pass` additionally exposes the entropy-refinement passes directly.

> **Breaking (0.2, pre-1.0).** For the `webp` tool: (1) **`-q` is now
> `--quality`**, not `--quiet` — `--quiet` is long-only, and a non-numeric `-q`
> suggests it; (2) the **codec default is source-derived** — JPEG re-encodes lossy
> at q75, everything else stays lossless — printed on every run and overridable
> with `--lossless`/`--lossy`. `cwebp` is unchanged (lossy-always). Neither can
> regress: PNG stays lossless, and JPEG (previously rejected) had no behavior.

### cwebp/dwebp migration

| libwebp command | here | note |
|---|---|---|
| `cwebp in.png -o out.webp` | same | lossy by default; keeps metadata (cwebp strips) |
| `cwebp in.png -o out.webp -q 80` | same | `-q` = quality in lossy mode |
| `cwebp in.jpg -o out.webp` | same | JPEG now accepted (warns: lossy→lossy compounds loss) |
| `cwebp in.png -o out.webp -lossless` | same | switches to VP8L (`-z` too) |
| `cwebp in.png -o out.webp -lossless -m 6` | same | `-m`/`-z`/`-q` = effort in lossless mode |
| `cwebp in.png -o out.webp -metadata none` | same | strip all |
| `cwebp in.png -o out.webp -crop x y w h` | same | supported; output **dimensions** match libwebp, **pixels differ** (own crop) |
| `cwebp in.png -o out.webp -resize w h` | same | supported; dimensions match libwebp, pixels differ (own resampler, not libwebp's); `0` on one axis keeps aspect |
| `cwebp in.png -o out.webp -size 200000` | same | supported; a codec-native bisection over lossy quality to hit the byte budget (`-v` shows the search) |
| `cwebp in.png -o out.webp -psnr 40` | same | supported; a PSNR floor for the quality search |
| `cwebp in.png -o out.webp -pass 6` | same | supported; entropy-refinement passes (`1..=10`; `1` is byte-identical to a single pass) |
| `cwebp in.png -o out.webp -near_lossless 60` | same | near-lossless preprocessing (`0..=100`, lower = stronger; implies lossless) |
| `cwebp in.png -o out.webp -preset photo -sns ...` / `-f` / `-sharpness` / `-segments` / `-sharp_yuv` / `-alpha_q` / `-jpeg_like` / `-partition_limit` / `-exact` | same | supported; psychovisual/RD tuning knobs and content presets, each byte-neutral at its default |
| `cwebp in.png -o out.webp -blend_alpha` / `-hint` / `-af` / `-pre` / `-map` | *(error)* | the true residue webpkit does not model (rejected, not silently ignored) |
| `dwebp in.webp -o out.png` | same | decodes VP8L or VP8; default PNG |
| `dwebp in.webp -o out.ppm -ppm` | same | netpbm output (also `-pam`) |
| `dwebp in.webp -o out.bmp -bmp` / `-tiff` | same | via the `image` crate (`formats` feature; rejected under `--no-default-features`) |
| `dwebp in.webp -o out.yuv -yuv` / `-pgm` | same | native YUV 4:2:0 planes; lossy input only (lossless is a clear error) |
| `webpmux -set icc profile.icc in.webp -o out.webp` | `webp meta set` | rewrite ICC/Exif/XMP without touching pixels |
| `webpmux -strip all in.webp -o out.webp` | `webp meta strip` | drop all sidecar metadata |

The `webp` brand tool has a **`webpmux`-style `meta` subcommand**: `webp meta
show`/`set`/`strip` reads and rewrites a file's ICC/Exif/XMP sidecar chunks without
decoding or re-encoding the image bitstream, so a lossy file's pixels are never
disturbed. In the library this is `read_metadata` / `write_metadata`.

## Verification (external / conformance)

webpkit is verified against the reference implementation (libwebp) rather than
against its own assumptions — both codecs:

- **Golden fixtures** under each codec's `*-conformance` crate (VP8L, VP8, and
  the alpha/animation harness) are generated by libwebp `cwebp` / `dwebp`
  (`just gen-fixtures`) and are never hand-edited.
- **`just conformance`** runs every fixture; each codec's `*-conformance` crate
  drift-gates its committed `conformance-results-*.json` ledger with an in-crate
  `tests/ledger.rs` test (all three symmetric). **`just drift-gate`** runs those
  gates; **`just gen-ledgers`** regenerates the ledgers after an intended change.
- **Property**, **fuzz** (`just fuzz-smoke`), and **differential** (behind the
  `oracle` feature) tests round out coverage for both codecs.
- **Measurement.** Committed integer ledgers (`corpus/metrics.json`
  size/ratio/peak-memory, `corpus/baseline.json`) are drift-gated, while
  criterion timing benches live (local-only) in `webpkit-bench`. See
  [docs/benchmarking.md](docs/benchmarking.md) for the methodology.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full methodology.

## Layout

| Crate                          | Role                                                              |
|--------------------------------|------------------------------------------------------------------|
| `webpkit`                      | The whole codec: VP8L (lossless) + VP8 (lossy) decode/encode, RIFF framing, image model, animation compositor, and (behind features) the work counters. `#![forbid(unsafe_code)]`, zero req. deps |
| `webpkit-cli`                  | The `cwebp` / `dwebp` / `webp` tools (libwebp-compatible; both codecs) |
| `webpkit-samples`              | Deterministic synthetic measurement corpus                       |
| `webpkit-bench`                | Criterion benchmarks                                             |
| `webpkit-alloc-count`          | Counting global allocator for deterministic peak-memory metrics  |
| `webpkit-lossless-conformance` | VP8L golden fixtures + in-crate `conformance-results-lossless.json` gate |
| `webpkit-lossless-fuzz`        | VP8L cargo-fuzz targets                                          |
| `webpkit-lossy-conformance`    | VP8 golden-fixture conformance harness                           |
| `webpkit-lossy-proptest`       | Shared VP8 proptest strategies                                   |
| `webpkit-lossy-fuzz`           | VP8 cargo-fuzz targets                                           |
| `webpkit-conformance`          | `ALPH` (transparent-lossy) decode conformance harness            |
| `webpkit-fuzz`                 | Umbrella decode-path cargo-fuzz targets                          |
| `xtask`                        | Build automation: `gen-fixtures` / `conformance` / `drift-gate`  |

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.
