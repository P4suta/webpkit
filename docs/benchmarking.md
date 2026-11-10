# Benchmarking & measurement

webpkit separates measurement into **two planes** with very different reliability
guarantees, and keeps them from contaminating each other. Read this before
adding a perf number, a gate, or a benchmark.

## The two planes

| Plane | What it measures | Reproducible? | Committed & gated? | Where |
|-------|------------------|---------------|--------------------|-------|
| **Deterministic ledgers** | Encoded size, compression ratio, peak memory, decode/re-encode stability; **lossy** size / ratio / reconstruction SSE / peak memory; **vs-libwebp** our-vs-cwebp size / centi-dB PSNR (requires pinned `cwebp`, fails closed) | Yes — integer-only, byte-identical across runs | Yes — `committed == fresh` drift gate | `corpus/metrics.json`, `corpus/metrics-lossy.json`, `corpus/vs-libwebp.json`, `corpus/baseline.json` |
| **Deterministic work-cost** | Algorithmic work in both encoders (histogram passes, match-finder hops, cluster scans, fdct/quantize/trellis/SSE calls, …) and both decoders (VP8L bit refills, Huffman/LZ/inverse-transform work; VP8 bool reads, coeff-token walks, IDCT / loop-filter / upsample calls) — a proxy for time | Yes — integer event tallies, toolchain- AND profile-independent | Yes — `committed == fresh` drift gate | `corpus/work.json` |
| **Timing** | Wall-clock throughput (lossless AND lossy encode/decode) | No — noisy, hardware- and load-dependent | **Never** — local/dev only | `webpkit-bench` (criterion) |

The **deterministic ledgers** are the load-bearing regression signal. Every
field is an integer (no float), so `serde_json` renders identical bytes on every
platform and the committed file is a pure textual diff of a fresh run — exactly
like `conformance-results-*.json`. They are reproduced on a pinned toolchain
(**1.96**, independent of the MSRV) (see the memory note below).

- `corpus/metrics.json` — per-`(sample, method)` encoded size + integer
  compression ratio + peak encode/decode memory over the synthetic corpus.
- `corpus/baseline.json` — the corpus sweep: decode → hash → re-encode →
  self-round-trip over every committed image, an encoder/decoder byte-stability
  oracle.

**Timing is deliberately excluded from CI.** Wall-clock time is noisy and
hardware-dependent; a timing gate would either flap or be set so loose it
catches nothing. Byte/size/memory regressions are caught by the ledgers instead.
Criterion benchmarks are a **local developer tool** for comparing two runs on
one machine, never a merge gate.

**Speed is gated deterministically, not by the clock.** To keep a *speed*
regression signal in CI without the noise, both encoders and decoders are
instrumented with integer **work counters** (the `work_count` module, behind the
`work-count` feature) — one per hot path (histogram passes, LZ77 match-finder
hops, meta-Huffman cluster scans, `fdct` / `quantize` / `trellis` / `sse_block`
calls, VP8L Huffman/LZ/inverse-transform operations, VP8 bool reads, …). The
counts are a pure function of the input, so they are byte-reproducible across
platforms, toolchains, and optimization levels, and `corpus/work.json` is
drift-gated exactly like `corpus/metrics.json`. They are a *proxy* for time: an
optimization that removes redundant work drops a counter, and the drop is a
committed, reviewable diff. The counters live behind each codec's `work-count`
feature and are **absent from every default/production build** (the `work!`
macro expands to nothing), so they never cost the shipped codec.

This makes the two byte-golden ledgers complementary oracles for the
optimization loop: `work.json` proves the work went *down*, while
`metrics.json`'s `encoded_hash` and `corpus/baseline.json` prove the output
bytes did *not* change (a pure speed refactor).

## The measurement corpus: `webpkit-samples`

Timing and ratio measurements need images large and varied enough to be stable.
The committed conformance/fuzz corpus maxes out at **64×64** — deliberately tiny
(fast fixtures, small diffs), but too small for stable throughput or meaningful
compression ratios. `crates/webpkit-samples` fills that gap.

It renders a fixed **`Content` × `SIZES`** matrix:

- **Content archetypes** spanning the VP8L difficulty space: `photo` (smoothed
  noise, predictor-friendly), `gradient` (two-axis ramp), `palette` (16-color
  indexed), `noise` (near-incompressible), `tiled` (LZ77 back-reference heavy),
  `solid` (trivial).
