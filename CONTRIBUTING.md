# Contributing

webpkit implements WebP decoding and encoding in Rust.
Use failing regression tests and libwebp fixtures to establish intended codec behavior before changing it.

## Setup

Install the tools declared in `mise.toml`, then run `mise x -- just bootstrap` and `mise x -- just doctor`.
`rust-toolchain.toml` selects the development compiler.
Use Git Bash for Just recipes on Windows.

## Development

Run `mise x -- just check-changes` before pushing.
The pre-push hook runs the same command.
The selector compares a clean checkout with `origin/main`, or the event's base commit in CI.
A nonempty change confined to workflow YAML runs Actionlint, YAML formatting, spelling, and workflow contract tests.
Changes to `.github/workflows/ci.yml` and uncertain comparisons run the complete gate.
Use `just check` to run the complete gate directly.
The complete gate checks formatting, Clippy, dependencies, spelling, workflows, workspace tests and doctests, documentation, the image feature, and the host `no_std` build.
Workspace checks explicitly enable `webpkit/image` and `webpkit/rayon`.
Workspace tests include conformance ledgers, stable fuzz seeds, image conversions, and the Rayon paths.
The revision corpus replay shares acceptance and round-trip assertions with its release-only fuzz target.
Pull requests and merge groups run one Linux quality job instead of repeating those suites in separate jobs.
CodeQL selects Rust or Actions analysis for recognized changes and analyzes both for unknown or mixed changes.
A change to the CodeQL workflow analyzes both languages.
Main-branch analysis runs both languages to maintain the pull-request comparison baseline without a scheduled scan.
Read [affected-work selection](docs/adr/affected-work-selection.md) for the comparison boundary and fallback behavior.

Use `just lint`, `just test`, or `just doc` for a narrower development check.
The supported Rust floor is 1.88.
Run `just msrv` before introducing a newer language or library feature.
Keep the workspace `rust-version` and the Justfile MSRV declaration consistent.
Update `Cargo.toml`, the Justfile, and CI together when raising the supported floor.
Treat an increase as a minor-version bump.

## Fixtures and verification

Keep generated goldens and `conformance-results-*.json` ledgers unchanged unless codec output intentionally changes.
Use `just gen-fixtures` to regenerate libwebp fixtures and `just gen-ledgers` to regenerate conformance ledgers.
`just conformance` and `just drift-gate` run the corresponding checks without rewriting them.

Release qualification checks the selected full commit on Windows and macOS, the MSRV, bare-metal `no_std`, corpus and measurement ledgers, reference suites, coverage, mutation testing, policy proofs, and fuzz discovery.
Those checks run before publication rather than on every pull request or a schedule.
The documented fuzz-linking defect leaves discovery incomplete until repaired.
A skipped or failed role can't qualify a release.
Read [the CI decision](docs/adr/release-only-heavy-checks.md) for source binding and [mutation testing](docs/mutation-testing.md) for survivor handling.

## Pull requests and releases

Use [Conventional Commits](https://www.conventionalcommits.org/) and describe the behavior change, its issue, and the checks performed.
Pull requests are squash-merged.
Release-plz prepares version proposals from conventional commits.
Publication requires complete qualification of the selected source and preserves semantic-version checks and the existing registry authentication.
A version proposal or ordinary main commit doesn't supply release verification.

## License

Contributions use the project's dual [`MIT`](LICENSE-MIT) and [`Apache-2.0`](LICENSE-APACHE) license.
