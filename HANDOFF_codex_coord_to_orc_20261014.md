# Codex coordinator handoff to ORC

> Written on 2026-08-14 PDT. The filename contains `20261014` because that is
> the exact name requested by the owner.

## Read this first

The requested Basic Sanity Milestone 1 was **not reached**. Hermit `main` is
clean, validated, and materially easier to inspect than at the beginning of the
session, but the decisive repeated-green check was not completed.

The persistent Codex goal paused at 2026-08-13 18:05:09 PDT when an owner
message deliberately interrupted an in-progress turn. The event record shows
`thread_goal_updated`, followed by `turn_aborted`, followed by the new owner
message. Interactive work continued for a while, but the persistent goal was
never resumed. When owner messages stopped, unattended work stopped too. This
was not caused by validation, a machine outage, or a product blocker.

Do not infer completion from task status, a local branch, or a pushed commit.
The facts below distinguish landed, pushed but unlanded, and local-only work.

## Owner mission carried by this session

The owner asked one Codex coordinator to simplify the infrastructure and drive
Hermit directly, with native subagents allowed only as controlled helpers. The
product outcome is a Hermit checkout whose current compatibility state can be
understood and reproduced without parent-workspace bookkeeping.

The milestones as understood here are:

1. **Basic Sanity Milestone 1** uses the current bare `hermit run --verify`
   contract. Bare `--verify` uses the legacy Stripped comparator. A fixed green
   envelope must pass five complete validates and every currently green cell
   must then pass 20 independently boxed repetitions. A flake blocks the
   milestone; the cell is not silently moved to red.
2. **Milestone 2** removes the weak behavior and makes strict INFO-log
   verification the default. This is intentionally a compatibility-standard
   change, so it must be developed on a feature branch and shown to the owner
   before landing, with the measured drop from the M1 envelope stated plainly.
3. **Milestone 3** makes unsupported syscall handling fail closed by default.
   It requires a fresh syscall audit and a measured envelope change. Do not
   conflate unknown, unsupported, implemented, and passthrough syscalls.

The owner also asked that validate and the periodic full-matrix pressure test
be usable from the inner Hermit repository, with a committed scorecard at each
Hermit commit. Parent-level scorecard machinery is not intended to remain a
second source of truth.

## Exact repository state at handoff

### Hermit

- Checkout: `/home/newton/work/dev-hermit/hermit`
- Branch: `main`
- HEAD and `origin/main`:
  `e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad`
- Status: clean and neither ahead nor behind.
- Latest subject: `Make manifest plans authoritative for pressure budgets`.
- Exact validation authority reports this SHA as `VALIDATED`:
  full/full profile, clean commit-anchored tree, 59/59 DAG nodes, 1,063 tests,
  363 filtered tests, 19/19 test nodes, zero failures, 926 seconds wall time on
  `devbig014`. The retained pass timestamp is 2026-08-14T04:31:33Z.

### dev-hermit parent

- Checkout: `/home/newton/work/dev-hermit`
- Branch: `main`
- Local HEAD: `39a83e0d169a6422be2b848e2c1ef08f8f483855`
- Local main is seven commits behind `origin/main`; all seven observed commits
  are ledger append commits.
- The only reported working-tree change is the dirty/detached `agent-utils`
  submodule described below.
- Do not use raw `git pull` in the parent. Its current registered update path is
  `./scripts/parent-main-write sync`, but it must not run until the
  `agent-utils` work has been preserved because sync restores the recorded
  gitlink checkout.

### agent-utils

- Checkout: `/home/newton/work/dev-hermit/agent-utils`
- State: detached at
  `c810ddb7f8c6992827d6d145e5dff9e1d2e49b96`.
- `origin/main` observed during the work was
  `b8b3274a84c08b355ff9a24acc0bb9421e3a0ba4`.
- Local-only, uncommitted paths:
  - `skills/README.md`
  - `py/wrkslots.py`
  - `py/tests/test_wrkslots.py`
  - `skills/wrkslots/SKILL.md`
  - `skills/wrkslots/agents/openai.yaml`
  - `HANDOFF_wrkslots_unfinished_20260814.md`
