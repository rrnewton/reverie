# Hermetic validate v3 — handoff from agent(hermit-138), 2026-08-27

Task: `hermetic-validate-v3-clean-base-minimal-env-fixed-test-workdir` (backlogged,
self-releasing via the watcher below). Branch `hermit-138/hermetic-v3`, pushed, head
`fc311ca956bddec4c6339f6a9f2eba246baa73a3`.

## 1. The one fact that decides what is actually blocked

**Fetchability and ancestry are different, and only the lint needs ancestry.**

- `git fetch https://github.com/rrnewton/reverie.git <pin>` returns **rc=0** for the
  current pin. The object is served. Cargo fetches and builds it — proven, because the
  whole in-image producer chain built against exactly this pin.
- `pre.reverie_pin` tests `merge-base --is-ancestor <pin> <reverie/main>`. Its own
  comment names the failing case: *"the commit is PRESENT in the pack but unreachable
  from main"* — the case it exists to refuse, because an object-presence test would
  accept a pin on a branch that is later rebased away.

So: **builds are fine, the lint refuses.** `pre.reverie_pin` is a SPINE node, so its
refusal skips ~70 of 72 nodes and nothing downstream runs.

Measured 2026-08-27: hermit main pin `1393db627933`; reverie main `ab07a892` on the
remote via `ls-remote` (not a stale local fetch); **not an ancestor**.
`rrnewton/reverie#506` landing did NOT clear this — it was rebased, so the commit is
preserved remotely but off main history.

**What would clear it:** bump hermit's reverie pin to a commit on reverie main
(`ab07a892` or an ancestor) **and move `REVERIE_DBT_BUDGET_BOUND_PIN` in
`ci/configure-build-jobs.sh` in the same change.** Moving one without the other
reproduces the exit-75 `gate.manifest` failure seen earlier tonight. The second half
asserts a DBT budget calibration; do not edit it without measuring, which is why I did
not.

## 2. The watcher

`/tmp/hermit-138/hermit-138-watch.sh`, detached under `setsid`, **2-hour bound**,
log `/tmp/hermit-138/watch.log`, run output `/tmp/hermit-138/acc5.txt`.

Polls every 60s for the **ancestry** condition above; when true, rebases the branch onto
fresh main and launches the acceptance validate, retrying up to six times on the
not-admitted cases (freshness, yield, cleanup quarantine). It **stops rather than
improvising** if the rebase conflicts, and logs a timeout if the two hours expire.

Keying on ancestry is deliberate: it is exactly what the blocking gate tests, so a
watcher firing on "506 merged" would spend a validate on a guaranteed refusal.

Its poll loop makes **no GitHub API calls** — two `git fetch` calls over the git
protocol — so it costs nothing against the REST quota.

By hand if it times out or dies: rebase the branch onto `origin/main`, then
`./ci-hub/ci-hub validate-run --checkout <slot 138 hermit> --agent <you> --target <head>
--wait 7200`.

## 3. What option one is, and ⚠️ what is actually dead

The graph **does not partition** into a host half and a container half:
`setup.manifest_plan` builds `test-harness`, consumed by `gate.manifest` and
`e2e.audit_compile_backend_parity_c` on the host AND by the cells in the image. Measured
both directions:

- producers on host, cells wrapped → all 13 cell nodes fail, *"artifact pointer is
  missing or empty"* (at `53539dea0dea`)
- producers moved into the image → `gate.manifest` fails,
  *"target/debug/test-harness: No such file or directory"*, exit 127 (at `31f5c2da0f82`)

⚠️ **What is dead is the PARTITION, not the allow-list.** Option one is still an
allow-list — it wraps the 13 `e2e.manifest_*` cells plus named producers — but it
**duplicates** the shared producers instead of moving them: the host copy stays
untouched and a second copy builds in-image, with cell dependencies repointed at the
in-image copy. That design **works** and is implemented; do not abandon it believing
allow-lists were refuted.

