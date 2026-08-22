# HANDOFF — agent `detdebug`, session ending 2026-08-20

Written because the owner is restarting the session and respawning agents into
proper worktree slots. I was spawned into the **shared parent checkout**
`/home/newton/work/dev-hermit`, not a slot.

**Bottom line: nothing of mine is at risk. Everything I authored is committed
and pushed. The one file I edited that a clean would destroy is not mine, and my
change to it is preserved as a committed copy.**

---

## 1. WHERE I WAS WORKING

Every path below is in the **parent repo** `rrnewton/dev-hermit` at
`/home/newton/work/dev-hermit`. I touched **no submodule** — not `hermit/`, not
`reverie/`, not `agent-utils/`.

**Branch:** `cc-triage-reports_20260818` (the branch the checkout was already on;
I did not create or switch branches). `HEAD = 9cf41a49`.

**Remote branch I created and pushed to:**
`origin/detcore-partial-determinism_20260819` on `rrnewton/dev-hermit`, currently
`9cf41a49`, **0 commits unpushed**. All twelve of my commits are there.

Note `cc-triage-reports_20260818` has **no fetched remote counterpart** — my work
is safe because I pushed it under the `detcore-partial-determinism_20260819`
name, not because the local branch is backed up.

### Product/tooling files I edited (all committed)

| path | state |
| --- | --- |
| `scripts/orphaned-hermit-reaper.py` | committed |
| `scripts/test_orphaned_hermit_reaper.py` | committed |
| `ci-hub/health/reverie_pin_lag.py` | committed (restored to `origin/main` verbatim) |
| `ci-hub/health/tick-hub.yaml` | **my hunk committed**; see §2 |
| `demos/lib/qemu_controller.py` | committed in `b674bf51` — **pre-review WIP, needs a Demo-Green-Review before landing** |

### Experiment directories I created (all committed)

```
experiments/detcore-partial-determinism_20260819/   <- this file; the main investigation
experiments/detdebug-geometry-rerun_260819/
experiments/detdebug-guards_260819/
experiments/headroom-monitor-liveness_20260820/
experiments/reaper-disk-criterion_20260820/
experiments/tick-cost-budget_20260820/
experiments/probes-timeout-visibility_20260820/
```

Temp asset dirs I made under `ignored/` (`qemu-guardcheck-assets`,
`qemu-ctrl-assets`, `headroom-demo-*`) are **already removed**. ~14 GB of
snapshot artefacts were deleted after extracting their scalars.

---

## 2. UNCOMMITTED RIGHT NOW — the part that gets lost

**I have no uncommitted work of my own.** Two files in the shared tree are dirty
and I edited one of them; neither is mine to save:

| path | git state | whose | what a clean does |
| --- | --- | --- | --- |
| `ci-hub/health/btrfs_headroom.py` | **UNTRACKED** | another agent's, **contains my liveness fix** | **DELETES THE WHOLE FILE** |
| `ci-hub/health/tests/test_btrfs_headroom.py` | **UNTRACKED** | another agent's, contains my 6 added tests | **DELETES THE WHOLE FILE** |
| `ci-hub/health/gate_set_integrity.py` | **UNTRACKED** | another agent's, I did not touch it | **DELETES THE WHOLE FILE** |
| `ci-hub/health/tick-hub.yaml` | ` M`, 84 lines | another agent's `btrfs_headroom` registration | reverts their registration |

**My changes to those untracked files are already preserved as committed copies**,
so nothing of mine is lost even if the originals are cleaned:

```
experiments/headroom-monitor-liveness_20260820/btrfs_headroom.py.WITH-LIVENESS-GATE
experiments/headroom-monitor-liveness_20260820/test_btrfs_headroom.py.WITH-LIVENESS-TESTS
experiments/headroom-monitor-liveness_20260820/DELTA.md        <- hunk-by-hunk description

(CORRECTED. An earlier draft of this handoff gave these paths as
experiments/detcore-partial-determinism_20260819/... which is WRONG -- there is
nothing by those names there. Verified: all three are tracked and committed in
6cfe9645, and the .WITH-LIVENESS-GATE copy is byte-identical to the live file.)
```

