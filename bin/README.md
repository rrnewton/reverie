# `bin/` — the hermit entry points an agent may call

Two commands. Reach for the second one; it runs the first.

## `bin/hermit-repeat` — run a guest N times, get the divergence distribution

The bread-and-butter tool for moving a red or yellow cell toward green: it runs
one guest N times and reports **where** the runs part company, as a
distribution rather than a single anecdote.

```bash
bin/hermit-repeat -n 20 -- /bin/echo hello                       # quiet-box baseline
bin/hermit-repeat -n 20 --mode concurrent --backend sabre -- ./g  # under contention
bin/hermit-repeat --selftest                                      # parsing self-check
```

- **One run = one `hermit run --verify --verify-strict`**, i.e. two back-to-back
  guest executions compared to each other. N runs give N independent
  observations. Runs are deliberately *not* compared across nodes: two
  separately-produced runs need byte-identical argv and environment to be
  comparable (`hermit log-diff --help` documents the measurements), and N nodes
  cannot have that.
- **N disconnected `safe-ci-dag-runner` nodes**, not a bespoke parallel runner,
  so scheduling, two-level cgroup boxing and resource accounting are the ones
  the rest of the repo already uses.
- **Every node goes through `bin/safehermit`**, so the easy path is the boxed
  path.
- **`--mode sequential` (default) and `--mode concurrent` measure different
  things** and are labelled as such in the output. Measured on
  `sabre` + `sh -c 'seq 1 2000 | wc -l'`, 2026-08-24: sequential 4/20 diverged,
  concurrent 13/20. Do not merge the two histograms.
- **Four coordinate units**, each with its keyspace named in the report:
  `info_record` (record ordinal in one log — *not* a line number),
  `sched_turn`, `virtual_ns`, and `syscall` (hermit's own `finish syscall #N`,
  which is **per-thread**, and not comparable across backends).
- **Clean, diverged, failed, no-result and not-run are five separate
  categories.** A run that crashed did not produce a divergence point and is
  never counted as one; a run that came back clean is a data point, not an
  absence.

Artefacts land in `ignored/hermit-repeat/<run-id>/`: `report.txt`,
`result.json`, `dag.json`, `runner.log`, and a per-node directory with the
`safehermit` bounds report and the node script that ran.

Tests: `bin/hermit-repeat-test.sh` (fast, hermetic) and `--selftest`.

## `bin/safehermit` — the only hermit entry point agents may call

A bounded, self-declaring wrapper around an arbitrary hermit binary. Every bound
is reported as `APPLIED` or `NOT_APPLIED` **with a reason, on every run**,
including when everything works — a wrapper that only speaks up when something
breaks becomes another inert mechanism.

```bash
bin/safehermit run -- ./prog                     # default hermit
bin/safehermit /tmp/hermit-patched run -- ./prog # arbitrary binary, absolute path
bin/safehermit --sh-help                         # the full rationale
```

It exists because on 2026-08-17 three ad-hoc hermit runs wrote ~4.5 TB of stderr
each and filled the filesystem. The incident binary was invoked by absolute
path, which never consults `PATH`, so a shim named `hermit` would have missed
precisely the case that caused the outage — hence the binary is an argument.

Tests: `bin/safehermit-test.sh`.

## Known gap

`scripts/bounded-run-space` is not present on `main` (branch
`runner-bounded-run-space_20260819` is unlanded), so `safehermit` reports
`bound.disk=NOT_APPLIED`. `hermit-repeat` applies its own total-artefact cap
(`--max-total-mb`, default 4096) because under `--verify` hermit's log goes to
files that `safehermit`'s 64 MiB **stderr** cap never sees.
