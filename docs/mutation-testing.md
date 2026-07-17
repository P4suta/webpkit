# Mutation testing

Line coverage proves a line *ran*; it says nothing about whether a test would
*notice* if that line were wrong. [cargo-mutants](https://mutants.rs/) closes the
gap: it rewrites the source one small change at a time (`a + b` → `a - b`, `<` →
`<=`, `return x` → `return Default::default()`, delete a match arm, …) and reruns
the tests against each **mutant**. A mutant the tests still pass is *missed* — a
concrete assertion the suite is not making. For a codec verified byte-for-byte
against libwebp, a surviving mutant is a precise pointer at a hole.

## Scope

Both published crates are mutated: `webpkit` (the library — its `lossless` and
`lossy` codec zones and the codec-agnostic core shell) and `webpkit-cli` (the
binaries). Tooling and test-harness crates (xtask, `*-fuzz`, `*-proptest`,
`*-conformance`, bench, samples, alloc-count) are never targeted — they exist to
test the product, not the other way round.

Configuration lives in [`.cargo/mutants.toml`](../.cargo/mutants.toml): nextest as
the runner, `--locked` on every inner build, `--features image,rayon` so the optional
interop module and the parallel `Method::Best` / wavefront / row evaluators are all
compiled and mutated, generous timeouts for the slow `Method::Best` paths, and an
`exclude_re` list of provably-equivalent mutants (each with a reason).

### The whole codec must be *reachable* to be mutated

cargo-mutants discovers source files by walking `mod` declarations from each crate
root with `syn` — and it does **not** expand macros. The single-crate consolidation
had moved every internal module (`lossless`, `lossy`, `anim`, `container`, …) behind
a `macro_rules!`-generated mod tree, which is opaque to that walk: for as long as it
stood, a full sweep silently generated **zero** mutants for the entire codec and only
ever mutated the crate-root facade plus the CLI. `lib.rs` now declares those modules
as **literal `pub(crate) mod` items** so the walk descends into every codec file, and
routes the non-default `__internals` re-exposure (which the sweep never builds)
through a macro so the walk sees exactly **one** copy of each file rather than a
duplicate per `cfg` branch. Restoring that visibility took the product-crate mutant
count from **1 259** (facade + CLI only) to **9 498**.

If a future refactor hides product modules behind a macro again, the symptom is the
same: `cargo mutants --list -p webpkit` stops listing anything under `src/lossless/`
or `src/lossy/`. Keep internal modules declared as literal `mod` items.

## Running it

```
just mutants                                          # full sweep of both crates (slow)
just mutants --file crates/webpkit/src/lossless/huffman/canonical.rs   # one file (fast inner loop)
just mutants --shard 0/4                              # one quarter of the work
just mutants-diff main                                # only what changed vs a base ref (the CI gate)
```

The tool is version-pinned (`cargo:cargo-mutants` in `mise.toml`, and the same
version in the CI `mutants` job) so the generated mutants stay reproducible; bump it
explicitly rather than via `latest`.

## The CI gate

The `mutants` job in `.github/workflows/ci.yml` runs `cargo mutants --in-diff` on
pull requests only: it mutates just the lines the PR changes and fails if any
survive. A full sweep is far too slow to gate every push, but the diff-scoped run is
fast and keeps new product code honest. On `push` / `merge_group` the job skips, and
the `ci-required` aggregate counts a skip as a pass. Because the config carries
`--features image,rayon`, a PR that touches the parallel evaluators is mutated there
too — the same paths whose parallel==serial byte-for-byte equivalence the dedicated
`rayon` CI job asserts by name (`serial_and_parallel_evaluation_agree`,
`wavefront_planner_matches_serial_byte_for_byte`,
`rayon_row_parallel_matches_serial_byte_for_byte`). Two independent nets now cover the
rayon product path that used to sit outside both mutation and coverage.

Triage a surviving mutant one of two ways:

1. **Kill it** — add or tighten a test so the mutation is caught. This is the default
   and the point of the exercise.
2. **Exclude it** — if the mutation is genuinely behavior-preserving (an equivalent
   mutant) or untestable, add a regex to `exclude_re` in `.cargo/mutants.toml` with a
   one-line reason. **Anchor it to the function / method / return type, never to a
   `path:line:col` coordinate** — a line anchor dangles or, worse, latches onto a
   different mutant the moment the code around it moves. Prefer killing over excluding.

## Baseline

<!-- Updated after each full sweep. -->

**Status: the authoritative full sweep is a pending pre-publish step — not yet run
against the post-campaign codec.** This is deliberate and recorded honestly rather
than carried over: the prior per-zone baseline (2035 / 3777 / 293 caught, "0 missed")
is void for two independent reasons.

1. **It was never valid on the merged crate.** That baseline was measured on the
   old separate `webpkit-core` / `-lossless` / `-lossy` crates. After they merged,
   the macro-hidden mod tree (above) meant no sweep actually mutated the codec, so the
   "consolidated re-sweep confirms the same totals" claim could not have been true —
   it generated zero codec mutants.
2. **P2–P8 rewrote the mutated code.** The pre-publish campaign rewrote the lossless
   encoder (auto-`Effort`), the entire lossy psychovisual RD path, the geometry /
   rescaler, and the mux / animation layers. Net change in `crates/webpkit/src`:
   **7 050 insertions, 726 deletions across 34 files.** The modules that moved most —
   and therefore whose old `path:line:col` exclusion anchors are now meaningless — are:

   | module | Δ lines | campaign phase |
   | --- | --: | --- |
   | `optimize.rs` (inter-frame optimizer) | +1136 | P8 |
   | `lossy/frame.rs` (SNS / segments / filter shaping) | ~+1018 | P3 |
   | `mux.rs` (editable webpmux-parity mux) | +688 | P8 |
   | `lossy/sharp_yuv.rs` (new) | +582 | P4 |
   | `lossless/vp8l/encode.rs` (transform search) | ~+567 | P2 |
   | `lossy/tuning.rs` (new) / `rate.rs` (new) | +476 / +449 | P3 / P6 |
   | `lossy/encoder.rs`, `lossy/alpha.rs`, `lossy/perceptual.rs` (new) | +455 / +414 / +222 | P3–P6 |
   | `lossless/{histogram,encoder}.rs`, `lossy/trellis.rs`, `effort.rs` | +127 / +121 / +110 / +104 | P2 / P3 |

The ~260-entry equivalence ledger that these phases invalidated is therefore
**re-derived from scratch by the full sweep, not hand-migrated** — translating stale,
unverified equivalence claims onto rewritten code would launder assertions no sweep
has confirmed. The prior ledger and its reasoning remain in git history as the
re-triage seed. `exclude_re` currently holds the **one** equivalent re-derived so far
(see the spot-check below); the rest are added back, each with a durable function-name
anchor, as the full sweep surfaces them.

### Running the authoritative full sweep (the pending pre-publish step)

```
just mutants        # cargo mutants -p webpkit -p webpkit-cli, nextest, --features image,rayon
```

This is a **long local run**: ~9 500 mutants, each re-running the full
`-p webpkit -p webpkit-cli` suite (including the `Method::Best` paths), so at the
observed ~1–2 min/mutant it is a multi-hour sweep even parallelised. Run it sharded
(`just mutants --shard k/N`) or per-zone (`--file crates/webpkit/src/lossless/…`) and
aggregate. Author the fresh `exclude_re` ledger from the aggregated survivors: each
must be either killed with a new deterministic test or excluded with a durable anchor
and a one-line reason. Expected genuine-equivalent classes (from the prior ledger, to
be re-confirmed on current code): disjoint-bit `|`/`^`, unique-key / unreachable-
boundary comparisons, capacity-only hints, non-terminating-loop mutations observable
only as a timeout, and the `#[cfg(feature = "oracle" | "bench")]` helpers the sweep
does not build (the `rayon` ones are now compiled and killed by the differential
tests instead of excluded).

### Scoped spot-check swept here

To ground this document in a real measurement rather than a projected one, a full
scoped sweep was run on one small, heavily-rewritten module — `crates/webpkit/src/`
`effort.rs` (P2 collapsed the three-tier `Effort` enum into a single auto/level
design, +104 lines):

```
cargo mutants -p webpkit -p webpkit-cli --features image,rayon --file crates/webpkit/src/effort.rs
```

**Result: 7 mutants — 5 caught, 1 unviable, 1 missed.** The one missed mutant,
`replace > with >= in Effort::level`, is a **provable equivalent**: in
`if n > MAX_LEVEL { MAX_LEVEL } else { n }` the two operators differ only at
`n == MAX_LEVEL`, where both yield `MAX_LEVEL` (the clamp is idempotent there). It is
now the sole `exclude_re` entry, anchored by function name. Re-running the spot-check
with the entry in place tests 6 mutants with 0 missed.

That is the full extent of what has been swept in this pass. **Every other zone —
all of `lossless` and `lossy` — remains for the authoritative full local run above.**