**THE REAL RISK IS NOT MINE:** a clean removes the entire disk headroom monitor
and the gate that watches whether gates exist, plus their registration. That is
the single most valuable unlanded thing in this tree and it belongs to whoever
authored it. **Do not let it be cleaned without their sign-off.**

I did **not** commit those untracked files, deliberately: committing an untracked
file in a shared tree sweeps in someone else's in-flight work.

For `tick-hub.yaml` I needed to commit a one-line timeout change without
sweeping their 84 lines, and the technique worked — worth reusing:

```bash
git diff <file> > /tmp/f.diff        # split into hunks, keep only yours
git apply --cached /tmp/mine.diff    # stage only your hunk
git show :<file> | python3 -c ...    # PARSE THE STAGED VERSION ALONE
```

That last step matters: a config that only parses in combination with someone
else's uncommitted work is a trap for whoever cleans next.

---

## 3. COMMITTED BUT UNPUSHED

**None.** `origin/detcore-partial-determinism_20260819` == `HEAD` == `9cf41a49`,
0 ahead. My twelve commits, oldest first:

```
fbc431a7  Localize the QEMU-snapshot "7 of 10" residual to 19 bytes
e1c6123d  Recover the original wh_* driver: the inputs were identical
bc9d4444  Answer wh_1/wh_7: a stale Python bytecode cache
b674bf51  Guard the in-hermit Python entry point   <- TOUCHES demos/, needs Demo-Green-Review
6cfe9645  Gate the headroom monitor's GROWTH alarm on still-open-for-write
da4dfe3e  Detect orphaned Hermit runs by orphanhood itself
ceeb64cb  Guarantee the reaper's summary= line on every exit path
76ba603b  Report unreadable processes as a third outcome
45de353d  Measure the tick per gate: 6.4% of a core, not 28%
3053dd88  Give pull_request_health a 90s timeout
b4d8dcc8  A timed-out gate is not silent: refute by inducing one
9cf41a49  Restore the landed reverie pin-lag gate
```

---

## 4. WHAT I WAS DOING, AND THE EXACT NEXT STEPS

All seven assigned tasks are **closed** with findings in TaskGraph. Nothing is
mid-flight. Three concrete next steps exist, none of them started:

### (a) The orphan reaper is still scheduled by nothing — HIGHEST VALUE

The detection gate is written, tested (39 tests, 16 subtests, all executed and
passing) and committed. **The tick-hub entry is deliberately NOT installed.**

```
staged, validated, not installed:
  experiments/reaper-disk-criterion_20260820/tick-hub-entry.yaml
```

Resume: run `python3 scripts/orphaned-hermit-reaper.py --report-orphans` once,
confirm a bare `summary=` line appears on stdout, then append that YAML block to
the `reminders:` list in `ci-hub/health/tick-hub.yaml` **using the single-hunk
staging technique in §2**, because that file carries another agent's uncommitted
work. Do not install it unverified — that is how two gates were silenced.

### (b) `probes.py` timeout patch — needs a submodule three-step

```
experiments/probes-timeout-visibility_20260820/probes-timeout-visibility.patch
experiments/probes-timeout-visibility_20260820/verify.py   <- induces 3 cases, orig vs patched
```

`agent-utils` is a **SUBMODULE** (`rrnewton/agent-utils`), **detached at
`6bfbf51`**, matching the parent gitlink. Landing is three steps across two
repos: apply the patch on a **branch** there (a commit on a detached HEAD is its
own gone-by-morning risk) → push to `rrnewton/agent-utils` → bump the parent
gitlink. **I deliberately did not touch the live file**: 28 gates import that
package every five minutes.

### (c) The real fix for the wrapper-orphan bug — NOT attempted

