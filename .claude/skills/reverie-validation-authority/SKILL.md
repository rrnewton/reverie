---
name: reverie-validation-authority
description: "Interpret Reverie's centralized local validation rows without confusing diagnostic validate.sh output, safe-ci containment, or hosted observations."
---

# Reverie validation authority

Use this skill for every Reverie local-validation evidence decision. It defines
what `ci-hub validate-run --repo rrnewton/reverie` measures and what its ledger
row means. By owner directive, GitHub Actions is off for validation authority;
hosted results are diagnostics and never a green, veto, or landing prerequisite.

## Producer path

The only centralized local producer is:

```text
ci-hub validate-run --repo rrnewton/reverie
  -> detached systemd user service
  -> ci-hub validate-lock
  -> safe-ci-dag-runner (cgroups required, fail closed)
  -> Reverie ./validate.sh --no-label-pr
  -> ledger/reverie/<host>/<month>.jsonl
```

Never run the product driver outside `safe-ci-dag-runner` and then describe it
as centralized validation. Never pass `--allow-cgroup-failure` or
`--unsafe-no-cgroups`: escaping the agent sandbox is not permission to run
without the project sandbox. If cgroup boxing cannot be established, the run is
NO_RESULT and must be rerun after repairing the environment.

## Qualifying row

`ci-hub validate-status --repo rrnewton/reverie --sha <exact-sha>` is the only
reader. Labels, direct `validate.sh` rows, and comments are caches or
diagnostics. A qualifying row carries all of these facts together:

- `repo=rrnewton/reverie`, the exact 40-hex commit and tree, clean tree, and
  commit anchoring;
- schema 5+, producer `ci-hub-reverie-validate-run`, policy
  `reverie-local-validation/v1`, full/full selection, exit 0, and raw/result
  pass;
- all seven gates, in order, each with a terminal pass: cross-client skill
  discovery, merge-gate policy, workspace build, regular workspace tests,
  documentation tests, clippy, and rustfmt;
- nonzero executed tests plus declared coverage of both test-bearing gates,
  with no zero-execution or absent test gate;
- live `ci-hub-validate-lock` owner ancestry, zero concurrent validates, and an
  exact target-bound authority snapshot; and
- observed `safe-ci-dag-runner` outer scope plus per-step cgroup, with unboxed
  fallback explicitly false.

Missing is unknown, not false. A malformed, direct, stale, cross-repository,
unboxed, zero-execution, incomplete, or red row never qualifies. A red complete
row remains durable adverse evidence; absence of a qualifying green must never
erase it.

## Repository binding

Repository identity is part of the receipt key. The reader filters by repository
*before* resolving a SHA or SHA prefix. A Hermit row and a Reverie row with the
same commit text are different evidence and cannot authorize each other.

## Current landing boundary

The canonical exact-head local LEDGER row is Reverie's validation authority.
Missing, malformed, stale, incomplete, unboxed, zero-execution, or red local
evidence blocks landing. GitHub Actions remains observable diagnostics only;
never wait for, dispatch, or cite it as the operative validation decision.
