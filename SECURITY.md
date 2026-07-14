# Security Policy

webpkit is a codec that parses untrusted, attacker-controlled input (WebP / VP8L
/ VP8 bitstreams). Memory safety and robust bounds handling are core goals — the
whole tree is `#![forbid(unsafe_code)]` and continuously fuzzed — so we take
security reports seriously.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub's [private vulnerability
reporting](https://github.com/P4suta/webpkit/security/advisories/new)
(Security → Advisories → *Report a vulnerability*). Include:

- the affected version / commit,
- a minimal reproducer (a crashing input file is ideal), and
- the observed impact (panic, hang, excessive memory, incorrect output).

We aim to acknowledge a report within a few days and will keep you updated as we
investigate. Once a fix is available we will coordinate a disclosure timeline
with you and credit you in the advisory unless you prefer otherwise.

## Scope

In scope: crashes, panics on untrusted input, unbounded memory / CPU on crafted
input, and any decode result that diverges from the WebP specification in a way
that affects safety.

Out of scope: issues that require a `panic`/`unwrap` reachable only from trusted,
developer-supplied API misuse, and denial-of-service that is documented as a
tunable limit (for example, an explicitly opt-out pixel/size guard).

## Hardening notes

- **Allocation limits.** `decode` / `decode_frames` apply no allocation cap by
  default beyond the per-side dimension limit (16384, ≈268 Mpx ≈ 1 GiB of RGBA),
  matching libwebp's least-surprising behavior. When decoding untrusted input,
  pass `DecodeOptions::max_pixels` via `decode_with` / `decode_frames_with` to
  reject oversized headers *before* any buffer is allocated.
- **Integer overflow.** The release profile builds with `overflow-checks = false`
  for speed, so overflow safety does not rely on the runtime trap. Every buffer
  size or index derived from an untrusted header is computed with explicit
  `checked_*` / `saturating_*` arithmetic and validated before allocation. The
  `dev`/`test`/`ci` profiles keep overflow checks enabled, so the property, fuzz,
  and oracle suites would surface any unguarded arithmetic as a panic.
- **`unsafe`.** The published crates are `#![forbid(unsafe_code)]`. The only
  `unsafe` in the workspace is an isolated allocation-counting `GlobalAlloc` used
  by internal benchmarks (a `publish = false` crate that is never a dependency of
  the codec).

## Supported versions

Pre-1.0: only the latest `main` receives security fixes.

| Version | Supported |
| ------- | --------- |
| `main` (latest) | ✅ |
| older commits   | ❌ |
