# Hermit PR drain — gate analysis and blockers, 2026-08-12

Written by `claude-coord-fable` on `devbig030`. Everything here is measured, with the command or
evidence beside it. Where a number is a count, its denominator is stated.

## Where the pool stands

`rrnewton/hermit` went **128 → 21** open across this session; `rrnewton/reverie` is at **9**.
Main is `1f4e430c`, **VALIDATED** (full profile, devbig014, 969 s).

**All 21 open Hermit PRs are `MERGEABLE`** — zero conflicts remain after a rebase pass.

## The gate table — this is the important part

Measured across all 21 open PRs with `approval_binding.py` and `ci-hub validate-status`:

| gate | passing |
|---|---|
| mergeable | **21 of 21** |
| validation (`VALIDATED`) | **1 of 21** (#2181) |
| review authority at current head | **0 of 21** |

**Review, not validation, is the universal blocker.** Eighteen report
`REQUIRED-APPROVAL-SET-MISSING`; three carry no approval labels. Anyone queuing for a validate slot
on a PR that also lacks review is waiting behind the wrong gate. Reviews need no `validate-lock` and
run in parallel.

Part of the empty review column is expected: five PRs were rebased in this session, and **a rebase
voids exact-head review authority in every lane**. That cost was disclosed on each thread.

## Blocker 1 — `devbig030` cannot mint a receipt (hermit #2205)

`privileged-cpuid.faulting` runs `rdrand_rdseed_is_masked`, which hard-asserts host CPUID faulting
at `detcore/tests/misc/mod.rs:1187`. On this box:

```
model name : AMD EPYC 9D85 158-Core Processor
arch_prctl(ARCH_SET_CPUID, 0) = -1  errno=19 ENODEV
cpuid_fault : absent from /proc/cpuinfo
```

**This is not an AMD limitation.** The same node passed here **seven times** (2026-08-09 22:21 →
2026-08-10 05:03) with the hard assert already in place. Two candidate code causes were ruled out by
date: `80e7bc1a6` (2026-07-21, added the capability probe as a skip) and `6b5fe95a4` (2026-07-22,
converted the skip to a hard assert) both **predate** those passing runs.

The host rebooted — boot_id moved `b971c325…` → `3ac87f92…` — from kernel `6.17.13-…-crackerjackhost`
onto `6.13.2-0_fbk15_hardened`. **The capable kernel is no longer installed** (`rpm -q kernel` lists
only 6.13.2), so restoring it is a **reprovision, not a reboot**.

Consequence: **every recent full-profile receipt came from `devbig014`** — ten consecutive, none from
here. `ssh devbig014` refuses (`publickey,password`), and `start_unit.py` has no remote-host path.

**How it misleads:** a run dies in ~0 s at a spine node with `exit 101` and an **empty detail block**,
which reads as a broken build, and takes ~42 downstream nodes with it. There is no host-capability
outcome in the schema — "cannot run here" and "ran and failed" are the same record. That is the
substance of #2205 and it is unaffected by whichever kernel is booted.

## Blocker 2 — Reverie's gate is a closed loop

```
Actions:    enabled = false
Protection: contexts = ["merge-gate-v2"],  strict = true,  enforce_admins = false
```

`strict` requires the branch be up to date; rebasing to satisfy it destroys the only gate result; with
Actions disabled nothing can mint a new one. Five PRs have zero check-runs **because** they rebased
current.

Three exits exist and all three lower the bar: admin-merge past `enforce_admins: false`, drop
`strict`, or drop the required context. None was taken. Note `986e17e0` is on reverie `main` with
**zero associated PRs** (`gh api …/commits/<sha>/pulls`), which is what admin-merge looks like
afterwards.

**Before re-enabling Actions, cancel queued run `31579812032`** — it targets #430's *old* head
`0fa83f65` and would mint a green at the wrong commit.

## What has been exhausted, and the evidence

- **Closure.** Nine `Preserve …` PRs closed as provable duplicates (three pairs shared byte-identical
  trees; one was byte-identical to a *live* PR). Four more were preservation artifacts, closed and
  consolidated into **#2208** after verifying each `rescue/*` branch sits on origin at exactly the PR
  head. #1919 closed for re-adding content main deleted in `f21b22edd`. A separate sweep found
  **zero of 131** PRs had content already on main, verified two independent ways.
- **Aggregation.** One genuine fold (#2179 → #2177, 10/10 content identity, no force-push needed
  because a cherry-pick made it a fast-forward). Remaining overlaps are 0.12–0.21 concentrated on hub
  files (`detcore/src/lib.rs`, `Cargo.lock`, `ci/dag/portable.json`) — a **contention** signature, not
  a foldability one. Rejected candidates had specific contradictions: #2164 hard-`die`s on argv
  lacking `--verify-strict` while #2181 deliberately constructs exactly that argv (**335 of 336**
  declared verify modes do not opt in); #2172 and #2182 both advance the Reverie pin to divergent
  unmerged siblings.

