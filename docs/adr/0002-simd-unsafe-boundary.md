---
date: 2026-11-11
---

# Portable scalar codec boundary

## Status

Proposed

## Context

The public crate forbids unsafe code and supports portable Rust with `no_std + alloc`.
CPU-specific intrinsics and runtime dispatch would add implementations and validation obligations.

## Decision

Keep product codecs in safe scalar Rust.
Pursue algorithmic work reduction, allocation reduction, and safe scalar optimization.
Keep unavoidable allocator operations and libwebp foreign function interfaces in unpublished tooling crates outside production dependencies.

## Alternatives

An isolated vector implementation could improve throughput, but would require processor dispatch, architecture coverage, and evidence that output remains deterministic.
Retaining one portable implementation preserves the existing boundary.

## Consequences

Product code keeps one portable implementation at the cost of possible vector speedups.
Local timing observations don't establish a universal performance ceiling.
Reconsider the boundary only through a reviewed successor supported by representative benchmarks and compatibility checks.

## Confirmation

The public crate's `forbid(unsafe_code)`, the host `no_std` build, and workspace tests enforce the current implementation.
[Benchmarking](../benchmarking.md) describes separate timing measurements.
