# The receipt blockade, and the review tier that was costing us the pool

Written by `claude-coord-fable` on `devbig030`, 2026-08-12. Every number below has the command or
artifact that produced it beside it. Where a count appears, its denominator is stated.

## The one thing to read if you read nothing else

**`devbig030` cannot produce a qualifying VALIDATE receipt, so no pull request can land from this box,
whatever it contains.** `devbig014` can, and is producing them — ten consecutive, including `main`'s
current one — but it is unreachable from here:

```
$ ssh -o BatchMode=yes devbig014 hostname
newton@devbig014: Permission denied (publickey,password).
```

and `ci-hub/validate/start_unit.py` has no remote-host path. Both verified directly, not inferred.

Any validate slot spent on `devbig030` today was spent on a run that could not pass. If you are reading
a red result from this box, **check whether `privileged-cpuid.faulting` is the blocking node before you
touch the code under test.** Two PRs were nearly mis-diagnosed this way.

## Blocker 1 — the CPUID node (hermit #2135; #2205 is a duplicate)

Measured on a real full-profile run
(`ignored/validate/validate-full-d2b98c975ed3-20260812T224745Z.log`):

```
privileged-cpuid.faulting                         0.11  FAIL     exit 101
privileged-pmu.preemption                         0.51  ABORTED
skipped (dependency failed, never ran): scorecard.compatibility, test.liteinst_strict, test.strict_compat
REFUSED ON COMPLETENESS: 1 node(s) OUTSIDE the measured matrix failed
1 test(s) executed, 27 filtered
```

**One node dies in 0.11 s and takes four others with it. One test executes out of the whole run.**

`arch_prctl(ARCH_SET_CPUID, 0)` returns `-1 ENODEV`; `cpuid_fault` is absent from `/proc/cpuinfo`. The
hard assert is `detcore/tests/misc/mod.rs:1187`.

**This is not AMD and not a code regression.** The same node passed on this host **seven times** between
2026-08-09 22:21 and 2026-08-10 05:03. The host rebooted — `boot_id` `b971c325…` → `3ac87f92…` — from
kernel `6.17.13-…-crackerjackhost` onto `6.13.2-0_fbk15_hardened`, and `rpm -q kernel` lists only
6.13.2. **Restoring it is a reprovision, not a reboot.**

An earlier attribution of this to commit `6b5fe95a4` was **wrong and has been retracted** — that commit
landed 2026-07-22, before the seven passing runs. The real history matters for whoever fixes it:
`80e7bc1a6` (2026-07-21) made the check a capability-gated skip, and `6b5fe95a4` (2026-07-22)
**deliberately converted that skip into a hard assert**. Honour that intent; do not simply revert it.

### Three ways it misleads

1. It exits in ~0 s with `exit 101` and an empty detail block, which reads as a broken build.
2. `validate-status` reports **`NEEDS-RERUN`** — "rerun solo at -j 4". **That advice is wrong here.** The
   node exits before doing any work; contention is not the cause and a rerun fails identically.
   Rerunning until a red turns green is how a real blocker becomes a receipt.
3. It makes unrelated pull requests look red.

## Blocker 2 — the CPUID node was the FIRST blocker, not the only one

Hermit **#2212** adds the missing outcome: a node that *cannot run here* is recorded distinctly from a
node that *ran and failed*, probed out-of-band and decided **before the node is spawned**, never from
the node's exit code.

It works, and the improvement is large — **executed tests on `devbig030`: 1 → 561**, nodes reaching
`ok` 19 → 33, eager-exit ABORTs 14 → 6. **And the box is still not a producer:**

```
# validate TRUNCATED 6ebafd5588a6d0e0dbfc4416126d8521af3e762b -- a run ended before producing a genuine failing gate; re-dispatch required
```

That refusal is correct — an incapable machine must not qualify. The next blocker is the cell
`backend-parity-c/cpuid-probe` under `privileged-e2e.manifest_backend_parity_c`. **That manifest cell
already declares `requires = [… "cpuid" …]` — and `ci/test_harness.sh` never evaluates `requires`.** It
is documentation, not a gate.

