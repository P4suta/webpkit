---
date: 2026-10-28
---

# Affected-work selection

## Status

Proposed.

## Context

The ordinary quality gate runs codec tests, documentation, and compilation even when a change only edits workflow YAML.
CodeQL also runs both Rust and Actions analysis regardless of the affected source.
A failed or incomplete path comparison must preserve full verification.
Local hooks and hosted jobs need the same selection rules.

## Decision

Keep `just check` as the complete source gate and release qualification unchanged.
Use `just check-changes` for the local pre-push hook and ordinary hosted quality job.
The `ci-select` binary in the `webpkit-ci-select` package validates the Git comparison and calls the dependency-free production decision in `xtask/src/ci.rs`.
The `webpkit-ci-core` package stays independent of the standard library and external dependencies.
The quality gate and CodeQL language set are separate typed decisions.

Select `just check-workflows` only for a complete, nonempty set of recognized workflow YAML paths.
The workflow gate runs Actionlint, workflow YAML formatting, spelling, and the source-bound workflow contract tests.
The ordinary CI workflow always selects the complete gate because its configuration controls compilation and tests.
Other Rust, action-definition, unknown, or mixed changes select the complete gate.

Select Rust analysis for recognized Rust workspace inputs and Actions analysis for workflow YAML or nested action metadata.
Changes to the ordinary CI workflow, CodeQL workflow, unknown inputs, or mixed language inputs analyze both languages.
Every successful analysis plan contains at least one language.
Run both languages on main to publish the pull-request comparison baseline.
A successful later main run covers both categories when it cancels an earlier run.
Remove the weekly scan and cancel superseded runs of the same ref.
Renovate updates to the pinned CodeQL action change the scanner workflow and run both analyses.

Bind an explicit expected head to the checkout's complete commit identity before selection.
A contradictory or malformed expected identity is an error.
Default local comparisons use `origin/main`.
An unavailable or empty base, failed Git comparison, dirty checkout, empty difference, malformed path, or undecodable path selects the complete gate and both analyses.
Use `--no-renames` so a move contributes both the old and new paths.
Adding an unknown path can never reduce verification.
Dispatch and selection failures remain failures rather than successful skips.

Source-bound Kani harnesses cover the production classification and conservative fallback.
Symbolic names use eight safe ASCII bytes, with up to four preceding paths selected from the recognized categories.
Malformed-path cases cover seven explicit shapes.
Real Git fixtures cover comparison and dispatch behavior outside the verifier's semantics.
Workflow contract tests bind local and hosted commands to the selector and preserve strict ordinary summaries and release qualification.
The proof inventory runs at the release gate.
An unrun proof supplies no verified release result.

## Consequences

Workflow maintenance avoids unrelated codec tests and Rust analysis when the comparison proves their absence.
Uncertain changes run the complete gate.
The selector trusts successful Git command results, the checkout filesystem, and the configured Rust and Just executables.
Local checks cover the selector and adapters.
GitHub event delivery, action behavior, and remote publication require separate evidence.
New source categories require explicit classification and corresponding boundary tests before they can reduce work.
