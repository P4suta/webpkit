# Architecture decision records

[Comparison corpus provenance](0001-comparison-corpus-provenance.md) separates public regression fixtures from local private-image measurements.

[Portable scalar codec boundary](0002-simd-unsafe-boundary.md) records the proposed product and tooling safety boundary.

[Opt-in deterministic parallel encoding](0003-rayon-default-and-parallel-ceiling.md) records feature defaults and deterministic scheduling.

[Explicit cwebp flag compatibility](0004-cwebp-residual-flags.md) records warnings, typed hints, and rejected transformations.

[Affected-work selection](affected-work-selection.md) defines the ordinary check comparison boundary.
[Release-only heavy checks](release-only-heavy-checks.md) defines qualification timing.

Recovered proposals keep their filenames and await owner acceptance.
New records use descriptive paths with a front-matter date.
Use Context, Decision, Alternatives, Consequences, and Confirmation sections.
Keep superseded records and link their successors.