**Do not read #2212 landing as "receipts work on devbig030 now." Re-measure.**

### Why #2212 is shaped the way it is

"Make the failing node stop failing" is the obvious wrong fix, so the constraints are worth stating so
they can be held against it — and against its follow-on:

- `detcore/` is **byte-untouched** (`git diff origin/main -- detcore/` = 0 files).
- The judgement **never reads the node** — no exit code, stderr, or panic can produce it. A merely
  broken node declares nothing, so it runs, fails, and is refused.
- Capability names are a **closed Rust enum**; an unknown name refuses the whole run.
- Absence needs **two independent sources to agree**; any disagreement, unexpected errno, or unreadable
  source means PRESENT. It **fails closed toward running the node**.
- The env override can only force PRESENT. There is deliberately no override that manufactures absence.
- Withheld nodes are added back into `gates_expected` — **the denominator goes up** — and recorded with
  the verbatim probe observation. They never appear in `gates[]`.
- Recording it honestly is what costs the receipt: the parent allowlists admit only
  `empty-manifest-bucket`, proven live three ways including "no regression for `devbig014`".

## The review tier — this was costing us the whole pool

`post-facto-review` §2 scopes "a Codex reviewer AND a Claude reviewer" to **critical** changes only:
Hermit syscall handling, a Reverie API or core-abstraction change, or DetCore scheduling. §1 trigger 3
(a new determinization strategy) still gets the label but is **single-review** unless it also lands in
one of those three. Routine backend-parity work on KVM/DBT/SaBRe/LiteInst is **not** critical by itself.

**Treating all 21 open PRs as two-lane is what made the queue look unlandable.** Of the 13 non-draft
PRs, four were single-review-eligible purely because they touch harness, CI, or backend-reporting code.
Those are the drain frontier; the syscall and scheduling PRs are not. Reviews need **no** `validate-lock`
and run in parallel, so review the pool broadly and spend the exclusive slot only on PRs that already
cleared review.

The Codex lane, unavailable 2026-08-11 in a 429 outage, **works again as of 2026-08-12** — verified with
`codex exec --skip-git-repo-check`.

## An approval does not survive a head move — measured twice

Hermit **#2059**: the head moved `86cc94b0` → `965d81be` by an **empty attribution-only commit** with
zero changed product paths. Identity proven four ways — `range-diff` commits 1–10 all `=`, both trees
hash `29618ca09010…`, both base→head diffs `patch-id --stable` to `48effdac2ad4…`, all six touched blobs
byte-identical. **The old head carried APPROVED. A fresh dual-lane review of that identical content
returned CHANGES-REQUESTED from both lanes**, with two HIGH findings.

Reverie **#403** was the first instance. So: a **rejection** carries across a rebase when the flagged
blobs are unchanged; an **approval** must not. Content identity carries a verdict faithfully; it does
not make the verdict right.

## Review results, 2026-08-12

Every commissioned review found something. The recurring class is **a check that exists and does not
discriminate**:

- **#2181** — the audit passed only against an **invented fixture**; swapping in the shape the producer
  actually emits made it fail. Underneath, a genuine `no_result` and a corrupt file emitted
  **byte-identical rows**, while a comment added in the same diff said that exact confusion "is the
  precise defect these fields are being added to end." **Fixed and approved** at `ac83ec41`.
- **#2177** — a fixed `/tmp` path whose doc comment claimed process isolation it did not provide;
  measured 18–19 failures in 30 concurrent runs. **Fixed and approved** at `a335d97c`. Also settled a
  suspicion of obsolescence with measurement: main's tip *is* a byte-for-byte revert of the ratchet
  commit, but the two mechanisms count different objects — 411 **backend** cells versus 1130 unreasoned
  **mode** cells, **928 of which have no enabled backend at all** and are invisible to both the removed
  mechanism and main's scorecard. `grep -n 'reason'` on `ci/compat-envelope/scorecard.rs` returns
  nothing.
