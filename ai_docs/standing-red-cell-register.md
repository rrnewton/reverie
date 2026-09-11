# Standing red-cell register

Produce the register from freshly fetched `dev-hermit` and Hermit `origin/main`:

```bash
./scripts/red-cell-register.py > ignored/red-cell-register.json
```

The command refuses a shallow Hermit history, a red cell without a current
reason, an unknown measurement state, an undated current value, or an
observation event that is missing from the published results series. It runs
`./ci-hub/ci-hub validate-status --limit 12`; raw validation shard files are not
used as a validation answer. It also runs `./ci-hub/ci-hub active-work --json`.
If the current agent assignment cannot be established, every cell says that
current work is unknown rather than saying that nobody is investigating it.

For every current red cell the register reports:

- the exact test, mode, and backend;
- `red` as the scorecard status and `not selected` as its meaning;
- the first-parent commit and date at which the current red status was
  established on main;
- the current structured reason and the separate commit and date at which that
  reason was established;
- the scorecard measurement state and the separate commit and date at which
  that state was established;
- the published results-series events named by the scorecard; and
- current claimed tasks whose complete TaskGraph record mentions the exact
  cell id or test id.

`red` does not mean that a run failed. The measurement state remains separate:
`never-measured`, `measured-and-passed`, `measured-no-verdict`,
`diverged-unlocated`, or `diverged`.

Each output records `verified_at`, `quotable_until`, the exact source commits
and scorecard blob, the refresh commands, the reader commands and their output
hashes, and the command that produced every row. After `quotable_until`, run
the command again before quoting a cell or a count.