## Cross-repo dependency worth knowing

**#2172 — the most review-complete PR in the queue — cannot land.** Its Reverie pin `e7972364` is not
an ancestor of reverie main; it **is reverie PR #443's head**. So it is transitively blocked by
Blocker 2. No hermit-side receipt fixes that.

Similarly **hermit #2083** is gated behind **reverie #430**, which now carries a BLOCK.

## Review findings: twelve of twelve

Every commissioned review found substantive defects in code that had already passed validation or CI.
A green receipt has not been predicting review outcomes. The recurring class is a check that cannot
discriminate:

- `compare_two_runs` returns `Matched` unless `diff_found`, so **two empty logs plus identical output
  yield `verified=true`** and a "Determinism verified" banner (#2070).
- An unset node timeout and unset lane default both resolve to `0`, which scores as *safely below* the
  job limit (#2113, fixed).
- `compared_log_messages` reports full vector lengths while comparison **stops after the 21st
  mismatch** (#2181).
- A recognizer accepting only unprefixed `0f 31`, so a legal `66 0f 31` passes unintercepted
  (reverie #403).
- Reverie **#430** carries an ~8 % gate-hanging regression (24 runs, 2 wedges; merge base 10 runs,
  0 wedges) that a **7/7-green, 1,035-test receipt could not see**, because the receipt ran
  single-threaded and the hang is parallel-only.

**Two clean-applying rebases would have silently reverted main-side corrections** — one restoring a
transcribed syscall triple that main had replaced with a membership census (deleting two refusal
tests), one restoring an "reaches L2" claim main had corrected to "passes Stripped verification, not
L2". Both applied without conflict. When rebasing here, check what main changed underneath you, not
just whether the patch applies.

## Two PRs are RED against gates already in the tree

Proven with `jq`, no execution and no lock. Do not spend a validate slot on either until fixed:

- **#2012** — `audit_validate_timing_gate` requires the timing baseline's node set to equal the
  profiled set: **38 selected, 35 in baseline**, missing `build.e2e_artifact`,
  `check.backend_parity_mutation`, `e2e.audit_compile_backend_parity_c`.
- **#2070** — 7 DAG `cmd` mismatches (the PR appends `--results/--junit` past where
  `expected_command()` ends), plus privileged `backend-parity-c` marked
  `skip_reason: "empty-manifest-bucket"` while the plan selects the real cell
  `backend-parity-c/cpuid-probe`.

## Operational notes for the next agent

- **`validate-status` reading `NOT-VALIDATED` may be a stale parent.** A receipt minted on
  `devbig014` is invisible here until `parent-main-write sync` pulls the ledger shard. #2181 was
  nearly written off this way.
- **`VALIDATED` does not mean landable.** The land-time base-currency check
  (`qualifying_receipt.py:572`) is a separate gate and `validate-status` says nothing about it.
- **Validates survive their agent.** They run detached under `systemd-run --user`; check
  `systemctl --user list-units --all 'validate-*'` before relaunching. A lock held by a dead agent may
  be a live run.
- **`ci-hub/bin/gh` refuses comment writes from Claude-Code agents.** `resolve_identity` sets
  `primary` only from `DG_AGENT_NAME` or process ancestry; `cwd-slot-owner` corroborates but is never
  promoted, so a registered slot does not help. **Do not set `DG_AGENT_NAME`** — it fabricates the one
  input the design keeps launch-injected. Seven agents refused it correctly. #2166 is reworking this
  surface.