- This prototype is **not safe to land as written**. See the dedicated handoff
  at
  [agent-utils/HANDOFF_wrkslots_unfinished_20260814.md](agent-utils/HANDOFF_wrkslots_unfinished_20260814.md)
  and the worktree section below.
- Two attempts to create a policy-compliant recovery commit stopped because
  `./ci-hub/bin/who-am-i --tag --role impl` returned `owner unresolved`.
  Nothing was committed or pushed. Do not bypass the disclosure check or
  reconstruct its output manually.

## Current Hermit compatibility scorecard

The committed scorecard is [SCORECARD.md](hermit/SCORECARD.md), checked by
`./ci/compat-envelope/scorecard.rs check`. The stable identities are in
`hermit/ci/compat-envelope/cells.json`; raw run data remains ignored.

The current denominator is **5,376 comparable cells**, not 7,056. For `N`
manifest tests it is:

```text
N × (5 Hermit backends × 3 common modes + 1 native naked control)
```

On 2026-08-13, `N = 336`, so this is `336 × 16 = 5,376`. `custom` commands are
not multiplied across every test/backend pair and are excluded from this
uniform denominator. Do not spread the transient number 336 through generic
documentation; the formula is the stable statement.

Current M1 score:

| Mode | Green | Total | Meaning |
| --- | ---: | ---: | --- |
| verify | 167 | 1,680 | Legacy Stripped same-backend repeatability |
| replay | 1 | 1,680 | One selected ptrace replay cell |
| chaos | 0 | 1,680 | No chaos cells counted green |
| naked | 0 | 336 | Native controls are currently red |
| **Total** | **168** | **5,376** | M1 metric, not strict determinism |

Backend totals are ptrace 148/1,008, DBT 9/1,008, KVM 0/1,008, SaBRe 9/1,008,
LiteInst 2/1,008, and native 0/336.

Ordinary full validation executes 172 selected cells: the 168 compatibility
cells above, two chaos race-exposure checks, and two explicit custom commands
outside the denominator. A passing validate must freshly execute all 172.

Important limitations:

- These verify cells compare each backend with itself. They do **not** measure
  cross-backend parity against ptrace.
- The table therefore reports no cross-backend parity number.
- The replay count of one is suspicious relative to historical claims. A
  targeted history and live-command audit was requested but not completed.
- The red-cell observation store currently contains no complete pressure-run
  observations. It has the schema for results and first-divergence fields, but
  M1 did not populate it with a complete run.

## What landed on Hermit main

The following session work is on current Hermit `main`:

- `d266ceabfe9f7628d7aacaca823c8814ac6e64e8` — preserve selected owner prompts
  and derive `docs/infrastructure/owner-prompts/{README,INTENT}.md`.
- `16d47dfebcffdf73ff800a5cd8a71ef84b7d4a5b` — consolidate Reverie pin policy
  and viability checks salvaged from the backlog.
- `7bc09085c6e04b2be42e4d9326114cb6aad9cde9` — require a plain-language summary
  and project impact for inner-repository changes.
- `1f4e430c135204591daa0c0cba84f77dae679a78` — make Hermit validate its own
  compatibility scorecard.
- `be4c090500e724d61380636e211322313295d51c` — count and inspect the complete
  comparable matrix.
- `77c9f416a1da940b1b6e0e7edc79d54436ff40c6` — show compatibility by mode and
  provide the pressure-test verify path.
- `fce239564dbb3678647de518c302d5543f30d880` — give cold strict compatibility
  builds their declared width.
- `5cb633686c0e35559f9f2cb76a2d619c44dd7652` — make pressure measurement
  inspectable and bounded.
- `b7d4e1a95f4ddef87be4d5d614421a343cc9fae5` — retry a measured harness
  permission denial.
- `87bb8c14d8f133609511e657ea0f78e6b203a69c` — make pressure-test timeouts
  truthful.