hermit already arms `PR_SET_PDEATHSIG(SIGKILL)` at both **inner** fork
boundaries (`hermit-cli/src/bin/hermit/container.rs:303`, `run.rs:2772`), so
killing the hermit CLI reaps its tree — confirmed live. **The broken link is one
level up: nothing arms it on the CLI against its own parent.** Completing that
chain is the fix. It carries real breakage risk (`nohup hermit run &` would die
with its shell; a `getppid()==1` guard preserves deliberate detachment) and needs
dual adversarial review plus a build I could not do in this checkout.

### Also open, not mine

- This checkout is **53 commits behind `origin/main`**. That single fact caused
  the pin-lag gate failure I fixed; **5 of 30 policed paths are still STALE**,
  and reconciling the parent would address them without touching any file.
- `pull_request_health` timeout raised to 90 s and committed; the phase-shift
  recommendation was deliberately declined.

---

## 5. PROCESSES STILL RUNNING

**ZERO of mine.** Verified by name against every scratch dir and script I
created.

⚠️ **There ARE live hermit/QEMU processes in this tree and they are NOT mine.**
`pids 408037, 408041, 408043, 408068` are a live `hermit run --strict` boot under
`experiments/first-divergence_20260820/bwork/` — a directory I never created.
**Do not attribute them to me and do not kill them on my account.**

Also still present, owner unknown and nobody watching:
`/var/tmp/sabre-erestartsys-probe.log`, 5,920,746,050 bytes, dated Aug 17.

Relevant if anyone reaps orphans during the restart: **all 8 orphans I censused
tonight had `ppid==1` AND a living owning agent.** Reparented to PID 1 is not
unowned; the scope name `run-p<PID>` gives you the owner pid, and killing on
`ppid==1` alone destroys paused agents' work.

---

## 6. ADDENDUM — the two hazards asked about by name

Measured at handoff time. **None of what follows is mine**; I list it because it
is what a restart or a clean will hit.

### The `ci-hub/` uncommitted work is larger than ~380 lines

Tracked-but-modified, **+441 / −22** across four files:

```
  +27  -3    ci-hub/directives/README.md
  +147 -11   ci-hub/directives/check.py
  +183 -8    ci-hub/directives/tests/test_check.py
  +84  -0    ci-hub/health/tick-hub.yaml
```

Plus **1,488 untracked lines** in four files that a clean deletes outright:

```
  803 lines  ci-hub/health/btrfs_headroom.py
  341 lines  ci-hub/health/tests/test_btrfs_headroom.py
  223 lines  ci-hub/health/gate_set_integrity.py
  121 lines  ci-hub/health/tests/test_gate_set_integrity.py
```

Of all of that I touched exactly one file, `ci-hub/health/tick-hub.yaml`, and
**my hunk there is committed** (`3053dd88`, the `pull_request_health` 90 s
timeout). The 84 uncommitted lines in it are another agent's `btrfs_headroom`
registration. The `ci-hub/directives/*` changes are not mine and I never opened
them.

Parent totals: **9 modified (tracked), 46 untracked**.

### The `hermit/` checkout is on reverie's HEAD — do not commit there

```
  hermit  HEAD           af82f1b92a2762840dc5894984be5e1f5bfc5cb9
  reverie HEAD           af82f1b92a2762840dc5894984be5e1f5bfc5cb9   <- IDENTICAL
  parent gitlink/hermit  e2f9a36b1ad80abd0c012f6ca5082b0613ff2cad   <- what SHOULD be checked out
  hermit branch          main          hermit remote: facebookexperimental/hermit
  hermit dirty files     3615
```

The hermit working tree is sitting on **reverie's commit**, so any commit made
there takes a reverie commit as its parent, and it is **3,615 files dirty**.
`reverie/` itself is clean (0 dirty).

**I made no commit in `hermit/` and no edit to it.** I read
`hermit-cli/src/bin/hermit/{container.rs,run.rs}` and several `detcore/src`
files to establish the `PR_SET_PDEATHSIG` finding in §4(c), and read nothing
else. The 3,615 dirty files predate me and are not attributable to my session.

