---
date: 2026-11-11
---

# Comparison corpus provenance

## Status

Proposed

## Context

Committed comparison ledgers need public inputs that a clean checkout can reproduce.
Private photographs aren't available to public contributors.

## Decision

Build committed ledgers from the deterministic corpus in `crates/webpkit-samples`.
Keep private-image measurements local, with paths supplied at runtime and no private names or results committed.
Treat synthetic ledgers as regression checks rather than evidence of photographic compression quality.
Review compression changes using local category and total results before adopting them.

## Alternatives

A licensed public photographic corpus would improve reproducibility and representativeness, but requires provenance review and maintained fixtures.
Private inputs can't serve as a public gate.

## Consequences

Public regression checks remain reproducible.
Their synthetic content limits conclusions about photographs.
Reconsider the corpus when a redistributable photographic set is available.

## Confirmation

The workspace conformance tests check committed fixtures.
The measurement commands in [benchmarking](../benchmarking.md) separate local measurements from committed ledgers.
Performance adoption still requires representative measurements.