- **`SIZES` = `[64, 256, 512]`** (square edges).

Generation is **integer-only and deterministic**: a `SplitMix64` PRNG (wrapping
`u64` arithmetic only, no float) seeds each archetype as a pure function of its
content, edge, and animation frame, so the same bytes are produced on every
platform. The crate is `no_std` (with `alloc`) and is the **single source of
truth** shared by the metrics ledger (`xtask`) and the criterion benches
(`webpkit-bench`), so a recorded size describes exactly the bytes a bench times.

## Running each layer

### Size + memory ledger — `just metrics`

```
just metrics          # gate: fail if committed corpus/metrics.json drifts
just metrics-bless     # (re)author the ledger after an intended change
```

Both recipes pin the toolchain with `cargo +1.96` and build `--release`:

- **`--release`** — the encode-heavy `Best` search is ~13× faster than debug
  (~42s vs ~570s), keeping the run inside its budget. Encoded sizes are
  deterministic and profile-independent, so the release gate matches any run.
- **`+1.96` (pinned toolchain)** — matches CI *and* pins the peak-memory fields, which
  are toolchain-sensitive (allocation patterns can shift between compiler
  versions). The committed `encode_peak_bytes` / `decode_peak_bytes` are
  reproducible only on this exact compiler.
- **All methods at every size** — `Fast`/`Balanced`/`Best` run at 64/256/512,
  including the full Tier-3 `Best` search at edge 512. Its peak memory stays in
  budget because the candidate streams are folded byte-invariantly (only the
  running-best is retained, not every family's stream), which holds the Best peak
  ~40–55% lower on incompressible content. Hence 54 rows: 6 archetypes × 3 methods
  × 3 sizes.

### Lossy size + quality ledger — `just metrics-lossy`

```
just metrics-lossy          # gate: fail if committed corpus/metrics-lossy.json drifts
just metrics-lossy-bless     # (re)author it after an intended size/quality change
```

The lossy analog of `just metrics`, gating `corpus/metrics-lossy.json` — one row
per `(sample, method, quality)` over the sample matrix x `Fast`/`Balanced`/`Best`
(Best capped to `edge <= 256`) x qualities `50/75/90`. Each row is all-integer:
`encoded_len`, `ratio_permille`, `encoded_hash` (the encoder byte-stability
oracle), **`sse`** (integer sum-of-squared-error of *our* decode vs the source —
the deterministic reconstruction-quality field; the human-facing dB is derivable
and stays in the print-only `metrics --lossy --vs-libwebp` aid, never committed),
and `encode_peak_bytes` / `decode_peak_bytes`. Same `--release` + `+1.96`
pin as `metrics` (the peak-memory fields are toolchain-sensitive). Field-level
diff via `cargo run -p xtask -- metrics --lossy --explain`; it ends with a
byte-invariance verdict (did the encoded bytes / `sse` move, or only peak
memory?) — the lossy loop's "did ONLY my intended field change" check.

### cwebp comparison ledger — `just vs-libwebp`

```
just vs-libwebp          # gate: fail if committed corpus/vs-libwebp.json drifts
just vs-libwebp-bless     # (re)author it after an intended size/fidelity change
```

The committed, drift-gated counterpart of the print-only `metrics --vs-libwebp` aid: it gates `corpus/vs-libwebp.json`, one row per synthetic sample for our `l9` lossless (`Best`, capped to `edge <= 256`) versus single-threaded `cwebp -lossless -exact -m 6 -q 100` (`-mt` omitted), and per `(sample, quality)` for our zero-knob `AUTO` lossy versus `cwebp -q Q -noalpha` at qualities `50 / 75 / 90` (66 rows).
Every field is an integer: `our_bytes`, `cwebp_bytes`, `size_permille` (`our_bytes * 1000 / cwebp_bytes`; below 1000 = our stream is smaller), and `our_psnr_centidb` / `cwebp_psnr_centidb` — reconstruction PSNR as fixed-point centi-dB (`round(dB * 100)`; `9900` = identical, i.e. every lossless row).
**Both** outputs are decoded by *our own* decoder before the PSNR, so the fidelity column is apples-to-apples (lossless compares full-WebP sizes, lossy compares bare `VP8 ` payloads).
The pinned reference (`1.6.0`) is recorded in the top-level `libwebp_version`.

Unlike the other ledgers this one **needs libwebp** (`cwebp`).
The gate/bless **fails closed** when `cwebp` is absent or does not report exactly `1.6.0`; a reference-free or version-mismatched run cannot validate or rewrite the ledger.
Its rows are integer sizes and centi-dB PSNR (no peak memory), which are deterministic and profile-independent, so the recipe builds `--release` for runtime budget only and needs **no `+1.96` pin** (cwebp's own output is pinned by the `1.6.0` version guard).
If a non-libwebp `cwebp` shadows libwebp on `PATH`, point `WEBPKIT_CWEBP` at the libwebp `1.6.0` binary.

### Private matched-fidelity campaign — `surpass`

```
cargo run --release -p xtask --features scoreboard-libwebp -- \
  surpass --real <dir> --max-edge 1024
```

`surpass` is the local, auditable comparison used for the libwebp-surpass gate.
It requires both the linked and command-line libwebp references to report exactly `1.6.0`; missing or mismatched references fail the command.
`--max-edge` reads source dimensions without decoding pixels, then asks cwebp to cap the longer image dimension while preserving aspect ratio.

- **Lossless basis:** deepest webpkit effort versus single-threaded `cwebp -lossless -exact -m 6 -q 100` (`-mt` omitted), including the WebP container on both sides.
- **Lossy basis:** for each libwebp quality 50, 75, and 90, build webpkit's full quality 0–100 RD curve.
  Select the smallest payload whose RGB-PSNR and SSIM are both at least the libwebp reconstruction.
  Quality-number remapping alone cannot count as an improvement.
- **Coverage:** size and matched-fidelity RD are evaluated on every recognized image entry using a fixed four-worker corpus wave (each codec invocation remains scalar).
  Before that expensive wave starts, all pinned cwebp lossy references are generated and validated so a reference-process failure is reported without discarding completed 101-point curves.
  An unreadable recognized image fails measurement completion; unrelated files and subdirectories are excluded.
  After that wave, timing and process-peak memory are measured serially for every recognized image without rebuilding its 101-point RD curve.
  The schema-4 report records the worker count, all-image resource policy, and sample count.
- **Timing basis:** portable safe scalar webpkit (Rayon disabled), alternating paired A/B runs on each image: one deterministic warm-up per implementation,
  then three AB/BA pairs (six timed samples per implementation).
  The upper median is taken per image, then across each category and the full corpus.
  Lossy encode timing supplies the same opaque RGBA8 buffer to both codecs (alpha forced to 255 and excluded from the comparison).
- **Memory basis:** every webpkit/libwebp encode and decode runs in a fresh invocation of the identical internal xtask worker executable and observer wrapper.
  The parent records OS process peak working set on Windows or maximum RSS on Linux, so the process baseline is the same while allocations from one codec cannot leak into the other codec's sample.
  Lossy encoders receive the same opaque RGBA8 input layout, and both lossy decoders receive the same WebP container rather than comparing a raw `VP8 ` payload with a container.
- **Environment:** OS, architecture, CPU model, logical CPU count, Rust toolchain, source commit and dirty state, corpus-worker count, timing-pair count, warm-up count, timed-sample count, and resolved scalar feature policy are recorded with the aggregates.
- **Privacy:** the report contains only `flat-icon`, `transparent`, `graphic`,
  `photo`, and overall aggregates.
  It records no source path, file name, or per-image result.
  The only output is the gitignored `target/surpass/latest.json`.

All size, time, and peak ratios are represented in integer permille and rounded up, so a value just above a threshold cannot be rounded into a pass.
The report evaluates the measurement thresholds directly: lossless median `<=100.5%` with at least half no larger than libwebp; matched-fidelity lossy median `<=105%` in every band and photo `<=103%`; scalar encode `<=2.0x`,
decode `<=1.5x`; and identical-worker peak memory `<=1.5x`.
`measurement_complete` is true only when every measurement threshold passes,
every recognized image completed size/RD evaluation, and every present category and every matched lossy band completed resources for every recognized image.
It is not the WK-400 completion bit: conformance, golden, oracle, fuzz, mutation,
and ledger gates remain separate requirements.

### Work-cost ledger — `just work`

```
just work             # gate: fail if committed corpus/work.json drifts
just work-bless        # (re)author the ledger after an intended algorithmic change
```

Both recipes build `--release` and pass `--features work-count` (which links the
counters into both codecs and the xtask `work` command):

- **`--release`, but NO `+1.96` pin** — unlike `metrics`, the counts are integer
  event tallies that do not depend on the toolchain or optimization level, so any
  compiler reproduces them; `--release` is only for the runtime budget. This is
  the key simplification over the peak-memory ledger.
- **Both codecs, encode AND decode** (schema `version: 3`) — every
  `(sample, method)` is measured for both the `lossless` and `lossy` encoders,
  plus one `lossless-decode` and one `lossy-decode` row per sample. The VP8L row
  isolates bit-reader, Huffman/LZ, inverse-transform, and output-pack work; the
  VP8 row isolates `bool_read`, `bool_renorm`, `coeff_token`, `idct_call`,
  `loop_filter_edge`, `upsample_row`, plus shared `predict_*` work. Counters name
  the *operation*, not the phase; which pass populates them is fixed by the
  reset→run→snapshot discipline, so decode-only counters stay zero in encoder
  rows and encode-only counters stay zero in decoder rows (the shared VP8 kernel
  counters `idct_call`/`loop_filter_edge` fire in both, since that encoder
  reconstructs and deblocks its own output). The VP8L decode row reuses the
  sample's already-measured `AUTO` output; the VP8 row is produced from a fixed
  mid-quality `Balanced` encode.
- **`Best` capped to `edge <= 256`** — the counter increments slow the `Best`
  search several-fold, and its `Best@512` cases dominate the run; 64/256 fully
  capture `Best`'s algorithmic signature (the counts scale predictably with
  size). `Fast`/`Balanced` run at every size. Plus 18 rows for each decoder.
- **Serial by construction** — the counters are process-global statics, so the
  `work` loop measures one operation at a time (reset → run → snapshot). Totals
  are `fetch_add`-order-independent, so an in-encode `rayon` region does not
  perturb them, but the ledger is generated `rayon`-off regardless.

### Timing — `just bench` + the serial/parallel workflow

```
just bench             # cargo bench -p webpkit-bench (criterion)
```

Benchmark groups (all sweeping the same `webpkit-samples` matrix):

- `encode` — lossless `Fast` / `Balanced` / `Best` (Best capped to `edge <= 256`),
  throughput in **input MB/s** (`Throughput::Bytes` = raw RGBA byte count).
- `decode/oneshot` and `decode/streaming` — lossless throughput in **Mpixels/s**
  (`Throughput::Elements`).
- `lossy_encode` — lossy `Fast` / `Balanced` / `Best` (Best capped to `edge <= 256`)
  at a fixed mid quality, **input MB/s**.
- `lossy_decode/oneshot` and `lossy_decode/streaming` — lossy VP8 decode
  throughput in **Mpixels/s** (oneshot decodes the raw `VP8 ` payload, streaming
  feeds the `IncrementalDecoder` the WebP container in 4 KiB chunks).
- `animation` — multi-frame encode/decode.

Because timing is noisy, always compare two runs with criterion's baseline
workflow rather than reading absolute numbers:

```
cargo bench -p webpkit-bench -- --save-baseline main   # record a baseline
# ... make a change ...
cargo bench -p webpkit-bench -- --baseline main         # compare against it
```

To measure the **serial-vs-parallel** `Best` evaluator, do the same across the
`rayon` feature — record a serial baseline, then re-run with the feature:

```
cargo bench -p webpkit-bench -- --save-baseline serial              # serial
cargo bench -p webpkit-bench --features rayon -- --baseline serial    # parallel vs serial
```

### Kernel microbenchmarks — `just bench-kernels` (back-to-back A/B)

The `--save-baseline` / `--baseline` workflow above has a failure mode when
attributing a **single-digit-% kernel change**: the baseline is recorded once (on
a cold machine) and every later `--baseline` run is compared against it, so warm /
thermally-throttled reruns read as a systematic slowdown and the same change can
flip sign between runs. That noise floor is larger than the delta a pure
codegen/scalar tweak moves, so cross-run baselines cannot resolve it here.

`just bench-kernels` (the `kernels` bench, behind `--features bench`) sidesteps
the bias structurally. Each numeric kernel is exposed to the bench via its codec's
dev-only `crate::bench` shim **together with its pre-optimization `*_reference`
twin** (the same verbatim copy the equivalence proptest pins byte-for-byte), and
both are timed **in the same run**:

```
just bench-kernels                 # all kernels
just bench-kernels sse_block       # one kernel's group
```

Because the optimized kernel and its reference are measured back-to-back — one
invocation, one thermal window, one profile — the opt-vs-ref delta is read
directly from their two point estimates instead of a persisted cold baseline:

```
jq .mean.point_estimate target/criterion/sse_block/opt/16/new/estimates.json   # ns/iter, optimized
jq .mean.point_estimate target/criterion/sse_block/ref/16/new/estimates.json   # ns/iter, reference
```

Non-overlapping `[lo hi]` CIs between `opt` and `ref` mean the gap is real. This
is still the **timing plane** — local-only, never committed, never a CI gate; the
`bench` feature adds no production code (the shims are `#[cfg(feature = "bench")]`)
and is compile-guarded in CI (`bench-build`, `--no-run`) only so it cannot
bit-rot. Pair every kernel change with a `--emit asm` check
(`cargo rustc --release -p webpkit --lib -- --emit asm`): the microbench
says *whether* a change is faster, the asm says *why* (e.g. bounds-check elision
vs. packed SIMD — the baseline SSE2 target emits no packed reduction for the
integer squared-difference sum, so `sse_block`'s ~2× win is scalar, not vector).

### Real-image timing — `just bench-real <dir>`

```
just bench-real <dir> [max_edge=512] [iters=5] [limit=0]   # cargo xtask bench-real
```

The criterion plane applied to **real images** instead of the synthetic matrix:
per file it prints `Method::Balanced`/`Best` encode throughput (raw RGBA MB/s) and
decode throughput (Mpixels/s), best-of-`iters`. Like `metrics --real`, `dir` is a
pure runtime path — it writes **no** repo file, bakes in no image path, and
**soft-skips** when `cwebp` (the source-image reader) is absent. Built with the
fast `quick` profile (optimized, no fat LTO) so the inner loop rebuilds in
seconds; `--limit N` benchmarks only the first N images for a fast smoke (the
heavy `Best` timing makes this the quick signal). Not gated — wall-clock only.
To time the parallel `Best` the CLI ships, add `--features rayon`:

```
cargo run --profile quick --features rayon -p xtask -- bench-real <dir> --limit 4
```

### libwebp size comparison — `metrics --vs-libwebp`

```
mise exec -- cargo run --release -p xtask -- metrics --vs-libwebp
```

Prints a table comparing our `Method::Best` encoded size against libwebp
`cwebp -m 6 -q 100` over the corpus (`Best` capped to `edge <= 256`, like the
ledger). This is a **printed-only developer aid**: it runs *after* the ordinary
gate/bless, writes **no** file, and is **never gated** — it needs libwebp
(`cwebp`, provided via `mise`), which the deterministic ledger deliberately does
not. When `cwebp` is absent or the wrong version it **soft-skips** (prints a
notice, exits 0) rather than failing. Floats in this table are fine because it is
never committed.

### Real-image size comparison — `metrics --real <dir>`

```
cargo run --release -p xtask -- metrics --real <dir> [--max-edge N]
```

Runs the same `Method::Best` vs `cwebp -m 6` size comparison, but over the real
images in a **caller-supplied directory** instead of the synthetic corpus. The
directory is a pure runtime argument (nothing about it is baked into the tool),
and each file is first re-encoded by cwebp with the width capped to `--max-edge`
(default `512`, aspect preserved; `0` disables the resize) so both encoders see
identical pixels. Like `--vs-libwebp` it is **print-only** — every temporary file
lives in a `tempfile::tempdir()`, it writes **no** file into the repo (the gated
`corpus/metrics.json` is untouched), and it **never gates**: it soft-skips when
libwebp is absent, and a file cwebp cannot read is reported as a `skipped` row
rather than aborting the run. The ratio column is integer permille
(`ours * 1000 / cwebp`; below 1000 means our stream is smaller).

### Real-image lossy AUTO comparison — `metrics --lossy --real <dir>`

```
cargo run --release -p xtask -- metrics --lossy --real <dir> [--max-edge N]
```

Composes `--lossy` with `--real`: instead of the lossless `Best`-vs-`cwebp -m 6`
size table, it compares our **zero-knob `AUTO` encoder** against `cwebp -q Q`
**default shaping** at each of qualities `50 / 75 / 90` — the exact default a user
ships, so it is the measurement basis for the AUTO default-shaping tune. For each
quality it prints size + PSNR + SSIM rolled up by **content category** — `photo` /
`graphic` / `transparent` — where the category is **derived from the pixels**
(alpha presence, then distinct-color count), never from the filename. Both
payloads are decoded by *our* decoder so PSNR/SSIM are apples-to-apples, and
`cwebp -noalpha` matches our alpha-dropping lossy encoder (the comparison is on
RGB). Columns: `size%` = `ours * 100 / cwebp` (`< 100` means we are smaller);
`+dB` / `+SSIM` = ours − cwebp (positive means we are better).

Like the other `--real` paths it is **print-only** (temp files in a dropped
`tempfile::tempdir()`, no repo file written, never gated) and **soft-skips** when
libwebp is absent. **Privacy:** the directory is a pure runtime argument and
results are aggregated by pixel-derived category, so **no filename is ever printed
or recorded** — keep a private corpus out of git and report only the category
rollup. If a non-libwebp `cwebp` (e.g. this repo's own CLI) shadows libwebp on
`PATH`, point `WEBPKIT_CWEBP` / `WEBPKIT_DWEBP` at the libwebp `1.6.0` binaries.

### Real-image multi-axis scoreboard — `scoreboard --real <dir>`

```
cargo run --profile quick --features scoreboard-libwebp -p xtask -- scoreboard --real <dir> [--max-edge N]
```

One print-only scoreboard that puts **four axes** side by side over the real corpus — size, fidelity, encode/decode speed, and webpkit peak memory — rolled up by pixel-derived content category (`flat-icon` / `transparent` / `graphic` / `photo`, plus an `overall` row), with a **lossless** section and a **lossy** (`q75`) section.
Both sides run their **shipped defaults**: webpkit `AUTO`, libwebp's simple-API defaults (`WebPEncodeLosslessRGBA`, `WebPEncodeRGB`).
This deliberately **builds the scoreboard, not a verdict** — no win-condition is preset; the numbers describe what a user gets out of the box on identical resized pixels.

**The columns are not equally rigorous, and the tool says so.** Read them by tier:

- **Size — rigorous.** Exact encoded byte counts (`ours B`, `lib B`) and the integer-permille `size%` = `ours * 100 / lib` (`< 100` = ours smaller).
  libwebp is the **in-process libwebp-sys reference**, which vendors the **same libwebp `1.6.0`** as the pinned `cwebp` binary (the tool prints the runtime version it linked), so the bytes are a true apples-to-apples default-vs-default comparison.
- **Fidelity (lossy) — rigorous.** `+dB` / `+SSIM` = ours − libwebp reconstruction PSNR / MSSIM (`> 0` = ours better).
  **Both** encoders' `VP8 ` payloads are decoded by *webpkit's own* decoder, so the YUV→RGB path is identical on both sides (`cwebp -noalpha`-equivalent RGB basis; lossless is exact, so it has no fidelity columns).
- **webpkit peak memory — rigorous (exact).** `encKiB` / `decKiB` are the peak *additional* requested bytes during a webpkit encode / decode, measured by the `webpkit-alloc-count` counting allocator (the same instrument as the memory ledger), reported as the bucket's worst case.
- **Speed — the timing plane (noisy, approximate, never gated).** `encO/encL MB/s` (raw-input MB/s) and `decO/decL MP/s` (Mpixels/s) are the **median of repeated in-process passes**. libwebp is timed **in-process via libwebp-sys on identical buffers** (no process/startup/IO overhead), the most rigorous method available —
  but it is still wall-clock: hardware- and load-dependent, non-deterministic, and reported for reading, never as a gate.
  Encode times the same effort as the size column; decode times the **same `VP8`/WebP bytes** on both decoders (our payload,
  wrapped in a container for libwebp).
- **libwebp peak memory — omitted, by construction.** libwebp allocates through C `malloc`, which bypasses the Rust counting allocator, so it **cannot** be measured on the same basis.
  Rather than fabricate a number or mix in a coarse OS peak-RSS from a different process model, the scoreboard shows **no** libwebp memory column and states why.

libwebp columns need the **`scoreboard-libwebp`** feature, which links the in-process reference; a plain build never compiles it, and without it the libwebp columns print `n/a` (webpkit size/speed/memory still show).
Like the other `--real` paths it is **print-only** (temp files in a dropped `tempfile::tempdir()`, no repo file written, **never gated**), needs `cwebp` only to **read** source images into RGBA, and **soft-skips** when it is absent.
**Privacy:** `<dir>` is a pure runtime argument and every image is bucketed by **pixels** (alpha presence, then a capped distinct-color count), never by filename, so **no file name is ever printed or recorded** — report only the category rollup.
Content note: the pixel classifier cannot separate a museum scan from an ordinary photograph (both are opaque,
high-distinct-color), so both land in the `photo` bucket.

## Reading results

**Compression (the ledger).** The measured impact of an encoder change is the
**git diff of `corpus/metrics.json`**. Per row:

- `encoded_len` — encoded WebP byte length; `ratio_permille` —
  `encoded_len * 1000 / raw_len`, parts-per-thousand of the raw RGBA (smaller is
  better). Read these per `(content, edge, method)`.
- `encoded_hash` — FNV-1a-64 of the encoded bytes, a byte-stability oracle: any
  change to the emitted stream shows up here even if the size is unchanged.

**Lossy compression + quality (the ledger).** The lossy analog is the **git diff
of `corpus/metrics-lossy.json`**, read per `(content, edge, method, quality)`.
It carries the same `encoded_len` / `ratio_permille` / `encoded_hash` fields
(here over the `VP8 ` payload and raw RGB) plus **`sse`** — the integer
sum-of-squared-error of our decode vs the source (lower is closer to the
original; the rate-distortion trade-off is `encoded_len` vs `sse` at fixed
quality). A pure speed refactor must leave both `encoded_hash` and `sse` fixed;
`metrics --lossy --explain` prints that verdict.

**Memory (the ledger).** `encode_peak_bytes` / `decode_peak_bytes` are the peak
*additional* requested bytes during that method's encode, and during a decode of
its own output — the working-set high-water marks (measured by the process-wide
counting allocator; the input buffer is excluded from the decode window).

**Work (the ledger).** Each `corpus/work.json` row's `counts` map is the
algorithmic work of one `(codec, sample, method)` operation. Encoder rows name
their method; decoder rows use `method = "decode"`. Read it per counter: a large
or fast-growing count is a candidate hot path, and the measured impact of an
optimization is the **git diff of `corpus/work.json`** (a counter that drops is
work removed). Because the counts are a deterministic proxy for time, a drop
should track a wall-clock improvement in `just bench` — cross-check the two.

**Timing (criterion).** After `just bench`, open the HTML report at
`target/criterion/**/report/index.html`. Read the throughput figures
(**MB/s** for encode, **Mpixels/s** for decode) and, when comparing against a
saved baseline, criterion's change estimate and its noise verdict. Treat single
absolute numbers with suspicion; trust baseline-relative deltas.

## Regression workflow

**Encoder / size or memory change:**

1. Make the change.
2. `just metrics` (or `just metrics-lossy` for a lossy-encoder change) — the
   drift gate fails and names the first divergent case.
3. Review the `corpus/metrics.json` / `corpus/metrics-lossy.json` diff: is the
   size/ratio/sse/memory move expected and in the right direction?
4. If intended, `just metrics-bless` / `just metrics-lossy-bless` and commit the
   updated ledger with the change. If not, fix the code until the gate is green.

**Speed / algorithmic change (the work-cost loop):**

1. `just work` — see which counter dominates (the hot path to attack). Lossy
   decode hot paths are the `lossy-decode/*` rows; lossy encode the `lossy/*`.
2. Optimize that path.
3. `just work` — confirm the counter dropped.
4. `just metrics` / `just metrics-lossy` **and** `just corpus-sweep` — confirm
   `encoded_hash` (and lossy `sse`) and the decode/re-encode baseline are
   **unchanged** (the refactor is byte-invariant). For a lossy-decode speedup the
   decoder output must not move: the golden conformance tests
   (`cargo test -p webpkit -p webpkit-lossy-conformance`) and the libwebp
   oracle (`cargo test -p webpkit --features oracle --test oracle_lossy`) are the
   guard. If the change intentionally moves bytes, bless the relevant ledger too.
5. `just work-bless` and commit the reduced ledger with the change.

**Timing change (local only):**

1. `cargo bench -p webpkit-bench -- --save-baseline before` on the base revision.
2. Make the change.
3. `cargo bench -p webpkit-bench -- --baseline before` and read the deltas.
4. For parallelism, use the `serial` / `--features rayon --baseline serial`
   cycle above. Nothing here is committed or gated.
