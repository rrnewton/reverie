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

## Cleanup quarantine: what a queued validation waits for

When a run's payload leaves processes behind, or its supervisor dies before
proving the payload gone, the slot keeps a cleanup record. `validate-lock
status` prints it. No new validation starts while any record exists, and that
includes records in this repository's other slots.

At normal exit, teardown sends SIGTERM, waits 5 s, and then sends SIGKILL to
two sets of processes: those that carry the run's payload token
(`CI_HUB_PAYLOAD_DOMAIN`), and those still in the payload's original process
group. A process that left the group and also dropped the token is not
reached. <https://github.com/rrnewton/dev-hermit/issues/428> tracks
cgroup-based teardown for that case.

An acquirer with `--wait N` (N > 0) classifies the record on every poll:

- **Waits, keeping its queue place.** Either the holder's supervisor is alive
  and will release, or the supervisor is dead, its census was complete, and
  the recorded residual processes are still running.
- **Discharges the record itself.** Payload absence is proven and the holder's
  supervisor is proven dead, which is the same proof `reclaim-dead` requires.
  The release record says `released_by=auto-reclaim:<agent>`. Since
  <https://github.com/rrnewton/dev-hermit/issues/419> this replaces the manual
  `reclaim-dead` step in that one case. `reclaim-dead` still works.
- **Refuses at once (exit 3).** This happens when the record cannot be
  verified, when the supervisor's liveness cannot be established (for example
  a holder on another host), or when a dead supervisor's census was incomplete
  or never ran. The message ends with "this quarantine cannot be recovered by
  waiting" and says why. Follow the printed recovery command
  (`census-orphaned-domain`, then `reclaim-dead`).

Two consequences are easy to miss:

- **A discharge is not an admission.** The discharge writes a release record,
  and that record starts the 90 s owner-watch yield. An attempt made with
  `--wait 0` or `--no-wait` can therefore discharge the record and still exit
  3 on the yield it just started. An attempt made after the yield ends is
  admitted. A waiting `run` drops its slot anchors while it yields, so the
  owner watch can start.
- **A residual the census froze cannot exit.** The census stops (SIGSTOP) the
  residual processes it records, and a supervisor that dies after doing so
  leaves them stopped (state `T`). The waiter's message names each stopped
  process as `pid@start-ticks`. An operator must SIGKILL exactly those
  processes, or an outer bound such as the service's `RuntimeMaxSec` must end
  them. The next attempt then discharges the record. A waiter never signals
  processes that its own run did not start.

`run` spends one `--wait` budget, measured from its start, across the
owner-watch yield, the global queue, quarantine waits in any slot, and local
acquisition. It does not restart the clock for each phase. Each phase can
overrun by one 3 s poll. After the child exits, teardown adds at most 16 s:
5 s of SIGTERM grace, 3 kill rounds of 2 s each, and a 5 s check that the
payload domain is empty. The Hermit service's `RuntimeMaxSec` is `--wait` plus
the child deadline plus 600 s, which covers all of these.

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
  does not know, and `cpu_placement` is new, as is `git_substitution` (next
  section). A run handle carrying either is therefore unreadable to a
  `run_registry.py` from a revision before that field: `ci-hub validate-status`
  lists it as unreadable, and an older `validate-run` treats it the way it
  treats any unreadable handle, which retains storage and can refuse
  admission. It is never read as a pass. The health tick and the integration-tip validation run through
  `operational-tool --run-current-tool`, so they read with the current
  parser. Anything else that reads the registry, including the shared
  parent checkout's `ci-hub`, must run from a tool root at or after this
  change, as the validate-run recipe already requires (launch from a
  detached dev-hermit worktree at `origin/main`).

## git: validation runs the unwrapped binary

