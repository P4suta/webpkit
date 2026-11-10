---
date: 2026-11-11
---

# Explicit cwebp flag compatibility

## Status

Proposed

## Context

Drop-in callers need explicit handling of unsupported encoder flags.
Ignoring a requested pixel transformation would produce unintended output.

## Decision

Accept `-af` and `-map` as compatibility no-ops with stderr warnings.
Consume the integer argument to `-map`.
Keep existing runtime compatibility no-ops silent.
Reject `-blend_alpha` and the unsupported dithering bit of `-pre`.
Parse valid `-hint` names into `ImageHint`.
The current lossy emitter doesn't consult that classification.

## Alternatives

Rejecting every unsupported optimization breaks otherwise usable invocations.
Accepting every flag without warnings conceals missing transformations.
Adding each effect requires separate behavior and oracle tests.

## Consequences

Compatibility callers can continue with explicit warnings about omitted effects.
Pixel transformations remain errors until implemented.
Reconsider each disposition when its requested effect becomes available.

## Confirmation

The [flag table](../../crates/webpkit-cli/src/cli/cwebp.rs) defines the dispositions.
[Drop-in tests](../../crates/webpkit-cli/tests/dropins.rs) check warnings, argument validation, rejection, and byte-neutral hint handling for the covered inputs.