This is also why I could not build or test the hermit-side fix: `make
build-hermit` fails here because `checkout-all` cannot check out the submodule
over that dirty tree. Any agent picking up §4(c) needs a clean hermit worktree
at the correct gitlink first.

### One more thing a clean destroys

`HANDOFF.md` — this file — is itself **untracked**. I have not committed it, per
instruction. If it should survive, `git add` on this single path sweeps nothing,
but that is the owner's call.

---

## 7. THE FOUR ITEMS THE COORDINATOR NAMED — exact paths and verified state

Re-verified at handoff time. **One path in §2 of this document was WRONG and is
now corrected here; the coordinator had been repeating the wrong path back.**

### (1) `ci-hub/health/btrfs_headroom.py` — the liveness fix

```
LIVE      ci-hub/health/btrfs_headroom.py
          UNTRACKED (??), 34,209 bytes, 7 refs to the fix. A CLEAN DELETES IT.
PRESERVED experiments/headroom-monitor-liveness_20260820/btrfs_headroom.py.WITH-LIVENESS-GATE
          tracked, clean, committed in 6cfe9645, 34,209 bytes
          cmp against the live file: BYTE-IDENTICAL
ALSO      experiments/headroom-monitor-liveness_20260820/test_btrfs_headroom.py.WITH-LIVENESS-TESTS  (13,666 b, 6cfe9645)
          experiments/headroom-monitor-liveness_20260820/DELTA.md  (3,675 b, 6cfe9645, hunk-by-hunk)
```

**NOT `experiments/detcore-partial-determinism_20260819/`** — nothing by those
names exists there. Looking in the wrong directory and finding nothing is how
this would have been declared lost.

### (2) `ci-hub/health/gate_set_integrity.py`

```
LIVE      ci-hub/health/gate_set_integrity.py   UNTRACKED (??), 9,582 bytes
          Plus ci-hub/health/tests/test_gate_set_integrity.py, UNTRACKED, 121 lines
          A CLEAN DELETES BOTH. I never edited either — no copy of mine exists.
          Recovery depends entirely on its author.
```

### (3) The orphan reaper and its tests

```
scripts/orphaned-hermit-reaper.py        worktree clean, last commit 76ba603b
scripts/test_orphaned_hermit_reaper.py   worktree clean, last commit 76ba603b
ON origin/main?  NO — both are on detcore-partial-determinism_20260819 only.
```

Survives a clean (committed and pushed), but `tick_code_currency` correctly
reports both as STALE policed paths, because the executing bytes differ from
what landed on main. 39 tests, 16 subtests, all executed and passing.

### (4) The `probes.py` timeout patch — THE EASIEST THING HERE TO LOSE

```
agent-utils                     SUBMODULE of rrnewton/agent-utils
  HEAD                          6bfbf5160d37c609031d16a75baaa9521f1aaf01
  branch                        HEAD  (DETACHED)
  dirty files                   0
  parent gitlink                6bfbf5160d37c609031d16a75baaa9521f1aaf01  (MATCHES)
```

**The live `agent-utils/py/tick_hub/probes.py` is UNMODIFIED — I never touched
it.** There is therefore nothing to lose *inside* the submodule. The patch lives
entirely in the parent, tracked and committed:

```
experiments/probes-timeout-visibility_20260820/probes-timeout-visibility.patch   committed b4d8dcc8
experiments/probes-timeout-visibility_20260820/probes_original.py                (agent-utils HEAD 6bfbf51, for a clean apply)
experiments/probes-timeout-visibility_20260820/probes_patched.py                 (the patched runner)
experiments/probes-timeout-visibility_20260820/verify.py                         (induces 3 cases, orig vs patched)
experiments/probes-timeout-visibility_20260820/README.md
```

That was the deliberate choice: keeping it out of the detached-HEAD submodule is
precisely what makes it *not* losable. To land it later: apply the patch on a
**branch** in `agent-utils` → push to `rrnewton/agent-utils` → bump the parent
gitlink. A commit made on the detached HEAD would be unreferenced and is the
single easiest thing in this tree to lose.
