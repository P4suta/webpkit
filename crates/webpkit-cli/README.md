# webpkit-cli

The `cwebp` / `dwebp` / `webp` command-line tools — libwebp-compatible WebP
encoding and decoding, built on the pure-Rust [`webpkit`](https://docs.rs/webpkit)
codec (both **VP8L** lossless and **VP8** lossy, `#![forbid(unsafe_code)]`).
`cwebp` encodes images to WebP (lossy by default, `-lossless` for VP8L), `dwebp`
decodes WebP back to PNG (or PPM/PAM/BMP/TIFF/YUV/PGM), and `webp` is the unified
brand tool that auto-detects direction.

This crate is part of the [webpkit](https://github.com/P4suta/webpkit) workspace.
If you want the codec itself — `decode`, `encode`, and the `Encoder` API — rather
than the command-line tools, depend on the [`webpkit`](https://docs.rs/webpkit)
umbrella crate, which is the recommended entry point for library use.

## Install

```
cargo install webpkit-cli --locked        # installs cwebp, dwebp, webp
```

> **PATH note.** `cwebp`/`dwebp` share libwebp's names. If you also have libwebp
> installed, whichever comes first on `PATH` wins — install these deliberately.
> The `webp` command never collides.

Input for JPEG/GIF/TIFF/BMP comes from the [`image`](https://crates.io/crates/image)
crate (default `formats` feature); PNG keeps its own decoder for metadata
fidelity. Build with `--no-default-features` to drop the `image` dependency —
`cwebp`/`webp` then read PNG (and the netpbm/raw inputs), and `dwebp`'s `-bmp` /
`-tiff` outputs are rejected rather than shipped.

## The tools

- **`cwebp`** / **`dwebp`** — libwebp-style CLIs that speak the same single-dash
  flags. Like libwebp, `cwebp` is **lossy (`VP8 `) by default**; `-lossless` (or
  `-z`) switches to lossless (`VP8L`), where `-q`/`-m`/`-z` select an effort tier
  instead of a quality. `dwebp` decodes **either** codec and writes PNG, PPM, PAM,
  BMP, TIFF, planar YUV (`-yuv`) or PGM (`-pgm`).
- **`webp`** — a brand tool that **auto-detects direction from the file's
  content**: `webp photo.png` writes `photo.webp`, `webp photo.webp` writes
  `photo.png`, and the output name is derived (use `encode`/`decode` to force a
  direction). It reads PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw, turns a **GIF into an
  animated WebP** (loop count and per-frame codec preserved), and its
  `decode`/`info`/`meta` handle lossless, lossy, and animated input alike.

```
# cwebp / dwebp, like libwebp:
cwebp in.png -o out.webp -q 80              # lossy by default (-q = quality)
cwebp in.jpg -o out.webp -lossless          # reads JPEG now (was rejected); -lossless keeps it exact
cwebp in.png -o out.webp -lossless -m 6     # -lossless (or -z) -> VP8L; -m/-z/-q = effort
cwebp in.png -o out.webp -near_lossless 60  # near-lossless preprocessing (implies -lossless)
dwebp out.webp -o back.png                  # decodes VP8L or VP8; default output is PNG
dwebp out.webp -o planes.yuv -yuv           # native YUV 4:2:0 planes (lossy input)
dwebp out.webp -o out.bmp -bmp              # also -tiff, -ppm, -pam, -pgm
cat in.png | cwebp - -o - | dwebp - -o - > roundtrip.png   # `-` reads stdin

# the webp brand tool — direction is auto-detected, output implicit:
webp photo.png                              # -> photo.webp (lossless, from PNG)
webp photo.jpg                              # -> photo.webp (lossy q75, from JPEG source)
webp photo.webp                             # -> photo.png  (refuses to clobber; --force to overwrite)
webp loop.gif                               # -> loop.webp  (animated; --lossy 80 for VP8 frames)
webp *.jpg -o ./out                         # batch into a directory
webp photo.png -q 80                        # -q is QUALITY now (selects lossy)
webp photo.png --near-lossless 60           # near-lossless (implies lossless)
webp info out.webp                          # codec, dimensions, alpha, metadata, animation
webp encode in.png -o out.webp              # force the encode direction
webp encode photo.jpg -o small.webp --target-size 200k -v   # bisect quality to a byte budget
webp photo.png --crop 0,0,512,512 --resize 256x256   # crop then resize before encoding
webp decode anim.webp -o f.png --frames all # f-000.png, f-001.png, ...

# metadata, without decoding a pixel (the webpmux half):
webp meta show tagged.webp                  # print the ICC/Exif/XMP a file carries
webp meta set in.webp -o out.webp --icc profile.icc --exif exif.bin   # replace kinds from files
webp meta strip in.webp -o clean.webp       # drop all sidecar metadata
```

Metadata (ICC/Exif/XMP) is **preserved by default** — kinder than cwebp, which
strips it. Use `-metadata none` (or `--metadata none`) to strip on encode, or
`webp meta strip` on an existing file. A GIF-derived animation carries no metadata
(the animation encoder does not model it).

**Preprocessing and size targets** (`-crop`/`-resize`/`-size` on `cwebp`,
`--crop`/`--resize`/`--target-size` on `webp`) are done tool-side on the decoded
pixels, exactly where libwebp's `cwebp` does them. Output **dimensions** match
libwebp for the same arguments; **pixels do not** — crop is exact, but resize uses
our own resampler, not libwebp's rescaler. `--target-size` bisects lossy quality
(shown under `-v`) rather than libwebp's opaque internal multi-pass.

> **Breaking (0.2, pre-1.0).** For the `webp` tool: (1) **`-q` is now
> `--quality`**, not `--quiet` — `--quiet` is long-only, and a non-numeric `-q`
> suggests it; (2) the **codec default is source-derived** — JPEG re-encodes lossy
> at q75, everything else stays lossless — printed on every run and overridable
> with `--lossless`/`--lossy`. `cwebp` is unchanged (lossy-always). Neither can
> regress: PNG stays lossless, and JPEG (previously rejected) had no behavior.

## cwebp/dwebp migration

| libwebp command | here | note |
|---|---|---|
| `cwebp in.png -o out.webp` | same | lossy by default; keeps metadata (cwebp strips) |
| `cwebp in.png -o out.webp -q 80` | same | `-q` = quality in lossy mode |
| `cwebp in.jpg -o out.webp` | same | JPEG now accepted (warns: lossy→lossy compounds loss) |
| `cwebp in.png -o out.webp -lossless` | same | switches to VP8L (`-z` too) |
| `cwebp in.png -o out.webp -lossless -m 6` | same | `-m`/`-z`/`-q` = effort in lossless mode |
| `cwebp in.png -o out.webp -near_lossless 60` | same | near-lossless preprocessing (implies lossless) |
| `cwebp in.png -o out.webp -metadata none` | same | strip all |
| `cwebp in.png -o out.webp -crop x y w h` | same | supported; output **dimensions** match libwebp, **pixels differ** (own crop) |
| `cwebp in.png -o out.webp -resize w h` | same | supported; dimensions match libwebp, pixels differ (own resampler, not libwebp's); `0` on one axis keeps aspect |
| `cwebp in.png -o out.webp -size 200000` | same | supported; CLI-side bisection over lossy quality to hit the byte budget (`-v` shows the search) |
| `cwebp in.png -o out.webp -psnr 40` | same | supported; a PSNR floor for the quality search |
| `cwebp in.png -o out.webp -sns ...` / `-f` / `-sharpness` / `-segments` / `-pass` / `-jpeg_like` | *(error)* | internal encoder-tuning knobs webpkit does not expose (rejected, not ignored) |
| `dwebp in.webp -o out.png` | same | decodes VP8L or VP8; default PNG |
| `dwebp in.webp -o out.ppm -ppm` | same | netpbm output (also `-pam`) |
| `dwebp in.webp -o out.bmp -bmp` / `-tiff` | same | via the `image` crate (`formats` feature; rejected under `--no-default-features`) |
| `dwebp in.webp -o out.yuv -yuv` / `-pgm` | same | native YUV 4:2:0 planes; lossy input only (lossless is a clear error) |
| `webpmux -set icc profile.icc in.webp -o out.webp` | `webp meta set` | rewrite ICC/Exif/XMP without touching pixels |
| `webpmux -strip all in.webp -o out.webp` | `webp meta strip` | drop all sidecar metadata |

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE)
at your option.
