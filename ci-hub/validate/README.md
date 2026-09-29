# `validate/` — local validation run visibility

**Follow skill `hermit-validation-authority` for Hermit authority and Reverie's
`reverie-validation-authority` skill for Reverie rows.**

This object turns the records that `hermit/scripts/validate.rs` writes into a readable,
machine-wide history. It owns the linkage from local run records to retained
profiles; it does **not** own worktree status, the generic DAG runner, or the
profile format (the latter two belong to the pinned `safe-ci-dag-runner`).

The retained-history consumer is:

| Command | Reads | Answers |
| --- | --- | --- |
| `validate/aggregate.py` | Every validation run on this machine (parent JSONL ledger + raw per-run logs + `safe-ci-dag-runner` profiling CSVs). | "What has every worktree/agent/slot validated, and where is the profiling?" |

The front door exposes it as `ci-hub local-history`. Worktree status belongs to
the wrkslots infrastructure rather than a second report in ci-hub.

Running `ci-hub validate-run` again for an unchanged commit starts a fresh
measurement. This is intentional: repeated runs expose intermittent failures
and provide the independent observations used by the basic-sanity scorecard.
The newest run does not erase older receipts; `validate-status` and
`local-history` retain the run history.

`ci-hub validate-status` renders run timestamps as compact ages by default
(for example, `1h36m ago`) so stale results are visible at a glance. Use
`--times local` or `--times utc` when an absolute timestamp is needed. JSON
output retains the recorded RFC 3339 timestamps in every mode.

With no SHA, PR, or run number, the recent table includes completed ledger rows
from every branch, ordered by completion time; it is not restricted to
`origin/main`, and live in-progress runs may also appear. Use
`--commit-timeline` for the `origin/main`-only view. The
`COMMITS-SINCE-GREEN` line and its JSON object name the exact local history ref
and full tip SHA used for the first-parent depth. This query deliberately does
not fetch, and the named tip may differ from the checkout's `HEAD`.

The `BRANCH` column is provenance captured from the source checkout's symbolic
`HEAD` before admission creates its detached validation checkout. A detached
source or a historical record that predates this field displays `-`; the reader
does not guess from refs that happen to contain the commit. `MAIN` independently
reports the authoritative relationship to `origin/main` as `exact-sha`,
`tree:<sha>`, `no-match`, or `unverifiable`.

Use `ci-hub validate-status --log-paths` when following a running or completed
validation. It adds the producer-recorded `SLOT` and full `LOG-FILE` columns;
they stay out of the default table because the path is intentionally wide.

Each run admitted by `validate-lock` receives the next RUN number for that
repository on the current machine before its child starts. The current run
handle's admitted result carries it while the run is in progress, and the
completed ledger row carries the same number. `ci-hub validate-status --run-no
NUMBER` looks up that run on the current machine. RUN is an identity, not proof
that a completed receipt exists: a run that fails before writing a ledger row
can leave a gap.

If freshly fetched `origin/main` has moved beyond that exact Hermit SHA, use
`ci-hub validate-run --frozen-validate ...`. The ordinary path still refuses a
target that does not contain current main. The explicit frozen path forces a
real rerun (`--ignore-cache`), disables receipt-backed PR labeling, and writes
its result to `ignored/validate/frozen/<unit>.jsonl` rather than the canonical
ledger. Both the run handle and that result name the target and the current-main
SHA observed before launch, set `measured_against_superseded_tip=true`, and set
`qualifying_receipt=false`. It is repeatability evidence for that exact tree,
never a claim that the current tip passed.

Hermit validation services start with a two-CPU quota, 8 GiB memory limit,
disabled swap, and 4,096-task limit. These cover immutable-tool preparation and
parent/driver compilation before Hermit enters its separate delegated DAG
scope. The DAG scope and its test nodes retain their own resource budgets.
The service wall backstop is the admission wait plus the effective child
deadline plus 600 seconds for startup; its existing 600-second stop timeout
covers finalization separately. An enclosing caller must leave room for both
service phases and its own setup/status work. Hitting any backstop is a failed
run, not successful qualification.

## CPU placement: validation stays off PMU-bound CPUs