- `2dd34a544857b04f247589817438d5083aa3e733` — separate repeatability claims
  from parity claims.
- `832935a77340e8c845b2256f5a5684eaffb277df` — build pressure plans with the
  shared typed safe-ci DAG model.
- `d44173a45b96e6f73f3a3807de47502c91ce7ebc` — keep standalone Rust tools fresh
  through Cargo.
- `f9b960072abd037b65aa9dfe34972718cf31e73e` — make pressure budgets match the
  attempts actually executed.
- `2332f726f54b1d8bd6b06c3397fa24d0058e4d8b` — refuse invented seedless chaos
  attempts and stabilize the `findmnt` fixture.
- `e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad` — derive pressure budgets from the
  manifest plan rather than a second parser.

Earlier documentation commits also corrected overclaims that bare `--verify`
was strict or bitwise. Preserve that distinction.

The current main scorecard and pressure path are useful, but not the final
architecture. In particular, the pressure path still serializes DAG JSON and
shells through `ci/run-dag.sh`, while validate invokes the Rust scheduler
library directly. The manifest parser, command renderer, and shell harness also
duplicate responsibilities.

## Validation evidence already earned

Five successive exact Hermit SHAs received clean full-validation passes:

- `832935a77340e8c845b2256f5a5684eaffb277df`
- `d44173a45b96e6f73f3a3807de47502c91ce7ebc`
- `f9b960072abd037b65aa9dfe34972718cf31e73e`
- `2332f726f54b1d8bd6b06c3397fa24d0058e4d8b`
- `e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad`

An earlier qualifying run at
`2dd34a544857b04f247589817438d5083aa3e733` also passed. These runs satisfy the
five-whole-validate stability part of M1. They do not substitute for 20
repetitions of each green cell because the commits differ.

Run the latest status with:

```console
cd /home/newton/work/dev-hermit
./ci-hub/ci-hub validate-status e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad
```

The owner has stated that local validate is the complete green/landability
authority while GitHub CI is disabled. Do not turn a hosted producer back into
a prerequisite.

## Basic Sanity Milestone 1: exact remaining work

The missing acceptance check is:

```text
168 current green cells × 20 independently boxed repetitions = 3,360 results
```

Every expected result must be present. The acceptable total is 3,360 passes,
zero ordinary failures, zero missing rows, zero inner or outer timeouts, zero
OOM kills, and zero infrastructure failures. A flake is a failure of M1; it is
not permission to edit the green set.

No stable command on current `main` performs that whole acceptance check yet.
Do not claim M1 from the five validates alone.

### Pushed but unlanded repetition implementations

Two recovery branches are now durable on origin:

1. `origin/codex/basic-sanity-green-repetitions`
   - HEAD `cb290c7384c5821e04f43822b8347162743a4e1f`
   - parent `b3af20d0fc7cd37035faeab66bdb0f371f0190ee`
   - two commits ahead of current `main`
2. `origin/codex/basic-sanity-green-repetitions-handoff`
   - HEAD `efa82557f16e8a0d554569c6372d56fd89356a96`
   - an independent earlier one-commit snapshot

Neither is landed. Do **not** land either as-is.