On devbig014, `git` on PATH is `/usr/local/bin/git`, a fleet telemetry
wrapper. It execs the git that `/etc/scm-telemetry.toml` names
(`/usr/local/bin/git.meta.real`), and on every call it also starts three more
`git.meta.real` commands and a detached `scm_telem_log`. Inside a systemd-run
cgroup at a load average of about 200, one call cost 164 ms of CPU through the
wrapper and 19 ms through `git.meta.real` directly. Hermit's `gate.manifest`
makes about 4011 git calls (https://github.com/rrnewton/hermit/pull/3311), and
RUN 1961 stopped at 901.5 of its 900 CPU-s
(https://github.com/rrnewton/hermit/issues/3381).

So, when every check below passes, `ci-hub validate-run` (for
`rrnewton/hermit`) and `pressure_schedule.py launch` run git as
`git.meta.real`. They put a run-private directory first on PATH; its only
entry is `git`, a symlink to `git.meta.real`. `validate/unwrapped_git.py`
decides this at every launch. Run `python3 ci-hub/validate/unwrapped_git.py`
to see what this host would decide: it prints the decision with
`planned_directory` and `created: false` and creates nothing.

This changes which git binary runs. It does not change any budget, timeout,
multiplier, audit, cell selection, or verdict.

It is a mitigation, not a fix for https://github.com/rrnewton/hermit/issues/3381,
which stays open. One paired measurement (**n=1**) at Hermit `98a621f4`, load
average 191-213, ran `gate.manifest`'s command both ways at the same time,
each in its own cgroup scope:

| git | CPU-s (`cpu.stat` `usage_usec`) | Wall (s) |
|---|---|---|
| Through the wrapper | 2292.6 | 2262.5 |
| `git.meta.real` directly | 1559.7 | 1827.5 |

That is 32 % less CPU. `usage_usec` is the counter dagrun compares with a
step's `cpu_timeout` when the run is boxed (agent-utils
`rs/dagrun/src/scheduler.rs`, `cpu_seconds_from_stats`). An unboxed run
instead samples the step's process group from procfs, which can miss
processes that leave that group (`uncontained_cpu_budget_warning`). Both numbers exceed the 900 CPU-s that step had
at `98a621f4`. That budget no longer applies: Hermit `d7138e26` moved the
tool self-tests into their own leaf nodes, and at Hermit `84da7b81`
`gate.manifest` has a 900 s wall timeout and a 120 CPU-s `cpu_timeout`.

The first local validate of `main` after this lands must record four things
from `gate.manifest`, and post them on
https://github.com/rrnewton/hermit/issues/3381:

- CPU-seconds, read from the cgroup counter;
- wall time;
- whether all six audits finished;
- how many cores were busy.

Making fewer git calls is https://github.com/rrnewton/hermit/issues/3382.

- **Who gets the new PATH.** For a validation, only validate-lock's child
  gets it, which is the validation driver and its DAG steps. The launcher
  runs that child as `/usr/bin/env PATH=<new> DEV_HERMIT_NETWORK_GIT=<wrapper> ...`.
  The unit's own `--setenv PATH` stays the inherited PATH, so validate-lock
  and the `ExecStopPost` retire path, including the ledger mirror's push and
  fetch, keep the wrapper. The pressure test sets PATH for its whole unit
  (`--setenv=PATH`), so `run_campaign`, its local git calls and its
  validate-lock all see that PATH.
- **Checks.** The substitution applies only when all of these hold:
  - PATH's `git` is the same file (`os.path.samefile`) as the `git` beside
    `git.meta.real`. A same-named copy does not count.
  - `git.meta.real` is an executable regular file.
  - The wrapper's configuration names `git.meta.real`. The wrapper reads
    `$SCM_TELEMETRY_CONFIG` when that variable is set, and
    `/etc/scm-telemetry.toml` otherwise. The file is parsed as TOML, and
    `binaries.git` must be an absolute path to the same file as
    `git.meta.real`. A missing file or an empty variable makes the wrapper run
    `/usr/bin/git.real` (git 2.52.0) instead, so either one refuses the
    substitution.
  - Both binaries print the same `git --version` line.

  Otherwise PATH is unchanged, with a reason code:
  - `not-the-wrapper`, `already-unwrapped`, `real-git-unavailable`,
    `no-git-on-path`, `relative-path-entry`, `unusable-directory`;
  - `wrapper-config-unreadable`, `wrapper-config-mismatch`;
  - `version-mismatch`, `version-probe-failed`, `materialize-failed`.

  The version probes write to temporary files, not pipes, because the
  wrapper's detached logger keeps inherited descriptors open. A probe that
  cannot create those files, start, or be waited for gives
  `version-probe-failed`, never an exception.
- **What the unwrapped run gives up.** Measured with strace on devbig014
  (`fb-git-wrapper-20260928-001642`):
  - Telemetry: the detached `scm_telem_log` (found on PATH), which itself
    runs `git.meta.real remote get-url origin`, and the three extra
    `git.meta.real` calls (`rev-parse --show-toplevel`, `remote -v`,
    `version`).
  - Two HTTP headers, passed as `-c http.extraHeader=x-fb-git-wrapper: ...`
    and `-c http.extraHeader=X-Client-Info: ...`. git hands these to its own
    children as `GIT_CONFIG_PARAMETERS`.
  - A trace2 file: `GIT_TRACE2` names a new 0600 file in `$TMPDIR`, with
    `GIT_TRACE2_BRIEF=1`, and the wrapper deletes the file afterwards.
  - The HTTP 429 retry (the binary's strings include `GIT_NO_HTTP_429_RETRY`).

  Nothing else changes. The same binary runs with the same arguments, minus
  those two `-c` options. Its environment is also the same, minus
  `GIT_TRACE2`, `GIT_TRACE2_BRIEF` and those `GIT_CONFIG_PARAMETERS` entries.
- **Network git calls.** Only network calls can meet an HTTP 429. Here is
  each one and which git it runs:
  - The pressure campaign's `git submodule update --init --recursive` keeps
    the wrapper. `launch` passes PATH's original git to the unit as
    `DEV_HERMIT_NETWORK_GIT`, and `run_campaign` runs this call with it.
  - The ledger mirror's push and fetch (`mirror.py`, run from
    `ExecStopPost`) keep the wrapper, because the unit's PATH is unchanged.
  - `pre.reverie_pin` (`scripts/check-reverie-pin.rs`: `git ls-remote` and
    `git fetch` of Reverie's `main`) runs `git.meta.real` and has no 429
    retry until https://github.com/rrnewton/hermit/issues/3390 lands.
  - Cargo's git fetches of Reverie, when the seeded git database lacks a
    revision, also run `git.meta.real`. `validate-run` gives the unit a
    private `CARGO_HOME` with a copy of `~/.cargo/config.toml`, and that file
    sets `net.git-fetch-with-cli = true` on devbig014, so cargo runs PATH's
    `git`. Nothing here adds a retry for these fetches.

  For scale, the retained logs of the 14 days before 2026-09-29 show no
  visible 429: 0 of 491 validate logs and 0 of 11,333 node logs.
- **Where the directory lives.**
  - For a validation: `<runtime root>/unwrapped-git/`. The runtime root is
    the run's private `mkdtemp` directory, which is also the unit's `TMPDIR`
    and is recorded as the handle's `runtime_root`. Every path that removes
    the runtime root removes this directory with it: a refused launch (for
    example because the log already exists), normal completion, and orphan
    recovery. A launcher killed by SIGKILL leaves it until orphan recovery. Nothing is created under
    `ignored/validate/artifacts/<unit>/` before systemd accepts the unit.
  - For a pressure test: `ignored/ci-hub/pressure-test-git/<slot>/`. Several
    exits remove it:
    - `launch`, when the run record cannot be written, systemd-run fails to
      start, or systemd refuses the unit;
    - `run_campaign`, when the run record has no checkout, when the checkout
      already exists, or when slot creation fails;
    - `run_campaign`'s `finally` block, after the worktree.

    A campaign that dies without running Python cleanup, for example
    SIGKILL at `RuntimeMaxSec`, leaves its directory. The next `launch`
    removes it: holding the launch lock, and once `systemctl is-active` shows
    the unit inactive, it sweeps every entry under `pressure-test-git/`.
    Each sweep with something to report writes a
    `pressure_schedule: swept ... removed=[...] left=[...]` line to the
    pressure log.

  Every removal deletes only the exact shape: a directory whose only entry is
  a `git` symlink to `git.meta.real`, which is the recorded one for
  per-run removal and the host's `REAL_GIT` for the sweep. Anything else is
  left in place and reported as `cleanup_error`:
  - in the pressure state and run record, which marks a campaign failed;
  - in the exception message for a `run_campaign` refusal;
  - in the launch summary for the sweep, which also makes `launch` exit 1,
    so the health tick reports it.

  A directory is created only if it does not exist yet, and is never reused.
  If creation or its check fails, what the launcher created is removed and
  PATH is unchanged (`materialize-failed`). If a step deletes the directory
  mid-run, PATH lookup falls through to the next `git`, which is the
  wrapper. `validate-run --dry-run` prints the planned child PATH without
  creating anything.
- **Evidence.**
  - Both run handles (the validation handle and the pressure test's bench
    handle) carry a `git_substitution` object with these fields: `applied`,
    `code`, `reason`, `resolved_git`, `resolved_git_realpath`, `real_git`,
    `effective_git`, `effective_version`, `wrapper_config`,
    `wrapper_config_git` and `directory`.
  - `run_registry` checks the object's structure. `applied` is true exactly
    when `code` is `applied`. An applied record names its absolute
    directory, runs `real_git`, and names the wrapper configuration and the
    absolute git that configuration names. Otherwise the directory is null
    and PATH's git runs.
  - A validation writes `git_substitution` to its handle, and appends a
    `validate-run: git substitution APPLIED|NOT-APPLIED: {...}` line to the
    retained log, only after systemd has accepted the unit. A refused
    launch's handle therefore has no `git_substitution`.
  - A `--dry-run --json` handle reports the plan instead, with
    `planned_directory` and `created: false`, which `run_registry` does not
    accept as a record.
  - The pressure log gets `pressure_schedule: slot=<slot> git substitution ...`.
  - `git_substitution` is a new handle field, so the **Older readers** note
    above applies to it exactly as it does to `cpu_placement`.
- **Not covered.** Reverie validations, `bin/safehermit`, and every other
  launch path keep the inherited PATH.

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
