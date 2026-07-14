# Mutation testing

Line coverage proves a line *ran*; it says nothing about whether a test would
*notice* if that line were wrong. [cargo-mutants](https://mutants.rs/) closes the
gap: it rewrites the source one small change at a time (`a + b` → `a - b`, `<` →
`<=`, `return x` → `return Default::default()`, delete a match arm, …) and reruns
the tests against each **mutant**. A mutant the tests still pass is *missed* — a
concrete assertion the suite is not making. For a codec verified byte-for-byte
against libwebp, a surviving mutant is a precise pointer at a hole.

## Scope

Only the product crate `webpkit` is mutated — its three zones (the `lossless`
and `lossy` codec modules and the codec-agnostic core shell). Tooling and
test-harness crates (xtask, `*-fuzz`, `*-proptest`, `*-conformance`, bench, cli,
samples, alloc-count) are never targeted — they exist to test the product, not
the other way round.

Configuration lives in [`.cargo/mutants.toml`](../.cargo/mutants.toml): nextest
as the runner, `--locked` on every inner build, generous timeouts for the slow
`Method::Best` paths, and an `exclude_re` list of provably-equivalent mutants
(each with a reason).

## Running it

```
just mutants                                          # full sweep of the product crate (slow)
just mutants --file crates/webpkit/src/lossless/huffman/canonical.rs   # one file (fast inner loop)
just mutants --shard 0/4                              # one quarter of the work
just mutants-diff main                                # only what changed vs a base ref (the CI gate)
```

The tool is version-pinned (`cargo:cargo-mutants` in `mise.toml`, and the same
version in the CI `mutants` job) so the generated mutants stay reproducible; bump
it explicitly rather than via `latest`.

## The CI gate

The `mutants` job in `.github/workflows/ci.yml` runs `cargo mutants --in-diff` on
pull requests only: it mutates just the lines the PR changes and fails if any
survive. A full sweep is far too slow to gate every push, but the diff-scoped run
is fast and keeps new product code honest. On `push` / `merge_group` the job
skips, and the `ci-required` aggregate counts a skip as a pass.

Triage a surviving mutant one of two ways:

1. **Kill it** — add or tighten a test so the mutation is caught. This is the
   default and the point of the exercise.
2. **Exclude it** — if the mutation is genuinely behavior-preserving (an
   equivalent mutant) or untestable, add a regex to `exclude_re` in
   `.cargo/mutants.toml` with a one-line reason. Prefer killing over excluding.

## Baseline

<!-- Updated after each full sweep. -->

Full-sweep status of the product crate's three zones (`cargo mutants` at
`PROPTEST_CASES=16`, the version pinned in `mise.toml`). The per-zone counts
below are from the pre-consolidation sweep of the former `webpkit-core` /
`webpkit-lossless` / `webpkit-lossy` crates; the merge into `webpkit` moved the
code without changing it, and the consolidated re-sweep (`mutants-full.yml`,
run once via `workflow_dispatch`) confirms the same totals:

| zone | generated | excluded | tested | caught | unviable | missed | timeout |
| --- | --: | --: | --: | --: | --: | --: | --: |
| `core` | — | 6 | — | 293 | — | **0** | 0 |
| `lossless` | 2199 | 42 | 2157 | 2035 | 122 | **0** | **0** |
| `lossy` | 4395 | 501 | 3894 | 3777 | 112 | **5** | **0** |

`missed` counts mutants a full sweep did not catch. Every product mutant is
caught, unviable, or an excluded/documented equivalent, with two caveats worth
recording:

- **The `lossy` zone reports 5 "missed".** All five are the `StructField`-genre
  `delete field …` mutants in `frame.rs` (`choose_filter` / `apply_loop_filter`),
  each of which deletes a struct field whose value the `..Default::default()`
  spread — or, for `skip`, `resolve_finfo`'s `(non_zero_y | non_zero_uv) == 0`
  fallback — already supplies. They are **provably value-preserving equivalents,
  not a coverage gap**. cargo-mutants 27.1.0 does not honor `exclude_re` for the
  `StructField` genre (verified: even the exact mutant name does not match), so
  they cannot be filtered like the other equivalents; the fields are kept
  explicit in the source for readability and decoder-mirroring, so they are left
  documented rather than refactored away to appease the tool. See the NOTE in
  [`.cargo/mutants.toml`](../.cargo/mutants.toml).
- The excluded counts are provably-equivalent or untestable mutants (each with a
  one-line reason in `.cargo/mutants.toml`): disjoint-bit `|`/`^`, unique-key or
  unreachable-boundary comparisons, capacity-only hints, value-preserving no-ops,
  non-terminating loops observable only as a timeout, and — the bulk of lossy's
  501 — code behind `cfg(feature = "oracle" | "rayon" | "bench")` that the
  default sweep does not build (covered instead by the corresponding
  feature-gated tests, e.g. `wavefront_planner_matches_serial_byte_for_byte`
  under `--features rayon`).