The main recovery branch is also preserved as draft Hermit PR
[#2256](https://github.com/rrnewton/hermit/pull/2256). Its description states
that the full run was not performed and records the known blockers. The draft
is a handoff artifact, not landing authority.

The main branch expands the selected 168 green cells to 20 leaves each while
sharing build and preparation nodes. Its intended plan is 3,514 nodes: six
shared build nodes, 147 preparation nodes, 3,360 cell nodes, and one summary.
It preserves fixed scheduler widths such as four manifest guests and one KVM
guest. No complete guest run was performed.

Adversarial review found material defects:

- A global `.pressure-test-generated-checkout` exemption could make an
  ordinary source checkout containing that file appear clean.
- The commit claimed a rustfmt check that actually failed.
- Subcommand help was not honest or discoverable.
- Standalone summary counted expected rows as attempted rows, so missing work
  could appear attempted.
- The implementation duplicated result identity/classification and contained
  a large second reclassification pass.
- It retained manual DAG generation, JSON serialization/reparse auditing,
  `ci/run-dag.sh`, runner-profile CSV parsing, shell harness execution, and
  resume markers. This is the architecture the owner explicitly rejected.

Useful parts to salvage are selection validation, stable repetition numbers,
sampling arithmetic, `passes/N` accounting, refusal of `0/0`, and the rule that
only an exact single-cell repeated result may be described as flaky.

### Recommended clean implementation

The completed read-only architecture audit recommends making
`ci/manifest-plan` a Rust library plus thin CLI. The library should own typed
manifest parsing, test/cell identities, selection, preparation, command
construction, execution, and schema-3 results. Both validate and pressure-test
should use the same builders and invoke the safe-ci Rust scheduler library in
memory.

The two entry points should differ only in selection, repetition count, and
expected outcome:

- validate selects the committed green regression cells and requires pass;
- pressure-test selects red probes or repeated green cells and retains each
  trustworthy result.

Do not serialize a DAG merely to shell through `ci/run-dag.sh`. Do not parse the
manifest separately in `manifest-to-commands.rs`, the shell harness, and the
pressure tool.

Acceptance tests should exercise the real CLI front doors against a tiny
fixture repository and trivial programs. Required cases are pass, output
mismatch, nonzero exit, bounded timeout, repeated results with one preparation,
preparation failure/missing row, and OOM when delegated cgroups are available.
After those fast tests pass, use the real
`backend-parity-c/host-identity` cell for 20 repetitions with a four-job pool.
Only then launch the 168×20 run asynchronously.

The safe-ci scheduler already observes timeout and OOM details internally but
does not expose all of them in its typed `StepOutcome`; extending that structure
is smaller and safer than reparsing profile CSV.

The generic safe-ci `--stress N` is not currently sound for M1. It duplicates
setup nodes, lets copies collide on result paths, drops named resource limits,
overstates memory as if every copy runs simultaneously, and can stop launching
after the first failure. Do not substitute it without fixing and testing those
properties.

## Pressure-test behavior on current main

Current main supports exact-cell and bounded-sample investigation. Examples:

```console
cd /home/newton/work/dev-hermit/hermit

# Inspect/check the committed table.
./ci/compat-envelope/scorecard.rs check

# One exact red cell with a tight timeout.
./ci/compat-envelope/pressure-test.rs run \
  --test applications/example-timed-progress-bar \
  --mode verify --backend ptrace --cell-timeout 60

# Reproducible ten-cell sample; custom is excluded.
./ci/compat-envelope/pressure-test.rs run \
  --sample 10 --seed 42 --cell-timeout 60

# Verify-only sample, the first expansion target.
./ci/compat-envelope/pressure-test.rs run \
  --mode verify --sample 10 --seed 42 --cell-timeout 60
```

The current result vocabulary distinguishes determinism/replay/parity failure,
crash/error, timeout, OOM, and infrastructure failure. A malformed or missing
artifact is not accepted as a test failure without exact runner evidence. Red
measurements may record the earliest/latest first-divergence scheduler turn and
virtual nanosecond for a fixed Detcore tree. A failure with no measurable INFO
log keeps those fields null.

This vocabulary is for observed execution outcomes. It is not the canceled
agent-invented hierarchy of compatibility “tiers.”

## Determinism, parity, and retained logs

Do not conflate these:

- Same-backend determinism asks whether repeated runs of one backend agree.
- Cross-backend parity asks whether a backend agrees with a fresh ptrace golden
  run.

For parity, the ptrace log generation is a real DAG dependency. KVM/DBT/SaBRe/
LiteInst parity cannot honestly run before the corresponding ptrace result
exists. Caching golden logs across commits was discussed but deliberately not
implemented because it adds invalidation complexity.

The owner requested:

- `hermit run --verify --keep-logs` for human usability;
- rename `--verify-logs` to the clearer `--print-verify-logs`;
- allow `hermit log-diff` to normalize one log;
- make `log-diff` emit a machine-readable JSON comparison containing the first
  divergent scheduler turn, virtual nanosecond, and other structured facts;
- retain normalized ptrace golden logs for later parity work.

Do not assume all of these are landed. Verify current CLI behavior before
claiming them. The pressure documentation says verify-mode attempts retain raw
logs and a normalized ptrace first-run stream; replay raw-log retention is
explicitly not implemented.

## Subagents and worktree cleanup

No native Codex subagent is currently running. The final implementation and
review children are completed.

Most helper agents were read-only audits and had no product slot. They covered
pressure CLI usability, matrix history, replay/chaos history, syscall status,
parity graph structure, log retention, e9patch status, historical parity
claims, M1 population, walkthrough quality, repetition architecture, and the
safe-ci stress design. Their conclusions are represented in this handoff; they
left no known writable checkout.

### `codex-m1-stress`

- Purpose: implement the 168×20 repeated-green acceptance run.
- Former path: `worktrees/codex-m1-stress/hermit`.
- Branch: `codex/basic-sanity-green-repetitions`.
- The two local commits were pushed to origin before cleanup.
- The registered releaser verified the outer and initialized nested repository
  HEADs on origin, revoked the exact recorded sentinel, removed the Hermit
  worktree, removed the slot directory, and removed the registry row.
- `worktrees/codex-m1-stress` is now absent and the slot is not in
  `worktree-state.json` or `worktrees/ACTIVE.md`.

### `codex-overhaul-ultra`

- Path: `worktrees/codex-overhaul-ultra`.
- Branch: `codex/single-threaded-infrastructure-overhaul-final`.
- HEAD: `41be1f01ff5f9d6cc8d5138b1429abef33d3d769`, clean and reachable from
  origin/current main.
- Registry status: `release-requested` with a complete remote-durability
  handoff; recorded owner process is absent and its systemd unit is inactive.
- Cleanup remains blocked because the registered stale-clear command refuses a
  missing old sentinel as an InvocationID mismatch: expected
  `7eb62dfd19394d9caabf26eb97c9e5ee`, observed empty.
- Do not manually delete it. The releaser needs a correct missing-unit recovery
  path or an authorized lifecycle repair.

### `codex-overhaul`

- Path: `worktrees/codex-overhaul`.
- Branch: `repair/pr-2070-current-main`.
- HEAD: `ae1969ea7537765cc86b34a043f07e6f24ac2ad0`, clean and remotely durable
  through origin branches.
- Registry status: `release-requested`; recorded owner process and unit are
  absent.
- Its exact registered continuation refused because the environment denied the
  `git merge-base --is-ancestor` proof with `Operation not permitted`.
- Do not manually delete it; retry the registered continuation in an execution
  context where the proof command is allowed.

### Registry-wide warning

After the successful `codex-m1-stress` cleanup, the advisory registry verifier
reported 51 drift rows / 83 product cells among 120 registered rows. This is a
pre-existing broader problem, not permission to run allocator `--repair` or
delete anything. Every row requires independent inspection. Do not sweep these
changes into an unrelated parent commit.

## Unfinished wrkslots prototype

The wrkslots work was performed directly in the canonical `agent-utils`
checkout, not a product slot. Its goal was to make `worktrees/` an opaque
directory managed only through a `wrkslots` CLI, enforce one active agent per
slot, remove parking/recycling, keep machine-sharded ACTIVE/ARCHIVED records,
and refuse deletion without cleanliness, durability, and process-use proofs.

Adversarial review stopped publication. The most important defect is that
`finish` authenticates the still-live owner and then physically deletes the
slot, contradicting the current rule that an owner cannot prove its own
absence. Other blocking defects include replayable caller-supplied PID
“authentication,” dead-owner adoption that overwrites historical ownership,
non-atomic initialization, Git replacement/environment attacks against
ancestry proof, mutation without global shard uniqueness checks, incomplete
archive evidence, and unresolved PID reuse/container/cgroup races.

The prototype remains uncommitted and unpublished. Do not land it. Preserve it
until the dedicated handoff is read and either repair it in an isolated
checkout or intentionally discard it after the owner confirms no value remains.

## TaskGraph and ORC state that should not be inherited as truth

TaskGraph notes are one agent's claims, not authority. The following items from
the ORC graph or overnight relay are contradictory, canceled, superseded, or
require fresh evidence. Delete them when they only describe canceled work;
otherwise rewrite them as historical notes with no blocking effect.

### Canceled compatibility tiers and reason classification

The owner rejected the agent-invented compatibility-tier system and reason
classifier. The owner rule is: run the cell; deterministic/parity is green;
anything else is red; a cell that cannot run is red. Observed operational
outcomes such as timeout and OOM remain useful, but they are not an ordered
compatibility tier.

Remove or disable graph work that treats any of these as current policy:

- `canonical-bitwise`
- `exit-and-stream-equality`
- `execution-only-self-consistent`
- `declared-but-unverifiable`
- retired strings such as `unqualified-no-comparison`
- `ci_disabled_reason` as an owner-requested compatibility classifier

The sealed candidates `4ed0eb4e`, `ecf06f8d`, and `e5e760ae` were explicitly
rejected for stale tier vocabulary. `7772a77b` was explicitly rejected because
reviewers could self-assert different model families and launder two Codex
reviews as Codex+Claude. Do not resurrect any of them.

`41be1f01ff5f9d6cc8d5138b1429abef33d3d769` did land before cancellation and is
an ancestor of current main. It added a `ci_disabled_reason`/tier ratchet. The
owner told the coordinator not to revert it unilaterally, but also rejected the
design. This is unresolved product debt: inspect its live behavior and ask the
owner whether to remove or simplify it. Do not cite its existence as approval.

### Reviewer-family bookkeeping

The “family receipt” wording was rejected as invented terminology. The useful
owner intent is simply that a coordinator-launched Codex review and a
coordinator-launched Claude review receive their ordinary labels and that the
workflow make honest behavior easy. There is no mechanical security boundary
between agents on this host, so do not claim adversarial identity protection.

Delete or demote graph work that attempts to prove model identity through
self-asserted comments, complicated process provenance, or another receipt
layer. The formal GitHub `REQUEST_CHANGES` limitation on owner-authored PRs is
not a blocker because the project uses its own blocking-comment protocol.

### Validation producer registration

The owner explicitly asked to temporarily disable the exact-Git-blob freeze on
validate infrastructure while this single coordinator repairs validate. Graph
items that block a locally passing exact-SHA validate only because a producer
hash map has not been rotated are outdated for this owner-authorized overhaul.
Do not reintroduce the registration/landing cycle as a prerequisite to M1.

The earlier `625909c619` cycle had no bootstrap path: producer registration
required a prior landing while landing required registered producer evidence.
That was escalated as an owner decision. The later direct-main repair directive
is the relevant exception for this work.

### Local versus hosted validation

The owner stated that local validate is the complete authority while GitHub CI
is off. Remove hosted-only validation dependencies from the active path. Keep
GitHub history as diagnostics, not a producer that must exist before local
evidence counts.

### Parent versus inner scorecard

The owner wants the scorecard versioned in Hermit so an agent can check out a
Hermit commit, validate it, choose a red cell, improve it, and commit the new
score. Graph tasks that make the dev-hermit parent CSV/full-matrix pipeline
canonical are obsolete. Port useful behavior, then delete the redundant parent
implementation rather than maintaining two pipelines.

### Stale denominators and milestone definitions

- `7,056` is obsolete because it multiplied `custom` across all tests.
- `5,376` is the current comparable denominator for 336 tests.
- Counts must always name the metric version: M1 legacy Stripped verify, M2
  strict INFO-log verify, or M3 strict verify plus fail-closed unsupported
  syscalls.
- Do not pool passes from different commit SHAs as 20 repetitions of one cell.
- Do not call M1 complete from five whole validates alone.
- Do not treat the historical replay/chaos green claims as current evidence;
  the current table says replay 1 and chaos 0 and requires live spot checks.

### Sealed-candidate queue and task closure

The overnight ORC relay accumulated more than a dozen “sealed” infrastructure
candidates and repeatedly described them as an adoption queue. Many were never
published, some existed only in `/tmp`, and several were later rejected. Do not
rebuild that queue from old notes. Re-establish each change from current main
and current owner intent, or delete the task.

Likewise, `closed`, `implemented`, `landed`, and `deployed` were repeatedly
confused. A task status or a local clone does not prove ancestry; ancestry does
not prove the primary checkout or running service uses the commit. Any graph
node claiming completion only from a task note needs fresh ancestry and live
activation evidence.

### Degraded-ORC draft PRs

The owner reported Hermit draft PRs #2242 through #2246 from the degraded ORC
period. #2246 was described as the only clearly core change (waitid scheduler
progress). #2242/#2245 were validation tooling; #2243/#2244 were inventory or
backend-matrix infrastructure likely overlapping this work. Do not land the
four infrastructure PRs merely because they exist. Compare them to current
main, keep only measurable value not already present, and close redundant
drafts. Treat #2246 as product work requiring its own current-main review.

