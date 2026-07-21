<!--
Thanks for contributing! A few reminders (see CONTRIBUTING.md for the full loop):
- Commits follow Conventional Commits (feat: / fix: / perf: / docs: / …); PRs are squash-merged.
- Do not hand-edit generated artifacts (conformance goldens, conformance-results-*.json) — regenerate them.
-->

## What & why

Describe the change and the motivation. Link any related issue (`Closes #123`).

## Linear

Closes DEV-___
<!-- Links this PR to its Linear issue; requires the Linear GitHub integration. -->

## Checklist

- [ ] `just lint` passes (fmt, clippy `-D warnings`, cargo-deny, typos, actionlint)
- [ ] `just test` passes
- [ ] New or changed product code is covered by a test that pins its behavior
      (the `mutants` CI gate mutation-tests the diff)
- [ ] Generated artifacts were regenerated with `just gen-fixtures` / `just conformance`, not hand-edited
- [ ] Docs / CHANGELOG updated if user-facing
