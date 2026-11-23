---
date: 2026-11-11
---

# Opt-in deterministic parallel encoding

## Status

Proposed

## Context

Lossy planning and lossless candidate evaluation support optional Rayon execution.
Entropy emission and scheduling costs limit the work that benefits from parallelism.

## Decision

Keep `rayon` outside the default feature set.
Preserve deterministic planning, commit order, and tie resolution across serial and parallel builds.
Use serial planning for small frames and the fast path without mode search.
Keep the supported Rust floor of 1.88.

## Alternatives

Default parallelism would simplify configuration for some consumers but add dependencies and require `std`.
Larger scheduling units require measurements demonstrating benefits without changing output.

## Consequences

Consumers explicitly enable parallelism and its dependency graph.
The default codec remains dependency-free.
No fixed speedup ceiling follows from the available source.
Reconsider defaults or scheduling when representative measurements justify the added cost.

## Confirmation

Workspace tests enable `webpkit/rayon` and compare serial and parallel results.
The host `no_std` check covers the allocation-only build.
Timing claims require the separate measurements described in [benchmarking](../benchmarking.md).