## Other owner-requested follow-up work

These items were intentionally behind M1 and remain useful if kept simple:

- `ci-hub validate-status` with no arguments should show a concise recent-run
  list like `gh run list`.
- `ci-hub local-history` should default to a concise recent view; the full dump
  should require an explicit flag.
- Group ci-hub commands by purpose and clearly distinguish read-only queries
  from actions. `validate-run` is not history/forensics.
- Eliminate compile warnings from unused lease states unless the design is
  actually justified. The owner does not trust the current lease design.
- Keep hard green and soft green explicit. A rebase with only mechanical
  conflict resolution may inherit reviewed content; a content change does not.
  A speculative soft-green land must immediately launch exact-main validate to
  become hard green.
- Distinguish landed from deployed by observing the running component, not by
  checking ancestry alone.
- Periodically scan open PRs and feature branches for interference or useful
  salvage, but do not let PR administration replace M1 execution.

## Recommended ORC continuation

1. Read this handoff and the dedicated wrkslots handoff before assigning any
   writer.
2. Preserve the dirty `agent-utils` files before running parent sync. Do not
   land them as-is.
3. Retry the two old slot continuations only through the registered lifecycle;
   do not delete directories or edit registry JSON manually.
4. Start one Hermit writer from current `origin/main` in a fresh registered
   slot. The primary Hermit checkout stays clean on `main`.
5. Use the pushed repetition branches as references, not as landing candidates.
6. Extract `ci/manifest-plan` into a shared Rust library and migrate
   pressure-test to the direct safe-ci library path with real CLI fixture tests.
7. Run the fast `backend-parity-c/host-identity` 20-copy probe.
8. Launch the 168×20 acceptance run asynchronously; continue documentation and
   inspection while it runs.
9. If and only if all 3,360 results pass, run final exact-SHA full validate,
   publish the clean implementation, update the scorecard/walkthrough, and
   declare M1.
10. Develop M2 strict-default behavior on a feature branch/PR and report the
    exact envelope drop before asking the owner to land it.

## Final cautions

- Agents may not coin terms. Describe facts plainly.
- Do not clear a bar by editing the scorecard, removing a cell, adding an
  exemption, widening a timeout, or weakening the comparator.
- Do not trust a task note more than the current repository and running
  program.
- Do not use `/tmp` as durable artifact authority.
- Do not manually clean registered worktrees.
- Do not let bookkeeping become the product. The immediate product test is
  whether a fresh agent can inspect one cell, run it quickly, understand the
  result, fix it, and improve a committed Hermit scorecard.
