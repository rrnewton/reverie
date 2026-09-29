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

### ⚠️ The bucket caps a cell and this tool does not — how two correct measurements disagree

The e2e bucket now wraps every cell in `timeout --kill-after=10s 15s bash -c '…'`,
a **per-cell deadline**. `bin/hermit-repeat --cell` deliberately does **not**
apply that cap: each node runs under its own `--deadline` instead. The tool
prints the difference on every `--cell` run, but it is worth understanding
before you compare any two numbers.

**Why it matters, with the cell it was observed on.** Measured 2026-08-27 at
hermit `5436f9349c5e`: `language-runtimes/python-dict-hash-iteration`
verify/ptrace takes **about 7.8 s per node** — roughly half the bucket's 15 s
cap. On a quiet box it finishes comfortably inside the cap either way, and the
bucket and this tool agree. On a loaded box the same cell can cross 15 s, at
which point **the bucket reports a timeout and this tool reports whatever the
cell actually did.** Neither measurement is wrong; they were taken under
different bounds.

That is the shape to watch for: a cell sitting at a large fraction of its cap is
one where bucket results and pressure-test results will start to disagree under
load, invisibly, with nothing in either output pointing at the cap. A cell at 7.8
of 15 s is already halfway there.

**So when a cell fails under load, establish which happened before reporting
it.** The report answers this directly: a failing run is shown either as
`BOUND FIRED: killed at the deadline, so this is not a cell verdict`, or with its
elapsed stated against the deadline and flagged `NEAR THE BOUND` at ≥ 80 %, with
a summary counting how many were killed by the bound, how many ran close to it,
and how many finished well inside it. A run with no elapsed recorded is reported
as unanswerable rather than as "not near the bound". A pressure test is
deliberately load, so it is the tool most likely to manufacture a bound-kill and
report it as flakiness if nobody looks.

Artefacts land in `ignored/hermit-repeat/<run-id>/`: `report.txt`,
`result.json`, `dag.json`, `runner.log`, and a per-node directory with the
`safehermit` bounds report and the node script that ran.

A `--cell` sweep also appends every run to the parent per-test series through
`ci-hub/series/series.py append-cells`. The series row uses the manifest test
id, `verify` mode, selected backend, Hermit commit, Detcore tree, measured wall
time when present, and the four separate divergence coordinates. A failed or
unresolved run is retained as a non-verdict; missing time or position is
omitted, never written as zero. Arbitrary guest commands and `--test-id` runs
do not claim a manifest-cell identity and therefore do not enter this store.

Tests: `bin/hermit-repeat-test.sh` (fast, hermetic) and `--selftest`.

## `bin/safehermit` — the only hermit entry point agents may call

A bounded, self-declaring wrapper around an arbitrary hermit binary. Every bound
is reported as `APPLIED` or `NOT_APPLIED` **with a reason, on every run**,
including when everything works — a wrapper that only speaks up when something
breaks becomes another inert mechanism.

```bash
bin/safehermit run -- ./prog                     # default hermit
bin/safehermit /tmp/hermit-patched run -- ./prog # arbitrary binary, absolute path
bin/safehermit --sh-resource-report /tmp/seed-7.resources \
  --sh-memory-max-bytes 17179869184 --sh-tasks-max 256 \
  --sh-cpu-cores 1 --sh-cpu-timeout 900 --sh-deadline 2700 \
  /tmp/hermit-patched run -- ./prog
bin/safehermit --sh-help                         # the full rationale
```

It exists because on 2026-08-17 three ad-hoc hermit runs wrote ~4.5 TB of stderr
each and filled the filesystem. The incident binary was invoked by absolute
path, which never consults `PATH`, so a shim named `hermit` would have missed
precisely the case that caused the outage — hence the binary is an argument.

`--sh-resource-report PATH` opts into accounting for the transient user unit
that sits outside a caller's worker cgroup. `PATH` must be new and have a real,
non-symlink parent directory. Before launch it atomically identifies the owned
`safehermit-*` unit so cancellation can reap only that unit; before unit cleanup
it is atomically completed with `memory_peak_bytes`, `cpu_usage_nsec`, and
`tasks_peak`. The report also binds those counters to `run_id`, the binary hash,
and a hash of the exact argv. A missing or malformed resource property fails
closed instead of manufacturing a zero measurement.

The metric file alone is not a run or qualification verdict. Callers must also
inspect the wrapper's actual exit status, its diagnostic
`resource_report_state`, and the report's breach fields. A unit-reference release
failure returns a nonzero status and reports `INCOMPLETE`, even when genuine
terminal metrics have already been published. Cancellation can also leave those
metrics available without any terminal diagnostic; an absent completion verdict
must not qualify. Do not interpret retained counters as successful completion or
substitute them for those status checks.

Detached accounting also requires positive integer values for
`--sh-memory-max-bytes`, `--sh-tasks-max`, `--sh-cpu-cores`,
`--sh-cpu-timeout`, and `--sh-deadline`. The payload remains behind a start gate
until the new unit's `memory.max`, `memory.swap.max=0`, `pids.max`, and `cpu.max`
match those declarations exactly. The completed report repeats the applied
limits and records CPU-time, wall-time, OOM-kill, PID-limit, and lethal log-cap
breaches; the parallel runner must compare those values with its own worker
specification.

**CPU placement.** The unit gets `AllowedCPUs=` with the CPUs that host PMU
drivers bind their counters to removed, derived at launch by
`ci-hub/validate/cpu_placement.py`, the same derivation `validate-run` uses (see
`ci-hub/validate/README.md`, "CPU placement"). On devbig014 a ptrace stop costs
far more kernel time on those CPUs: timed-progress-bar `--verify` costs 97.89 s
of CPU on CPU 0 against about 28 s elsewhere
(https://github.com/rrnewton/hermit/issues/3265). `hermit run --strict` pins
itself to one random core from its allowed set, so an unplaced run occasionally
draws such a CPU. `placement.cpus=` states `APPLIED` only after a probe unit shows
the user manager enforcing the list; otherwise it states `NOT_APPLIED` with the
reason, and the run proceeds on every CPU. `DEV_HERMIT_EXCLUDE_CPUS=none`
excludes nothing, `DEV_HERMIT_EXCLUDE_CPUS=<cpu list>` excludes exactly that list,
and a malformed value exits 2 before anything runs. Placement changes which CPUs
the run may use and nothing else: no bound, deadline, argument or environment
variable. A Hermit guest cannot see the placement, because Hermit shows it a
virtualized CPU view; read the unit's threads from the host instead.

Tests: `bin/safehermit-test.sh` and `bin/safehermit-resource-test.py`.

## Disk-bound scope

`safehermit` does not create a btrfs subvolume per run: it no longer calls
`scripts/bounded-run-space`, so its retained log directory is a plain directory
and `bound.disk` reports `NOT_APPLIED`. It never bounded guest writes, an
explicit Hermit `--log-file` outside that directory, or a Hermit process that
did not enter through `safehermit`.

`hermit-repeat` therefore keeps its own total-artefact cap (`--max-total-mb`,
default 4096), because under `--verify` Hermit's log goes to files that
`safehermit`'s 64 MiB **stderr** cap never sees.