On devbig014 a ptrace-stop-heavy cell costs 7.6-7.9x the CPU on host CPU 0
(https://github.com/rrnewton/hermit/issues/3265): the AMD uncore PMUs are bound
there, and `amd_l3` is bound to the first CPU of each L3 complex. So a Hermit
validation, `tick-pressure-test`, and the hourly integration-tip validation run
on the online CPUs minus every CPU named by a
`/sys/bus/event_source/devices/<pmu>/cpumask`. The list is derived from the
host at every launch by `validate/cpu_placement.py`; nothing is hard-coded.
Run `python3 ci-hub/validate/cpu_placement.py` to see what this host would
exclude, or add `--json` to print the full record.

This changes where the work runs. It does not change any budget, timeout, cell
selection, or verdict.

- **Fail-safe.** If no PMU has a cpumask, nothing is excluded (source
  `sysfs-none`). If sysfs cannot be parsed, nothing is excluded
  (`sysfs-malformed`). If the result would leave fewer than half the CPUs,
  nothing is excluded (`sysfs-too-broad`). A mask covering more than a quarter
  of the online CPUs, such as `power_core`, belongs to a per-core PMU; it is
  skipped and listed.
- **Override.** `DEV_HERMIT_EXCLUDE_CPUS=none` excludes nothing, and
  `DEV_HERMIT_EXCLUDE_CPUS=0,16` excludes exactly that list. Set it in the
  environment of `ci-hub validate-run`, of `pressure_schedule.py launch`, or of
  the integration-tip tick, which forwards it to its launcher. A malformed list,
  or one that would exclude every CPU, refuses the launch.
- **Mechanism.** The service unit gets `AllowedCPUs=<list>`. The Hermit driver
  runs in its own dagrun scope in another slice, where that property no longer
  applies, so the launcher also sets `HERMIT_CI_ALLOWED_CPUS`,
  `HERMIT_CI_EXCLUDED_CPUS` and `HERMIT_CI_CPU_PLACEMENT_SOURCE`. Hermit's
  `safe_ci_scope` then applies the same list to that scope with
  `systemctl --user set-property --runtime <scope> AllowedCPUs=<list>`, and
  every step and container underneath inherits it. What carries the list to
  the cells is that scope's cgroup cpuset, not per-process affinity: the
  validate container runs with `podman run --cgroups=disabled --privileged`
  (hermit `ci/hermetic/run-in-pinned-root.sh`), so its processes stay in the
  placed scope's cgroup. Measured: a container started that way inside a scope
  placed with `AllowedCPUs=1-315` read `Cpus_allowed_list=1-315`.
  If the scope cannot be placed, the driver prints `CPU placement NOT APPLIED`
  with the reason, records it, and runs unchanged: placement is never a gate.
  `bin/safehermit` applies the same derivation to its own unit (see
  `bin/README.md`).
- **Evidence.** The run handle carries a `cpu_placement` object with the
  source, the excluded, allowed and online lists, the contributing and skipped
  PMUs, and a detail. The retained log's first lines include a
  `validate-run: cpu placement ...` line. Hermit's ledger row carries a
  `cpu_placement` extension with what the scope actually got (`applied`, the
  effective cpuset, and the process's `Cpus_allowed_list`), and its
  completed-run summary prints a `CPU placement: <status> (excluded CPUs:
  ...)` line that the verdict does not read. Reverie validations are not
  placed. Core collisions between concurrent cells and the other unplaced
  launch paths are tracked in https://github.com/rrnewton/hermit/issues/3347.
- **Older readers.** `run_registry.parse_current_record` refuses a field it
  does not know, and `cpu_placement` is new. A run handle written by this
  revision or later is therefore unreadable to a `run_registry.py` from an
  older revision: `ci-hub validate-status` lists it as unreadable, and an
  older `validate-run` treats it the way it treats any unreadable handle,
  which retains storage and can refuse admission. It is never read as a
  pass. The health tick and the integration-tip validation run through
  `operational-tool --run-current-tool`, so they read with the current
  parser. Anything else that reads the registry, including the shared
  parent checkout's `ci-hub`, must run from a tool root at or after this
  change, as the validate-run recipe already requires (launch from a
  detached dev-hermit worktree at `origin/main`).

## Per-run output that survives the fresh checkout

Disposable checkouts are created under `worktrees/validate/`. Private Cargo
homes are created under `ignored/validate/cargo-homes/`, outside the directory
whose children wrkslots treats as validation slots. A normal waiter removes
both at completion; `ci-hub validate-run --sweep-completed` retries cleanup for
terminal runs whose waiter was interrupted, including its recorded private
Cargo home. Before removing a checkout, the sweep copies any untracked `*.jsonl` receipt to
`ignored/validate/orphaned-receipts/<unit>/`.

Before creating any new disposable validation checkout, `ci-hub validate-run`
runs one bounded cleanup batch. It creates the new checkout only when the batch
leaves no deferred checkout, no non-running retained checkout, and no
bookkeeping error. A refusal names the first path and reason and directs the
caller to `ci-hub validate-run --sweep-completed`; running validation checkouts
remain eligible for normal concurrent admission and are never removed by this
gate. `--in-place` creates no validation checkout and therefore does not run the
entry cleanup.

`ci-hub validate-run` prints `E2E_RESULTS` and `RUNNER_LOGS` paths for Hermit
runs and records the same paths as `e2e_result_root` and
`safe_ci_dag_runner_log_dir` in `ignored/validate/runs/<unit>.json`. They live
under `ignored/validate/artifacts/<unit>/`, outside the fresh checkout that is
removed after validation.

`python3 ci-hub/series/series.py history --test <test-id> --parent <root>` is
the cell-level history reader. Its durable series rows retain whole-cell wall
time and guest runtime counters, but not CPU time. For a same-run comparison of
two backends, use:

```bash
ci-hub/ci-hub ledger backend-factor \
  --run-id <validation-run-id> \
  --baseline ptrace --comparison kvm --mode verify --json
```

That command requires one qualified validation-ledger row, verifies the
ledger-bound compact cell artifact, and then reads the schema-4 E2E results in
the same retained run directory. It reports CPU and elapsed-time factors for
the exact first-attempt PASS intersection whose compact verdict is
`compared-and-matched`, plus every excluded cell. Modes whose compact verdict
is `performs-no-comparison-by-design` are reported as excluded rather than used
as evidence of a match. CPU time is the process-group CPU measured by the test
harness; elapsed time also includes preparation and time descheduled by other
work on the host. The raw timing files are retained local evidence, not
digest-bound by the validation ledger; the command prints a query-time digest
and the digest of every source file so a quoted result can be checked against
the exact bytes it used.

After a canonical Hermit receipt is readable, the waiting `validate-run`
requests a combined scorecard projection and current-run observation transaction
in `hermit_test_ledger`. The source checkout's catalogue and scorecard are not
rewritten. The publisher queries the canonical row by run ID and verifies its
retained proof operands and complete raw-result census before derived
publication. Passing and failing validation rows use the same proof contract;
a selected run with zero cells still needs a real finalized producer and an
authenticated empty census. A pre-DAG refusal supplies neither.

The combined writer holds the ledger lock across its immutable snapshot,
catalogue reconciliation, current-run observations, and atomic derived-file
replacement. Publication also verifies the selected tool source and measured
checkout bindings. A publication refusal preserves the native validation
verdict, retained results, and checkout, and is reported as incomplete delivery.
Pending raw publication batches remain in the existing spool for the hourly
backlog sweep; a later user validation is not required to retry them. The
service finalizer and `validate-run --attach <unit>` use the same run-bound
publication path.

Current run handles retain wire schema 1 so already-deployed readers can
continue observing a run started by newer code. Their complete contract is
owned by `run_registry.py`, and the `producer` field names that file so readers
can distinguish them from historical schema-1 handles (including twelve older
manual benchmark rows that already carry `kind`). Every current row records the
existing `validate-lock` `kind`
(`validate`, `reverie-validate`, `frozen-validate`, or `bench`), so the state
`failed` used by the pressure-test path cannot silently acquire the meaning of
an ordinary validation state. Python readers validate through `read_record`;
the Rust validate-lock and validate-status consumers ask the same authority for
their typed projections. Other schema-1 handles remain readable only as
historical input so a process started before deployment can finish without being
rewritten into the current contract.

The lock authority keeps execution admission separate from canonical receipt
authority. `lock_admissible=true` means the holder, owner identity, anchor, and
cleanup state are valid for that holder's validation kind. `admissible=true`
additionally means the run may support a canonical current-main receipt. A
`frozen-validate` holder therefore has the first property and deliberately not
the second.

`E2E_RESULTS` contains the harness's per-cell JSONL and JUnit output, including
failure reasons. Compare its per-category cells with Hermit's existing
`ci/expected-e2e-plan.json`; that committed plan is the denominator that
distinguishes a complete run from a passing subset. `RUNNER_LOGS` contains the
incrementally flushed per-step logs and `journal.jsonl`, so a killed runner
still leaves the last observed step and test boundaries.

For schema-6 failed receipts, the ledger adapter validates the retained
`cell_results` artifact before classification and writes `failed_cells` as exact
`test/mode/backend` ids whose typed verdict is `compared-and-diverged`.
`flaky_failed_cells` is the subset present in the exact-tree generated
`flaky-cells.json`. A cell with `unavailable-with-reason` is not put in either
list, and its presence prevents an aggregate manifest failure from being called
known-flaky.

## Reverie centralized producer

`ci-hub validate-run --repo rrnewton/reverie --checkout <reverie> --agent
<agent> --target <exact-sha>` uses the same detached systemd service,
box-exclusive `validate-lock`, run registry, and ledger adapter as Hermit. Its
product command is always the exact full `./validate.sh --no-label-pr`, executed
as one `safe-ci-dag-runner` step. Cgroup containment is mandatory: the wrapper
passes neither `--allow-cgroup-failure` nor `--unsafe-no-cgroups`, and the node
itself proves both the safe-ci step cgroup and live validate-lock owner ancestry
before it runs the product driver.

Reverie events use the existing HistoryRow/JSONL wire format and shard to
`ledger/reverie/<host>/<month>.jsonl`. The direct schema-3 row emitted by
`validate.sh` remains diagnostic. The central wrapper emits the schema-5
`ci-hub-reverie-validate-run` row that carries exact repository, current
seven-gate v3, test-coverage, lock, and safe-ci evidence. The reader retains the
historical seven-gate v1 and six-gate v2 contracts for old exact-SHA receipts.
`validate-status` filters by
repository before resolving a SHA, so a same-text Hermit SHA cannot satisfy a
Reverie query.
