# webpkit — task entry points. Tools come from mise (`mise.toml`) / PATH.
# Recipes are thin wrappers; webp-specific logic lives in `cargo xtask`.
#
# Setup: `mise install` then `just hooks`. Health check: `just doctor`.

# Fall back to `cargo test` when nextest isn't installed.
nextest_present := `command -v cargo-nextest >/dev/null 2>&1 && echo 1 || echo 0`

# Minimum line coverage (of the product crate) enforced by `just coverage` / CI.
# Product coverage sits ~95%; the floor leaves headroom without hiding regressions.
coverage_floor := "90"
# Non-product crates excluded from the coverage gate (tooling / test harnesses).
cov_exclude := '(xtask|webpkit-alloc-count|webpkit-bench|webpkit-cli|webpkit-samples|webpkit-conformance|webpkit-lossless-conformance|webpkit-lossy-conformance|webpkit-fuzz|webpkit-lossless-fuzz|webpkit-lossy-fuzz|webpkit-lossy-proptest)'
# The product crate that mutation testing (`just mutants`) targets; tooling and
# test-harness crates are never mutated.
mut_packages := "-p webpkit"
# Declared MSRV, verified by `just msrv` / the CI msrv gate. Keep == Cargo.toml.
# Floor pinned by let-chains, `is_multiple_of`, and the optional `image` dep (all 1.88).
msrv := "1.88"

default:
    @just --list

# ----- bootstrap / health -----

# One-shot setup for a fresh clone. Idempotent.
bootstrap:
    @echo "==> 1/4 rustup components + msvc target"
    rustup component add rustfmt clippy rust-src
    rustup target add x86_64-pc-windows-msvc
    @echo "==> 2/4 mise install (see mise.toml)"
    mise install
    @echo "==> 3/4 lefthook install (git hooks)"
    mise exec -- lefthook install
    @echo "==> 4/4 bun install (commitlint, commit-msg hook)"
    mise exec -- bun install
    @mise exec -- just doctor
    @echo "bootstrap done. Try: just build / just test / just lint / just conformance"

doctor: doctor-native

# Verify every dev tool the recipes rely on is reachable. Exits non-zero on the
# first missing required tool; optional tools are a soft warning.
doctor-native:
    @echo "==> webpkit doctor"
    @bash -c 'set -e; \
        check() { printf "  %-16s " "$1"; out=$($2 2>&1 | head -1) && printf "ok    %s\n" "$out" || { printf "MISSING\n"; exit 1; }; }; \
        soft()  { printf "  %-16s " "$1"; out=$($2 2>&1 | head -1) && printf "ok    %s\n" "$out" || printf "warn  (optional) not found\n"; }; \
        check rustc         "rustc --version"; \
        check cargo         "cargo --version"; \
        check cargo-nextest "cargo nextest --version"; \
        check cargo-deny    "cargo deny --version"; \
        check taplo         "taplo --version"; \
        check biome         "biome --version"; \
        check yamlfmt       "yamlfmt --version"; \
        check typos         "typos --version"; \
        check actionlint    "actionlint -version"; \
        check lefthook      "lefthook version"; \
        check just          "just --version"; \
        check bun           "bun --version"; \
        check cwebp         "cwebp -version"; \
        check dwebp         "dwebp -version"; \
        soft  cargo-fuzz    "cargo fuzz --version"; \
        soft  cargo-insta   "cargo insta --version"; \
    '
    @echo "==> doctor: ok"

# ----- build / test -----

build:
    cargo build --workspace --all-targets

test:
    @if [ "{{nextest_present}}" = "1" ]; then \
        echo "==> cargo nextest run --workspace"; \
        cargo nextest run --workspace; \
    else \
        echo "==> nextest absent -> cargo test --workspace"; \
        cargo test --workspace; \
    fi
    @echo "==> cargo test --doc --workspace"
    cargo test --doc --workspace

# ----- format / lint -----

fmt:
    cargo fmt --all
    cargo sort --workspace
    taplo fmt
    biome format --write .
    yamlfmt .

fmt-check:
    cargo fmt --all -- --check
    cargo sort --workspace --check
    taplo fmt --check
    biome format .
    yamlfmt --lint .

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

deny:
    cargo deny check advisories bans licenses sources

typos:
    typos

typos-fix:
    typos --write-changes