Also refuted, by measurement, is the cheaper repair of publishing host-built outputs
into the image's target directory: a host-built hermit gives **rc=127, `error while
loading shared libraries: libunwind-x86_64.so.8`** at the same mount where a
container-built one runs. The blocker is the toolchain, not the file location.

## 4. Measured costs — option one is cheap

| step | cost |
|---|---|
| in-image `hermit-install` release build (e9patch, SaBRe staging) | **66 s** |
| full in-image producer chain (runtime_release → e2e_artifact) | **177 s** |
| cargo re-seed after a pin move (`run-split-validate.sh --fetch-only`) | **4–5 s** |
| image rebuild after a flake change | 111 s first, **64 s** rebuild |
| in-image `test-harness` build | 23 s |

The image digest **reproduces byte-identically** from the committed lock — verified on
two boxes and again on a rebuild after the image was pruned.

⚠️ The pinned image is **not durable** on devbig014; it was pruned mid-session between
two of my own runs. `run-in-pinned-root.sh` fails closed with *"image is not present
locally"*. Expect to rebuild (64 s) rather than assume presence.

## 5. Coverage — ⚠️ this is a partial, not goal 2 complete

Route one covers the **13** scheduled `e2e.manifest_*` nodes = **308 cells** (306
portable, 2 privileged). It does **not** cover **at least 14** further steps that run
Hermit guests and keep the **host's** working directory and environment:

`test.hermit_integration`, `test.cli`, `test.arbitrary_binaries`, `test.hermit_modes`,
`test.app_strict_verify`, `test.command_strict_verify`,
`test.ignored_syscall_regressions`, `test.rr_suite_contract` (all verified to spawn
`CARGO_BIN_EXE_hermit`), `test.envelope_levels`, `test.applications_e2e`,
`test.liteinst_strict`, `test.sabre_examples`, `test.dbt_parity`, `test.strict_compat`,
plus `test.detcore_misc` / `test.detcore_parallel`, which run guests **in-process**
through the Detcore tool via `reverie_ptrace::spawn_fn_with_config` — no CLI exists to
pass `--workdir` or `--mount` to, so the control does not map in its current form.

`test.dbt_parity` is **architecturally excluded**: both `supports_test_workdir` and
`validate_mode_workdir` refuse DBT because the dispatcher rebuilds the launch from
program/args/env and does not preserve `Command::current_dir`.

This limitation is also in the route-one commit message, so it travels with the code.

## 6. What is already established — do not redo

- Both control arms **PASS** in the pinned root, each with a mutation arm failing **by
  name**: `pwd -P is not /test` when `HERMETIC_TEST_WORKDIR` is mutated;
  `environment contains a name outside the allowlist` when one extra guest variable is
  injected. The environment check is a **set equality with a count in both directions**,
  so a newly-leaking variable fails rather than passing as a subset.
- **200 of 200** through `test-harness run`, working directory rotated over four
  directories, each iteration writing into `/test` and the next asserting it empty at
  start — 400 guest executions, 369 s.
- The producer chain runs end-to-end in-image and scheduled cells **PASS** through the
  real consumer path (`run-with-hermit-e2e-artifact.sh --require-install …`). The
  *"artifact pointer is missing"* failure is gone.
- Five in-image producer copies present in a **real** `--show-plan`, with every host
  copy intact. (`e2e.metadata` is deliberately absent — it produces nothing the cells
  execute, and lane fusion dedupes it into `gate.manifest` anyway.)

## 7. The remaining gap, stated precisely

**One clean validate showing the 13 cell nodes green as a group.** Absolute, not
differential — a contaminated baseline cannot move that bar.

⚠️ Read the **trailing summary**, not the exit code: per-test-id retry counts, the
failed-test-id list, flaky warnings (which fire even on a pass), and whether
`executed_tests` is UNKNOWN. A row with `executed_tests=null` is a **non-verdict** that
cannot qualify a receipt whatever the exit code says — I was handed one tonight.

⚠️ And if you compute a one-directional cell diff, **compare populations before
outcomes**. Measured: applying the outcome test to a change known to break 13 nodes
scored **0 new failing cells**, because 304 of 305 cells never ran and absent cells
leave the comparison silently. Require `cells in baseline only == 0` or void the
comparison. The instrument's noise floor is ~20 cells main-against-main (15 ERROR,
5 FAIL), so nothing below that is separable from zero.

## 8. Adjacent, not mine

- `rrnewton/hermit#2744` adds `ci/hermetic/assert-build-dependencies.sh` (18 executables
  + 4 native libraries) and carries my `flake.nix` and `image.digest` **byte-identically**
  — same digest `e38c3b2d5c…`. **Both it and my branch change `image.digest`, so
  whichever lands second conflicts on that line.** If 2744 lands first, drop the xxd
  commit from my branch on rebase; that is the preferable outcome.
- I reviewed 2744: its real assertion's only call site is
  `run-split-validate.sh:290`, and that script has **no caller on main**. Its self-test
  is scheduled via `make lint-checks`, but a self-test proves the checker works, not
  that anything was checked. Suggested calling it from `run-in-pinned-root.sh`, the
  wrapper every in-image command passes through — which matters because option one
  builds producers in-image *without* going through `run-split-validate.sh`.
