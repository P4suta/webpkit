---
date: 2026-10-26
---

# Release-only heavy checks

## Status

Proposed.

## Context

Ordinary CI repeated conformance and Rayon tests already included in the workspace inventory.
Operating-system matrices, measurement runs, reference comparisons, coverage, and mutation testing also ran independently of a publication candidate.
The summary jobs accepted skipped work, while publication had no dependency on successful source qualification.

Release-plz can select an associated release pull request's commit and restore the original checkout afterward.
A check of `HEAD` after publication can't establish which source the publisher used.
Setting `release_always = true` alone leaves that internal selection active in [release-plz 0.3.174](https://github.com/release-plz/release-plz/blob/156f0bb78b6e3c1559e568aef979d59ce69cff6f/crates/release_plz_core/src/command/release.rs).

## Decision

Run one Linux quality job for pull requests and merge groups.
Keep workspace tests, conformance ledgers, stable seed replay, documentation, image checks, and a host `no_std` build in `just check`.
Enable `webpkit/image` and `webpkit/rayon` explicitly for workspace checks.
The CLI already enables both features in the resolved workspace graph.
Use the shared workspace suite for their tests and doctests rather than repeating the codec suite in a separate image-feature run.
Require the exact successful ordinary role inventory, permitting only the title-job skip defined for merge groups.
Remove duplicate main-push runs and separate repetitions of workspace suites.

Resolve a release candidate from complete repository, commit, and pull-request data before starting expensive work.
Qualify the exact current main commit identified as the recognized release proposal's merge commit.
An ordinary main commit without a release association starts no qualification campaign.
Malformed, incomplete, ambiguous, or inconsistent metadata is an error.

Qualify the selected full commit through the fixed role inventory in `release-plz.yml`.
Keep native platform tests, compatibility builds, corpus and measurement ledgers, reference suites, coverage, full product mutation testing, policy proofs, and fixed-count fuzz discovery.
Check tracked source and the commit identity before and after each role.
Require every role to report success for the same commit before admitting publication.
The existing fuzz-linking defect remains a release blocker rather than a successful skip.

Keep the tracked release-plz configuration and the proposal command unchanged.
Create a publisher-only configuration from the tracked TOML, overriding only `workspace.release_always` and `workspace.pr_branch_prefix`.
Use `true` and `qualified-source:` respectively.
Git [forbids a colon in a reference name](https://git-scm.com/docs/git-check-ref-format#_description).
The pinned publisher compares that prefix with the actual pull-request head reference, so no valid reference can select its temporary-checkout branch.
The existing action receives the derived configuration through its supported `config` input and publishes from the qualified checkout.
Pin the inspected CLI version and revalidate source and metadata immediately before that action.

Keep pure admission decisions in the dependency-free `no_std` core imported from `xtask/src/ci.rs`.
Keep Git, JSON, TOML, metadata transport, and workflow outputs in the adapter.
Source-bound Kani harnesses cover commit parsing, candidate selection, and complete ordinary and release admission.
Actual workflow tests check job structure and command bindings.
Proofs assume the adapter's validated metadata and Git checkout facts.
They don't prove remote service behavior.

## Consequences

Routine changes run a single deterministic quality gate.
Release qualification still requires the checks that establish platform compatibility and codec behavior.
Manual investigation and ledger regeneration remain available without granting publication approval.
Skipped, failed, cancelled, missing, stale, or different-source results can't qualify a candidate.

The source binding depends on the inspected release-plz version and valid GitHub reference data.
An upgrade must preserve the publisher-only selection behavior or replace it with a checked alternative.
The workflow binds qualification to source identity.
Package reproducibility and remote publication transactions need separate evidence.
Unrun proof and discovery campaigns supply no release-verification result.