actionlint:
    actionlint .github/workflows/*.yml

# Build rustdoc under `RUSTDOCFLAGS=-D warnings` (mirrors CI's doc gate).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Aggregated merge gate (mirrors CI).
lint:
    just fmt-check
    just clippy
    just deny
    just typos
    just actionlint

# ----- conformance / verification (webp) -----

# Regenerate golden fixtures from libwebp cwebp/dwebp (needs cwebp/dwebp on PATH).
gen-fixtures:
    cargo xtask gen-fixtures

# Run every codec's conformance fixtures and drift-gate its committed ledger.
# Each `*-conformance` crate owns an in-crate `tests/ledger.rs` gate (all symmetric).
conformance:
    cargo test -p webpkit-lossless-conformance -p webpkit-lossy-conformance -p webpkit-conformance

# Fail if any committed conformance ledger drifts from a fresh run (the `ledger` gate).
drift-gate:
    cargo test -p webpkit-lossless-conformance -p webpkit-lossy-conformance -p webpkit-conformance --test ledger

# Regenerate every committed conformance ledger from the current fixtures.
# Ledger writing is tool-free (it recomputes from committed fixtures), so no
# `--features oracle` here — the alpha/anim generators used to be oracle-gated and
# this recipe silently regenerated 2 of 4, reporting success while touching neither.
gen-ledgers:
    cargo test -p webpkit-lossless-conformance -p webpkit-lossy-conformance -p webpkit-conformance --test ledger -- --ignored

# Sweep the committed image corpus for decoder/encoder regressions.
corpus-sweep:
    cargo xtask corpus-sweep

# (Re)author the corpus byte-golden baseline after an intended output change.
corpus-bless:
    cargo xtask corpus-sweep --bless

# Measure the shared synthetic corpus and gate corpus/metrics.json.
# Built --release: the encode-heavy Best search is ~13x faster than debug
# (~42s vs ~570s), keeping the run inside its ~120s budget. Encoded sizes are
# deterministic and profile-independent, so the release gate matches any run.
# Pinned to `+1.96` to match CI because the ledger's peak-memory fields are
# toolchain-sensitive, so the committed numbers only reproduce on this exact
# compiler. This pin is unrelated to the MSRV ({{msrv}}) — do not sync them.
# (The `cargo xtask` alias can't select --release, so invoke cargo directly.)
metrics:
    cargo +1.96 run --release --quiet -p xtask -- metrics

# (Re)author the compression-metrics ledger after an intended size change.
# `+1.96` to match the `metrics` gate (peak-memory reproducibility, not the MSRV).
metrics-bless:
    cargo +1.96 run --release --quiet -p xtask -- metrics --bless

# Measure the LOSSY encoder over the sample matrix and gate corpus/metrics-lossy.json
# (size / ratio / reconstruction sse / peak memory, per method x quality). Same
# --release + `+1.96` pin as `metrics` (its peak-memory fields are
# toolchain-sensitive); the quality field is integer sse, so the ledger stays
# byte-golden. Best is capped to edge<=256 for runtime.
metrics-lossy:
    cargo +1.96 run --release --quiet -p xtask -- metrics --lossy

# (Re)author the lossy metrics ledger after an intended size/quality change.
metrics-lossy-bless:
    cargo +1.96 run --release --quiet -p xtask -- metrics --lossy --bless

# Measure deterministic algorithmic-work counters and gate corpus/work.json.
# --release for runtime budget only; unlike `metrics`, the counts are integer
# event tallies that are toolchain- AND profile-independent, so NO +1.96 pin is
# needed (any compiler reproduces them). `--features work-count` links the
# counters into both codecs and the xtask `work` command.
work:
    cargo run --release --quiet --features work-count -p xtask -- work

# (Re)author the work-cost ledger after an intended algorithmic change (verify
# byte-invariance separately with `just metrics` + `just corpus-sweep`).
work-bless:
    cargo run --release --quiet --features work-count -p xtask -- work --bless

# (Re)generate the shell completions and man pages committed under
# crates/webpkit-cli/assets/. They ship in the published tarball so packagers get
# them without a build step, and `webpkit-cli`'s `ledger` test byte-compares them
# against the binary — so run this after any change to the `webp` flag surface.
gen-assets:
    @bash -c 'set -e; \
        out=crates/webpkit-cli/assets; \
        run() { cargo run --quiet -p webpkit-cli --bin webp -- "$@"; }; \
        mkdir -p "$out/completions" "$out/man"; \
        for sh in bash zsh fish powershell elvish; do \
            echo "  completions/webp.$sh"; \
            run completions "$sh" > "$out/completions/webp.$sh"; \
        done; \
        echo "  man/webp.1"; \
        run man > "$out/man/webp.1"; \
        for c in encode decode convert info explain completions man; do \
            echo "  man/webp-$c.1"; \
            run man "$c" > "$out/man/webp-$c.1"; \
        done'

# Criterion benchmarks.
bench:
    cargo bench -p webpkit-bench

# Isolated per-kernel microbenchmarks for the autovectorization loop. Each kernel
# is timed against its pre-optimization `*_reference` twin in the SAME run, so the
# opt-vs-ref delta comes from two point estimates measured back-to-back (no
# cold-baseline bias). Local-only; never a CI gate.
#   just bench-kernels               # all kernels
#   just bench-kernels sse_block     # filter to one kernel's group
bench-kernels filter="":
    cargo bench -p webpkit-bench --features bench --bench kernels -- "{{filter}}"

# In-process comparison vs the libwebp C reference -> self-contained HTML
# dashboard (size, lossy quality, and wall-clock speed). Links libwebp via
# `libwebp-sys` and measures both codecs in the same process (no CLI/IO skew).
# Local-only (timing is machine-dependent); the report is written under target/.
#   just report-vs-libwebp                    # -> target/vs-libwebp.html
#   just report-vs-libwebp out.html
report-vs-libwebp out="target/vs-libwebp.html":
    cargo run -p webpkit --example vs_libwebp --features oracle --release -- "{{out}}"

# Encode/decode throughput over a directory of real images (print-only, NOT gated).
# `dir` is a pure runtime path — nothing is written into it or the repo, and no
# image path is recorded. Built with the fast `quick` profile (optimized, no fat
# LTO) so rebuilds after an encoder edit are seconds; timing deltas stay valid as
# long as before/after both use it. Needs cwebp on PATH (soft-skips otherwise).
#   just bench-real ~/images                 # full corpus, 512px cap, best-of-5
#   just bench-real ~/images 256 1 3         # 256px cap, best-of-1, first 3 images
bench-real dir max_edge="512" iters="5" limit="0":
    cargo run --profile quick --quiet -p xtask -- bench-real "{{dir}}" --max-edge {{max_edge}} --iters {{iters}} --limit {{limit}}

# Line coverage of the product crate with a hard floor (tooling crates excluded).
coverage:
    cargo llvm-cov nextest --workspace --ignore-filename-regex {{cov_exclude}} --fail-under-lines {{coverage_floor}}

# Mutation-test the product crates; survivors mark assertion gaps. Long by
# default — narrow with pass-through args (`just mutants --file <path>` / `--shard 0/4`).
mutants *args:
    cargo mutants {{mut_packages}} --test-tool nextest {{args}}

# Reproduce the CI PR gate locally: mutation-test only lines changed vs base (default origin/main).
mutants-diff base="origin/main":
    @mkdir -p target
    git diff "{{base}}...HEAD" > target/mutants-diff.patch
    cargo mutants --in-diff target/mutants-diff.patch {{mut_packages}} --test-tool nextest

# Verify the declared MSRV over the consumer surface of the published crates
# (default / feature opt-ins / no_std). Not `--all-targets`: dev/tooling deps
# (e.g. cargo-platform) need a newer rustc and no consumer builds them.
msrv:
    cargo +{{msrv}} check -p webpkit -p webpkit-cli --locked
    cargo +{{msrv}} check -p webpkit --features rayon,image --locked
    cargo +{{msrv}} check -p webpkit --no-default-features --features alloc --locked

# Build the product crate as a no_std library (host + bare-metal) to prove no
# accidental `std::` use crept into production code.
no-std:
    rustup target add thumbv7em-none-eabi
    cargo build -p webpkit --no-default-features --features alloc
    cargo build -p webpkit --no-default-features --features alloc --target thumbv7em-none-eabi

# In-process differential against the libwebp reference (links libwebp-sys) —
# both codecs plus the umbrella alpha/animation cross-checks. All three oracle
# suites now live in the one `webpkit` crate.
oracle:
    cargo test -p webpkit --features oracle --test oracle --test oracle_lossless --test oracle_lossy

# Run the umbrella round-trip example (lossless + lossy through the one API).
example:
    cargo run -p webpkit --example roundtrip

# Brief fuzz run (needs nightly + cargo-fuzz) across every fuzz crate and target.
fuzz-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    for crate in webpkit-lossless-fuzz webpkit-lossy-fuzz webpkit-fuzz; do
      for dir in crates/"$crate"/seeds/*/; do
        target=$(basename "$dir")
        echo "==> fuzz-smoke $crate/$target"
        mkdir -p "crates/$crate/corpus/$target"
        cp -n "crates/$crate/seeds/$target/"* "crates/$crate/corpus/$target/" 2>/dev/null || true
        cargo +nightly fuzz run "$target" --fuzz-dir "crates/$crate" --features fuzzing -- -runs=16384
      done
    done

# ----- git hooks -----

hooks:
    lefthook install
    bun install

# ----- lefthook delegated recipes (do not run directly) -----

_hook-fmt +files:
    cargo fmt -- {{files}}

_hook-cargo-sort:
    cargo sort --workspace

_hook-taplo-fmt +files:
    taplo fmt {{files}}

_hook-biome-format +files:
    biome format --write {{files}}

_hook-yamlfmt +files:
    yamlfmt {{files}}

_hook-typos-fix +files:
    typos --write-changes {{files}}

_hook-actionlint +files:
    actionlint {{files}}

_hook-commitlint msg_path:
    bunx commitlint --edit {{msg_path}}
