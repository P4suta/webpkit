# Benchmarking & measurement

webpkit separates measurement into **two planes** with very different reliability
guarantees, and keeps them from contaminating each other. Read this before
adding a perf number, a gate, or a benchmark.

## The two planes

| Plane | What it measures | Reproducible? | Committed & gated? | Where |
|-------|------------------|---------------|--------------------|-------|
| **Deterministic ledgers** | Encoded size, compression ratio, peak memory, decode/re-encode stability; **lossy** size / ratio / reconstruction SSE / peak memory | Yes — integer-only, byte-identical across runs | Yes — `committed == fresh` drift gate | `corpus/metrics.json`, `corpus/metrics-lossy.json`, `corpus/baseline.json` |
| **Deterministic work-cost** | Algorithmic work in the encoders (histogram passes, match-finder hops, cluster scans, fdct/quantize/trellis/SSE calls, …) **and the lossy decoder** (bool reads, coeff-token walks, IDCT / loop-filter / upsample calls) — a proxy for time | Yes — integer event tallies, toolchain- AND profile-independent | Yes — `committed == fresh` drift gate | `corpus/work.json` |
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
regression signal in CI without the noise, the encoders are instrumented with
integer **work counters** (the `work_count` module, behind the `work-count`
feature) — one per hot path
(histogram passes, LZ77 match-finder hops, meta-Huffman cluster scans, `fdct` /
`quantize` / `trellis` / `sse_block` calls, k-means comparisons, …). The counts
are a pure function of the input, so they are byte-reproducible across
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
- **Both codecs, encode AND lossy decode** (schema `version: 2`) — every
  `(sample, method)` is measured for both the `lossless` and `lossy` codecs
  encode, plus **one `lossy-decode` row per sample** that isolates the VP8 decode
  hot paths (`bool_read`, `bool_renorm`, `coeff_token`, `idct_call`,
  `loop_filter_edge`, `upsample_row`, plus the shared `predict_*`). Counters name
  the *operation*, not the phase; which pass populates them is fixed by the
  reset→run→snapshot discipline, so the decode-only counters stay zero in the
  encode rows and the encode-only counters stay zero in the decode rows (the two
  shared-kernel counters `idct_call`/`loop_filter_edge` fire in both, since the
  encoder reconstructs and deblocks its own output). The decode row is produced
  from a fixed mid-quality `Balanced` encode (decode work is method-independent).
- **`Best` capped to `edge <= 256`** — the counter increments slow the `Best`
  search several-fold, and its `Best@512` cases dominate the run; 64/256 fully
  capture `Best`'s algorithmic signature (the counts scale predictably with
  size). `Fast`/`Balanced` run at every size. Plus the 18 `lossy-decode` rows.
- **Serial by construction** — the counters are process-global statics, so the
  `work` loop measures one encode at a time (reset → encode → snapshot). Totals
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
algorithmic work of one `(codec, sample, method)` encode. Read it per counter:
a large or fast-growing count is a candidate hot path, and the measured impact
of an optimization is the **git diff of `corpus/work.json`** (a counter that
drops is work removed). Because the counts are a deterministic proxy for time, a
drop should track a wall-clock improvement in `just bench` — cross-check the two.

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