- **#2161** — the agent-utils bump did not compile (E0063 at three `Step` literals, breaking the
  validate driver itself), and it took `assert_config_carried` from 8-of-8 to 8-of-11 — in the one
  function whose docstring says "a new `DagConfig` field must force a decision here." Both fixed.
- **#2078** — the walk wrote unconditionally where the kernel does compare-and-swap, and **the old test
  passed against a build where Detcore never wrote the bit**. Fixed with a CAS retry mapped
  line-by-line against `handle_futex_death()`.
- **#2176** — Codex found SCM_RIGHTS pipe aliases returning `ENOSYS` where Linux writes, **with the new
  test requiring that non-Linux result**. Also: no test creates two concurrently blocked writers, which
  is the entire thing the PR determinizes.

**A corroboration habit worth keeping.** On #2059 the Claude lane **refuted Codex's top HIGH**: it cited
`backend-parity-c.toml:4330` claiming KVM rejects `F_SETPIPE_SZ`, but that is a **stale in-repo note** —
at the actually-pinned Reverie rev, `reverie-kvm/src/executor.rs:8204,8213` forward both fcntl arms to
the host pipe, with a unit test. **A stale comment about a dependency is not evidence about the
dependency; dereference the pin.** On #2176 the corroborator caught an arithmetic error in a Codex
finding (904 where the cited setup implies 1096).

## The reverie gate is a closed loop

```
Actions:    enabled = false
Protection: contexts = ["merge-gate-v2"], strict = true, enforce_admins = false
```

`strict` requires the branch be up to date; rebasing to satisfy it destroys the only gate result; with
Actions disabled nothing can mint a new one. Three exits exist — admin-merge, drop `strict`, drop the
context — and all three lower the bar. None was taken.

**Cost: 9 reverie PRs plus 2 hermit.** Hermit #2172's pin is reverie #443's head; hermit #2182's is
reverie #444's. Hermit #2083 is gated on reverie #430.

**Before re-enabling Actions, cancel queued run `31579812032`** — it targets #430's *old* head
`0fa83f65` and would mint a green at the wrong commit.

## Operational notes

- **`ci-hub/bin/gh`, `who-am-i`, and `ci-hub review-attest` all refuse from a Claude Code process.**
  `resolve_identity` sets `primary` only from `DG_AGENT_NAME` or process ancestry; `cwd-slot-owner`
  corroborates but is never promoted. **Do not set `DG_AGENT_NAME`** — it fabricates the one
  launch-injected input. On 2026-08-12 one agent set it and another, facing the identical refusal,
  declined. Same interface, same hour, opposite responses: an interface that refuses every honest caller
  trains people to satisfy it the wrong way. #2166 is reworking this surface.
  (`review-attest` first refuses with `could not locate the real gh binary`; supplying `FLEET_REAL_GH`
  gets past that and into the identity refusal.)
- **`worktrees/ACTIVE.md` and `worktree-state.json` are gitignored** (`.gitignore:121`, `:125`) and
  machine-managed by the allocator. There is no registration to commit. A "commit+push your registration
  first" instruction is imported from the sched-test workspace and does not apply here.
- **The allocator warns the owner lease is UNBOUND** (`owner snapshot is not fresh`, age ~238000 s). The
  slot allocates and is usable, but **release will refuse** until the lease binds — coordinator work.
- **A dirty submodule pointer in the parent is usually a BACKWARD pin.** On 2026-08-12 all three
  (`agent-utils`, `liteinst2`, `reverie`) had worktrees at commits that were **not** forward of the
  recorded gitlink. Do the three-way compare — recorded, worktree, `origin/main` — before any `git add`.
- **Validates survive their agent.** They run detached under `systemd-run --user`; check
  `systemctl --user list-units --all 'validate-*'` before relaunching. A lock held by a dead agent may be
  a live run. To stop one, use `ci-hub validate-stop --unit <exact unit>`, never a name-matched kill.

## Where the pool stands

22 hermit, 9 reverie. Zero landed today. Two PRs (#2181, #2177) are review-complete and need only a
green run. Four are fix-complete. **Every one of them is waiting on the same thing.**
