# Contributing

Contributions to webpkit are welcome. webpkit is a pure-Rust WebP codec —
**VP8L** (lossless) and **VP8** (lossy), decode and encode — developed
**test-first** and verified against golden fixtures produced by libwebp's
`cwebp` / `dwebp`.

## Setup

The toolchain is pinned via [mise](https://mise.jdx.dev/) (`mise.toml`); tasks
run through [just](https://github.com/casey/just). The Rust toolchain itself is
owned by `rust-toolchain.toml`.

```
mise install        # install pinned tools (incl. cwebp/dwebp via the http backend)
just bootstrap      # one-shot setup: rustup components, hooks, commitlint deps
just doctor         # verify the environment matches the pins
```

Declare tools in `mise.toml` and install via `mise install`; do not add them ad
hoc.

## Dev loop

```
just lint     # fmt-check + clippy -D warnings + cargo-deny + typos + actionlint
just test     # cargo nextest (falls back to cargo test) + cargo test --doc
just doc      # rustdoc under -D warnings
```

## Verification methodology

webpkit follows an external-verification discipline:

- **Golden fixtures.** Cases live under `crates/webpkit-lossless-conformance/fixtures/{decode,encode}/<case>/`
  as `meta.toml` + `input.*` + `expected.*`. Goldens are produced by libwebp
  (`cwebp` / `dwebp`) — **never hand-edited.** Regenerate with `just gen-fixtures`.
- **Conformance ledger.** Each codec's `*-conformance` crate owns an in-crate
  `tests/ledger.rs` drift-gate over its committed `conformance-results-*.json`
  ledger — all three (VP8L, VP8, alpha/anim) symmetric. `just conformance` /
  `just drift-gate` run the gates; `just gen-ledgers` regenerates the ledgers
  after an intended output change.
- **Property + fuzz + differential tests.** Both codecs carry proptest suites
  (the lossy strategies are shared via `webpkit-lossy-proptest`) and cargo-fuzz
  targets (`webpkit-lossless-fuzz` / `webpkit-lossy-fuzz` / `webpkit-fuzz`,
  `just fuzz-smoke`); the differential path cross-checks against libwebp behind
  the `oracle` feature.
- **Mutation testing.** [cargo-mutants](https://mutants.rs/) injects deliberate
  bugs into the product crates; a surviving mutant is an assertion the tests do
  not make. `just mutants` runs the full (slow) sweep; `just mutants --file <path>`
  narrows it. CI gates each PR on `--in-diff`, so new or edited product code must
  ship with a test that catches its mutations. See `docs/mutation-testing.md`.
- **TDD.** Write the failing test / fixture first, then make it pass.

## Commit / PR rules

- [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` /
  `perf:` / `docs:` / `refactor:` / `test:` / `chore:` / `ci:` / `build:`).
  commitlint (`commitlint.config.mjs`) lints every commit in a PR.
- **Squash-merge only.**
- Releases are cut by [release-plz](https://release-plz.dev): it opens a release PR
  that bumps the version + CHANGELOG from conventional commits, then on merge
  publishes to crates.io (cargo-semver-checks gates breaking changes) and tags.

## Before pushing

- `just lint` and `just test` green (the `lefthook` pre-push hook runs both).
- **Do not hand-edit generated artifacts:** conformance goldens and the
  `conformance-results-*.json` ledgers. Regenerate them via `just gen-fixtures` /
  `just gen-ledgers`.
- Do not bypass hooks with `--no-verify`. If a hook fails, fix the cause.

## License

Contributions are accepted under the project's dual MIT / Apache-2.0 license
([`LICENSE-MIT`](LICENSE-MIT) / [`LICENSE-APACHE`](LICENSE-APACHE)).
