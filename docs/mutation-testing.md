# Mutation testing

[cargo-mutants](https://mutants.rs/) changes production expressions and tests each change.
A surviving mutation identifies behavior that the tests didn't distinguish.
Passing line coverage alone doesn't establish that tests detect incorrect results.

## Scope

The committed policy in [`.cargo/mutants.toml`](../.cargo/mutants.toml) targets both published crates, `webpkit` and `webpkit-cli`.
Tooling, conformance, fuzz, and benchmark crates are outside that product mutation inventory.
The policy enables `image,rayon`, uses Nextest and locked inner builds, and retains the committed exclusions.
Keep the cargo-mutants version in `mise.toml` aligned with release qualification.

Product modules must remain reachable through literal `mod` declarations.
Cargo-mutants doesn't expand a macro-generated module tree.
The previous consolidation hid codec modules from discovery and invalidated the old separate-crate baseline.
Restoring literal declarations increased the observed inventory from 1,259 to 9,498 mutations.
That historical count supplies no passing result for current source.

## Running checks

```sh
just mutants
just mutants --file crates/webpkit/src/effort.rs
just mutants --shard 0/4
just mutants-diff origin/main
```

`just mutants` confirms the complete committed product policy.
File, shard, and diff selections support investigation.
An incomplete selection can't qualify a release.
Pull requests use deterministic regression tests in `just check` rather than automatically running mutation campaigns.
Release qualification requires the full policy to succeed for the selected commit.
The manual `mutants-full.yml` workflow supports ledger investigation and regeneration, not release approval.
Missing shards, surviving mutations, tool failures, and timeouts supply no passing result.

## Survivors and exclusions

Add or strengthen a deterministic test when a mutation changes observable behavior.
Exclude a mutation only when its equivalence has a specific justification.
Anchor an exclusion to a function, method, or return type rather than a changing line coordinate.
Keep the reason beside the expression in the committed policy.
Don't copy old exclusions onto rewritten code without confirming their validity.

## Baseline

The authoritative full sweep remains incomplete after the codec rewrite.
The old separate-crate totals don't verify the consolidated implementation.
The previous rewrite changed 34 product files, so old line-based exclusions also ceased to identify the intended expressions.

The recorded `effort.rs` spot check produced seven mutations: five caught, one unviable, and one surviving equivalent.
The equivalent changes `>` to `>=` in `Effort::level` at the clamp boundary.
Both branches return `MAX_LEVEL` at equality.
After excluding that expression by function name, the recorded repeat checked six mutations with no survivors.
The spot check covers no other codec zone and doesn't complete release qualification.
