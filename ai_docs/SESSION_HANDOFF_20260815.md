# Session handoff — 2026-08-15

Written by `hermit-coord` on devbig030, task `write-and-land-session-handoff`.

This is **not a summary of activity**. It is the set of facts that are expensive to rediscover,
recorded so a fresh coordinator or a peer machine can resume without re-deriving them.

**How to read the sourcing.** Every figure below carries one of three marks:

- **MEASURED HERE** — I ran the command on devbig030 on 2026-08-15 and the command is named.
- **RELAYED** — it comes from a named task record or another host; the source is named and I did
  not independently reproduce it.
- **NOT-DETERMINABLE** — the claim is in circulation but nothing available to me establishes it.
  It is recorded rather than dropped, because an absent caveat reads as confidence.

> **READ §12 FIRST.** Several load-bearing facts below were corrected or superseded after §1–§11 were
> written. §12 supersedes them and says which. The most important: **main's green belongs to
> `1b98ccef5`, four commits back — not to any head named in §1–§11 — and NOT-VALIDATED is the absence
> of a qualifying run, not a red result.** Sections §1–§11 are otherwise left as written; each figure
> stays anchored to the SHA it was measured against, which is what keeps it usable.

---

## 1. Two blockers stand between a qualified cell and a landed cell, not one

This is the single most important correction in this document. Treating the CPUID failure as *the*
blocker leads to the conclusion that a CPUID-capable host is sufficient. It is not.

### Blocker (a) — `privileged-cpuid.faulting` returns ENODEV on this host

**MEASURED HERE.** `arch_prctl(ARCH_SET_CPUID=0x1012, 0)` in a forked child returns `rc=-1`,
`errno=19` (`ENODEV`), on devbig030 right now, kernel `6.13.2-0_fbk15_hardened_0_g33ebba20e5e4`.

**MEASURED HERE — the reboot boundary.** `/proc/stat btime` is `1786431757` =
**2026-08-11T07:02:37Z**. Computed from the tracked ledger `ledger/hermit/devbig030/2026-08.jsonl`
(119 rows, of which 102 are `profile=full`), splitting `emitted_at` at that instant:

| | full-profile runs | passed |
|---|---|---|
| before the reboot | 71 | **34** |
| after the reboot | 31 | **0** |

**Do not conflate the run figure with the gate figure — they have different denominators.**
The `privileged-cpuid.faulting` gate appears in **58 of the 71** before-rows (54 under that name
plus 4 under the older name `cpuid.faulting`), **all passing**; and in **13 of the 31** after-rows,
**all failing with exit 101**. So "0 of 31" is a *run* result. It is not 31 gate failures; the gate
is simply absent from the other 18 rows.

**MEASURED HERE — the only discriminated delta.** `/proc/cmdline` carries a prepended
`csdlock_debug=1`. **No causal claim is made about that token.** It is recorded because it is the
one difference anyone has isolated, not because it is believed to be the cause.

**NOT-DETERMINABLE — "hardware is unchanged down to the microcode."** This claim is in circulation
and I have no reason to doubt it, but no before-reboot hardware or microcode snapshot is available
to me, so I cannot reproduce the comparison. Treat the *conclusion* as unverified here even though
the ENODEV itself is measured.

**RELAYED — the routing consequence** (task `lane-patching-backends-share-then-ratchet`): a
CPUID-capable host needs **kernel 6.17+ for AMD** (`CpuidUserDis`, leaf `0x80000021` bit 17).
Re-running on this box cannot help; "NEEDS-RERUN" is the wrong reading of this signal.

### Blocker (b) — `gate.manifest` exceeds its budget independent of any PR's content

**RELAYED, from the closed task `establish-what-gate-manifest-actually-measured`**, which measured
it through an actual failing run:

- Enforcement is **aggregate cgroup CPU, not wall time**. Budget is **300 CPU-seconds**. The runner
  polls the step cgroup's `cpu.stat` `usage_usec` once per second and reaps when
  `cpu_used_s >= 300`.
- The failing run crossed 300 aggregate CPU-seconds at **372.567 wall-seconds** and ended at
  **372.923 wall-seconds**. The outer **900-second wall limit did not fire**: `step_end` records
  `timed_out=false`, `cpu_timed_out=true`.
- **The unit trap, and it has bitten repeatedly.** `journal.jsonl` shows `elapsed_s=372.567`
  sitting beside `limit_s=300`. These are *different measurements* — wall against CPU — and the
  field names do not say so. A reader who compares them concludes the gate is off by 24%.
- **Attribution:** the terminating work is the known post-`91533e73` repeated-compilation path
  (`pressure-test.rs` self-test → `scorecard.rs` check → `rust-script` → `cargo build` → `rustc
  serde_derive`). `pressure-test.rs` grew 3553 → 5426 lines at `91533e73`, and `self_test`
  `pressure_cells` call sites grew 2 → 6. It grew again to 5697 at `1b98ccef` with the call count
  staying 6, and is still 5697/6 at `0301dc4e`.
- **#2281 did not introduce it.** #2281 changes pin metadata, locks and pin literals — not
  `pressure-test.rs`, `scorecard.rs`, `validate_plan.rs`, or the runner.
- **The exact node CPU total was read but not retained and is no longer recoverable.** With n=1 and
  no exact value, this run cannot apportion cost between components or establish a plateau.

**The standing problem, which outlives the individual task:** the gate's own self-test keeps
growing, and every addition makes it likelier to trip. The pressure that creates is either to stop
instrumenting or to keep raising the bound. **Both are bar-lowering dressed as maintenance.** The
ceiling fix was withdrawn, not deferred, on exactly that reasoning.

---

## 2. Each host holds half of the CPU-journalling fix, and the halves are consistent

**MEASURED HERE — the half we hold is a finding, not an unlanded commit.** Every local ref in
`agent-utils/` is an ancestor of `origin/main`; nothing is ahead. Anyone searching this host for an
unlanded patch will find none and may wrongly conclude the work was lost. It was not: the
"correction" is the verdict recorded on the closed task
`settle-whether-cpu-stat-is-read-before-the-cgroup-is-destroyed`.

Two careful readers on two machines reached opposite conclusions about the same gap, and the
disagreement decided which fix was correct:

- **hermit-shell:** the CPU value is already collected before cgroup cleanup and merely omitted
  from `journal.jsonl` — a one-field emit of a value already in hand.
- **devbig014:** the cgroup is destroyed before anything reads it — a new read must be added.

**hermit-shell's mechanism is the correct one.** *"No new post-exit cgroup read is required for
`step_end`."* Shipping devbig014's reading would have produced a fix that looks correct and journals
nothing, or journals a zero.

**MEASURED HERE — `f00c3135` exists, is unlanded, and is already shaped to our correction.**
Full SHA `f00c313525f1897dfd15ecc0b0554e2a3e63cfcd`, branch
`journal-cpu-stat-before-cgroup-teardown` on `rrnewton/agent-utils`, subject *"Journal final
per-step CPU counters"*, authored from devbig014. It is **not** an ancestor of `agent-utils`
`origin/main`. Its merge-base with `origin/main` **is** `origin/main`, so it is exactly
`origin/main + 1` with no divergence. Its own commit message says it reuses *"the `cpu.stat` map
already captured before cgroup cleanup"* — i.e. **the two halves agree; they are not in conflict.**

**Design constraint agreed by both sides, worth preserving:** the unit must be in the field name
(`cpu_usage_usec`, `wall_elapsed_s`, `cpu_limit_s`, `wall_limit_s`), and a missing counter must stay
absent rather than becoming a measured zero, because a zero falsely claims a measurement.

### The line-number trap in this cluster — check the revision before you open the file

Two task notes both name agent-utils commit `b4951000ceccb08b16b1aab1f4cf303a4ca84172` (hermit's
`agent-utils` gitlink) but are **numbered against different revisions**. **MEASURED HERE**, both
sides:

| Claim | At `b4951000` | At `c810ddb` (parent's own `agent-utils/`) |
|---|---|---|
| `scheduler.rs:1518-1521` = the `cpu.stat` read | **correct** — `:1518` is literally the comment *"Read the step's cgroup measurements BEFORE cleanup() removes the child cgroup."*, read at `:1519-1522`, `cg.cleanup` at `:1535-1538` | **wrong** — `:1518` is an unrelated CPU-timeout path (`mculprit.lock()` then `reap()`); the read is at **`:1616`** and cleanup at **`:1633`** |
| `scheduler.rs:1614-1617` = final read, `:1687-1690` = `cpu.*` profile insert | **wrong** — `:1614-1617` is `shared.lock()`/`sh.running.remove()` abort bookkeeping; `:1687-1690` is an ABORT emit format string | **correct** — exactly the comment + read, and exactly the `row.insert(format!("cpu.{k}"), …)` loop |

Both findings are sound. Only the anchors are mixed. **Anchor by the comment text, not the line
number.**

---

## 3. Qualified and unlanded — and every cell count in circulation is stale

**MEASURED HERE**, by reading `git show <ref>:SCORECARD.md` at each ref. Denominator is 5376 cells
throughout.

| ref | green | red | what cites it |
|---|---|---|---|
| `origin/main` | **170** | 5206 | current landed state |
| `2b805d60d` | **171** | 5205 | #2267 head — **still current** |
| `f588e8590` | **178** | 5198 | closed task `independently-verify-three-scorecard-claims` |
| `f40babbbb` | **181** | 5195 | open task `independently-verify-liteinst-181-and-25` |
| `b8bfa040d` | **187** | 5189 | the "liteinst branch at 187" figure |
| `eae6f26ef` | **190** | 5186 | **#2263 head right now** |

**#2263's branch has run 178 → 181 → 187 → 190.** Every one of those figures was correct when
written. None is wrong. All but the last are stale. **A liteinst cell count is meaningless without
its head SHA** — that is the fact worth carrying.

**The "179 combined, measured not summed" figure is now stale.** 179 was measured against #2263 at
178 (+8 over main) plus #2267 at +1. With #2263 at 190 (+20), it must be re-measured before it is
quoted again. **I did not re-measure it**, because doing so requires an actual merge, not
arithmetic — which is precisely why the original was measured rather than summed.

**"31 cells at Bar B" is bound to `b8bfa040d` and I did not re-measure it.** The open task
`independently-verify-liteinst-181-and-25` reports **25** at Bar B on `f40babbbb` (16
`verify/ptrace` assert-only plus 9 others). 25@`f40babbbb` and 31@`b8bfa040d` are consistent with a
growing branch, not a contradiction. **The count at the current head `eae6f26ef` is
NOT-DETERMINABLE** without re-running the classification.

### PR queue — MEASURED HERE from the GitHub API, 2026-08-15 ~20:20Z

All four are OPEN, non-draft, `mergeable=MERGEABLE`:

| PR | branch | head | labels |
|---|---|---|---|
| #2263 | `feature/liteinst-enable-two-verified-cells` | `eae6f26ef` | `adversarial-review-codex1` |
| #2267 | `fix/recovery-cell-lane` | `2b805d60d` | — |
| #2281 | `fix/bump-reverie-pin-ee6716a` | `0301dc4e2` | `adversarial-review-codex1`, `passed-review-codex` |
| #2282 | `fix/assert-expected-values-in-three-parity-fixtures` | `1506fc1f2` | — |

**#2263's head has moved repeatedly.** `passed-review-*` does not auto-clear on an ordinary push, so
on this branch specifically, confirm any review label was applied to the *current* head before
treating it as authority.

---

## 4. The liteinst lane is at the gate, and the gate is a core-abstraction change

**RELAYED, from the closed task `trace-the-actual-enotsupp-producer` — obtained by tracing a real
run, not by inference.** The remaining blocked cells are task-creating guests refused by a single
producer.

**MEASURED HERE — I verified these citations in the source** (reverie checkout at `ee6716a`,
`reverie-ptrace/src/injected_syscall.rs`), because two of the three line citations I checked
elsewhere in this session were bound to the wrong revision. **This one is exact:**

- `:152` — `// TODO-HUMAN-REVIEW(PR-103): Review representable rewritten-register updates.`
- `:153-156` — `pub(crate) fn validate_user_regs_update(...) -> Result<(), Errno>`
- `:157-167` — the `unsupported` predicate, covering `orig_rax`, `rip`, `cs`, `ss`, `ds`, `es`,
  `fs`, `gs`, **`fs_base` (`:165`)**, **`gs_base` (`:166`)**, and non-status `eflags` bits
- `:168-172` — `if unsupported { Err(Errno::ENOTSUPP) }`

**Why this makes the blocked cells one class:** creating a thread requires setting the new thread's
TLS base — `fs_base`, and `gs_base` on some paths. Both are in the unsupported list. The
injected-syscall frame used by the e9patch/LiteInst path cannot represent a segment-base update, so
**any guest that creates a thread is refused.** The traced sequence ends
`intercepting guest syscall … syscall=clone3` → `beginning inject of syscall: clone3` →
`Error: -524 ENOTSUPP`.

This is consistent with every observation in the lane: all blocked cells are threaded or
thread-creating, and single-threaded guests run clean under liteinst.

**The fix is not authorized.** Changing the injected-syscall frame to represent segment-base updates
plausibly changes a core abstraction, and owner policy requires discussion before a core-abstraction
change. Extending with new event types is fine; changing the model is not. **The lane is blocked on
an owner decision, not on measurement or effort.**

---

## 5. Verdict channel — settled, do not re-derive

**MEASURED HERE** at hermit `origin/main` `61b9ba7aa`. **Re-checked at `302a1a9c0` for §12: both
facts below still hold, and the count is still 2.**

- `ci/test_harness.sh` is an **elif chain**. `:3796` is `elif [[ $status != 0 ]]`; `:3799` is
  `elif [[ $mode == verify && $(verify_asserts_bitwise_parity "$metadata") == true ]]`.
  **Both gate. Exit status is evaluated first and takes precedence.**
- **Exactly 2 cells assert `bitwise_parity` on `origin/main`:**
  `tests/e2e/manifests/backend-parity-c.toml:84` and `tests/e2e/manifests/c-programs.toml:3734`.
  **A naive grep returns 3** because `tests/e2e/manifests/inventory/test-files.json:727` mentions
  the term in *prose*. Match the assert, not the word.

**`verify` also refuses a stdout divergence at identical exit status.** **RELAYED** from devbig014,
n=1 in each direction, not locally reproduced — n=1 each way is a bracket, not a rate; do not quote
it as a frequency.

**MEASURED HERE — the mechanism, and it resolves how this fits the elif chain.**
`hermit-cli/src/bin/hermit/verify.rs:415-427`, `VerificationOutcome::into_exit_status`:

- `Verdict::Matched` → `Ok(self.guest_status)` — the **guest's** status is passed through
- `Verdict::Diverged` → `Err("Mismatch between run 1 and run 2 outputs.")`
- `Verdict::NoResult` → `Err(...)`, fail-closed, explicitly rather than mapping "no verdict" onto a
  guest exit status

So **there is no separate stdout channel.** The comparison happens inside hermit and surfaces as
**hermit's** exit status. This supersedes any framing that counts stdout-channel candidates
separately (`hermit-bytes` corrected exactly that on `origin/main`).

**The consequence for §5's elif chain, which is easy to get backwards:** the `$status` tested at
`:3796` is *hermit's* exit status, not the guest's. A stdout divergence at identical **guest** exit
status therefore makes hermit exit nonzero and trips **`:3796`** — the exit branch — and never
reaches the parity branch at `:3799`. **A cell does not need a `bitwise_parity` assert to be
refused for a stdout divergence.** What the assert adds is refusal of the *converse* case: a
zero hermit exit whose published verdict is not full parity.

---

## 6. Instrument hazards that each cost real time

Recorded because each one produced a wrong answer that looked right.

1. **A control at one layer does not validate another.** A control run proves what it exercises and
   nothing above or below it.
2. **Check-mutation and value-mutation give different answers.** Deleting an assertion and
   corrupting the value it checks are different experiments with opposite verdicts. A
   check-mutation refusable count is an *upper bound*.
3. **`uptime -s` prints local time; `/proc/stat btime` is epoch.** **REPRODUCED HERE TODAY:**
   `btime` = `1786431757` = **07:02:37Z**, while `uptime -s` prints **`2026-08-11 00:02:38`** — a
   7-hour error. **A second form of the same trap, which I hit while checking the first:**
   `awk`'s `strftime()` also renders **local** time, so
   `awk '/^btime/{print strftime("%Y-%m-%dT%H:%M:%SZ",$2)}'` prints `00:02:37` **and labels it
   `Z`**. Use `datetime.fromtimestamp(b, timezone.utc)`.
4. **A truncating filter asserting a negative reports present things as absent.** A truncating
   filter is safe in a *presence* check and unsafe in an *absence* check. A `head -40` on a ref
   sweep earlier in this session produced a false "on no ref" for three artifacts that were tracked.
5. **Host load above roughly 70 produces timeouts that must not be scored as refusals.**
   **MEASURED HERE:** load average is **69.76 on 316 cores** right now — about **22% utilization**,
   with `/proc/pressure/cpu` `some avg10=3.32`. **So the threshold is not CPU saturation, and the
   mechanism is NOT-DETERMINABLE from load and PSI alone** — both are blind to `flock` waits. Record
   the correlation; do not assert contention as the cause.
6. **Line citations in task notes are revision-bound and are mixed in practice.** Of three I checked
   today, **two pointed at the wrong code** at the revision they named (see §2). Verify a citation
   against the revision before relying on it.

---

## 7. State at handoff — MEASURED HERE, 2026-08-15

- dev-hermit parent: on `main`, in sync with `origin/main` at the time of writing. The parent
  advanced during this task (`730aa749` → `578e9b08`), so **re-sync before reading `ledger/`** —
  it is tracked and advances only on pull.
- hermit: `origin/main` = `61b9ba7aa` when this was written; **now `302a1a9c0` — see §12.**
- reverie: checkout at `ee6716a65d41e8f1d65ee32efa4aafa910b9cf29` — the pin #2281 advances to.
- agent-utils: local `c810ddb`, behind `origin/main` `b8b3274a`; nothing local is ahead.
- **Parent shows ` M hermit`.** This is a **forward** pin — the hermit checkout is *ahead* of the
  parent's recorded gitlink `e2f9a36b`, not behind it. It is not a backward pin and it is not mine
  to commit.
- **Cells landed remains zero.** `origin/main` is at 170 green **cells on the scorecard** and no
  liteinst cell has landed. **That is a cell count, not a validation verdict** — do not read it as
  main being green. Where main's green actually lives is §12.
- Slot `coord` (owned by `hermit-coord`, shared read-only with `codex-lander-audit` and
  `hermit-276`): branch **names** match — `worktrees/coord/hermit` and `worktrees/coord/reverie` are
  both on `codex/cross-client-skill-discovery` and `liteinst2` is detached, exactly as `ACTIVE.md`
  records. **This does not establish that the slot is where I think it is** — see the hazard below.
  One untracked `.safe-ci-dag-runner/` directory in `coord/hermit`, left alone.

### Re-allocation moves the registry ahead of the disk, and the registry cannot detect it

**RELAYED** — found by `hermit-teardown`, then corrected by `hermit-bytes` while running the check.
This is a silent-corruption hazard: nothing errors, and work simply lands on the wrong branch.

The allocator enforces one slot per agent, so **re-allocating an already-owned slot to a new task
moves the registry ahead of the disk**: the registry names the new branch while the checkout is
still on the old branch at the old head. In teardown's words, *"an agent that edited without
checking would have been working on the previous task's branch while the registry claimed
otherwise."*

**The obvious check is insufficient, and this is the part worth carrying.** The registry records
**no head** for a slot — only a branch name. So a branch-name comparison is the only
registry-versus-disk check the registry can support, and **it passes in a case where the hazard is
present**: a re-allocation that moved the branch *name* while leaving the checkout at an old head
*on that same name* looks clean. The name check rules out one failure mode and is **silent on the
other**.

What actually establishes it is cross-checking the disk HEAD against a commit you know you pushed
for the *current* task. **If you have no such record, you cannot verify the slot from the registry
alone — say so rather than assuming clean.** Do **not** `git checkout -b` to reconcile a
divergence; worktree-provisioning says stop.

**Applying that standard to this handoff: the `coord` slot is NOT verified.** The name check passed,
but this task wrote nothing to the slot, so I hold no task-pushed commit to cross-check the head
against. Its head state is **NOT-DETERMINABLE** by me.

**One route is structurally safe, and it is the one this document took.** Writes through
`scripts/parent-main-write` are unaffected by the hazard: it fetches and verifies ancestry on every
write and publishes to `main` directly, rather than to whatever branch a slot happens to be on, so
the wrong-branch hazard cannot reach them.

## 8. Open owner decisions

These are blocked on a person, not on work. Recording them so they are not re-investigated as if
they were technical unknowns.

1. **The Reverie injected-syscall frame change** that would unblock the liteinst lane (§4) — a
   core-abstraction change requiring discussion first.
2. **Bounding how much measurement `gate.manifest` carries** (§1b). Raising the bound converts a
   visible failure into an invisible one and destroys the only instrument watching the cost.
3. **A write lane for `agent-utils`** so `f00c3135` and the emit it depends on can land (§2).
   `agent-utils` is product code and needs a PR; none is assigned.

---

## 9. Foreign dirt in the parent tree — it predates you

**MEASURED HERE, and independently observed by three agents.** `hermit-bytes` and `hermit-revents`
each reported the following without consulting each other, and I observe the same:

```
 M  hermit                    (modified submodule pointer)
?? hermit-stat-fixture.tmp    (untracked)
```

**Nobody claims either one. All three of us left them untouched, which is correct** — unexpected
changes are somebody else's work, and the rule is record and stop, not tidy.

**Recorded here so that whoever resumes does not attribute them to their own run.** This matters
more than usual right now because **the residue gate and the unpushed-commits gate are both dark**,
so these observations are the only visibility anyone has on parent-tree state.

On the submodule pointer specifically (§7): it is a **forward** pin — the hermit checkout is *ahead*
of the parent's recorded gitlink `e2f9a36b`. It is not a backward pin, and it should not be `git
add`ed to "fix" it.

---

## 10. A commit can vanish from `main` while its content lands — and byte-identity is the wrong test

**RELAYED** from `hermit-revents`, **then corrected in three places by measurement here.** The
substance is right and important; the details would mislead a naive check.

`d40dce4b` (*"ai_docs: persist the 2026-08-15 operating rules with their incidents"*) is **not an
ancestor of `origin/main`** — **MEASURED HERE**. Its content nonetheless landed. **Anyone searching
`origin/main` for `d40dce4b` would conclude the work was lost. It was not.**

Three corrections, each **MEASURED HERE**:

1. **The commit object did not vanish.** `d40dce4b` still resolves, and it is reachable from **three
   pushed refs on origin**: `refs/remotes/origin/rescue/auto-d40dce4b`,
   `refs/remotes/origin/rescue/auto-26ff743e`, `refs/remotes/origin/rescue/auto-dcf6ad27`. So
   "did not survive as a commit object" is too strong. It survives *on origin*, just not on `main`.
   **Search all refs, not just `main`, before concluding a commit is gone.**

2. **`98c16a31` did not add the 38th rule.** Its parent `98c16a31^` already had **38 rules** (531
   lines). `98c16a31` kept the count at 38 and added **20 lines** to an existing file. The rule
   count reached 38 before it.

3. **A byte-identity check would FAIL here and wrongly read as lost content.** The file at
   `origin/main` and at `98c16a31` is **38 rules / 551 lines**, sha256 `fcda7364…`. The file at
   `d40dce4b` is **27 rules / 300 lines**, sha256 `1521f178…`. **These are not byte-identical, and
   they should not be** — `origin/main` carries a *superset*.

**The correct survival test is containment, not equality.** **MEASURED HERE:** of `d40dce4b`'s 27
rule headings, the number absent from `origin/main` is **zero**. That is what establishes the
content survived.

**MEASURED HERE — current state of the file** at `origin/main`
(`ai_docs/transient/2026-08-15-operating-rules-with-their-incidents.md`): **551 lines, 38 rules**
(38 `###` headings under 6 `##` sections), sha256
`fcda7364d63492225ec795212cc8eb973306871da08529c4c2508509c46f8580`.

**A counting hazard I hit while checking this, worth carrying:** my first rule count returned **6**,
because the regex matched the `##` section headings and numbered list items instead of the `###`
rule headings. The document's own structure defeated a plausible-looking count. **Name the heading
level you are counting.**

---

## 11. The unpushed-commits gate reported a large false number and missed the one true case

**This is the only genuinely stranded work anyone found on 2026-08-15**, and the gate built to find
exactly this did not see it.

**RELAYED** (owner): the unpushed-commits gate reported **1316**, which turned out to be a
tag-reachability artifact — **zero real unpushed commits** within the population it scans.
**§12 corrects the scope of that zero: it is true of the parent repository and false of the fleet.**
Six or seven change clones hold commits on no remote ref.

**MEASURED HERE:** meanwhile, two real commits sat on **zero origin refs** in a temp clone at
`/home/newton/temp/dev-hermit` — a path the gate never looks at. `hermit-906`'s work:

| | |
|---|---|
| commits | `cfd64cb77ca43f2ddbdd3b39c4658f7d7b146f7e`, `5ae9f7545b4bdbfae310f0fffd264d609b179bca` |
| author tag | `[hermit2, hermit-906, unresolved, devbig030, role=impl]` |
| touched | `ci-hub/lib/agent_identity.py`, `ci-hub/lib/tests/test_agent_identity.py` — **nothing else** |
| base | `61663764596dd237953ec64360ea928abee799f4` |
| tree | clean |

**The lesson is about scope, not arithmetic.** A gate's number is a claim about the population it
scans, and this gate's population excludes the temp clones where per-change work is deliberately
done. **A large number from a gate and a true miss are not opposites; here they were simultaneous.**

**Preserved, not vouched for.** On owner authorisation the tip was pushed **additively** — new ref,
no force, no rebase, no amend — to
`rescue/hermit-906-unreviewed-who-am-i-identity-resolver-brittleness` on `rrnewton/dev-hermit`.
**MEASURED HERE by read-back from the remote:** both commits are now reachable from exactly one
origin ref each. The `rescue/` prefix is the existing convention on this remote.

**THE SEAL DOES NOT COVER THE CURRENT HEAD, AND THESE TWO COMMITS ARE UNATTESTED.** The clone's
`.git/ci-hub-change-seal.json`, sealed **2026-08-13**, names candidate `5ae9f754` — but HEAD has
since moved to `cfd64cb7`. **Preserving work is not the same as vouching for it.** Whoever picks
this up is inheriting unreviewed, unsealed work under someone else's name; the branch name says so
deliberately so it cannot be mistaken for authored or vetted.

**One artifact to avoid on inspection:** `git diff origin/main..HEAD` in that clone shows 17 deleted
`ledger/hermit/devbig014/2026-08.jsonl` lines. That is a **two-dot direct tree comparison against a
stale cached `origin/main`**, not a deletion by these commits. Against the merge-base the diff is
the two `agent_identity` files only. **`A..B` in `git diff` is not merge-base relative** — `git log`
and `git diff` read the same syntax differently.

---

## 12. Corrections — read this before acting on §1–§11

Written by `hermit-coord` on 2026-08-15, task `refresh-the-session-handoff-with-corrections`, after
§1–§11. **A handoff carrying a stale premise is worse than none**, because the next coordinator acts
on it without re-testing — which is how three stale blockers cost hours today. Every figure below
names its source and the SHA it is anchored to.

### 12.1 Main's green belongs to `1b98ccef5`, four commits back — MEASURED HERE

| | |
|---|---|
| last green | **`1b98ccef5d5098bbc3e72fba689b1a026aaeb780`** |
| hermit `origin/main` now | **`302a1a9c0fde564db0292d1ea5cabc91343e79bc`** |
| distance | **4 commits** |

`git rev-list --count 1b98ccef5..origin/main` = **4**, and they are `302a1a9c0`, `61b9ba7aa`,
`5465e1451`, `d6856c1f9`.

**NOT-VALIDATED is not red. It is the absence of a qualifying run.** A head with no record is not a
head that failed. Conflating the two turns a missing measurement into a defect that nobody can find.

**That green is a local devbig014 receipt, not a GitHub verdict** — `profile full/full`, host
`devbig014`, wall 1001 s. **RELAYED** via the evidence promoted at `d6258467`
(`ai_docs/evidence/verify-main-green-and-scorecard-20260815/`), produced by `hermit-teardown`. The
distinction matters because §12.2 shows there is no GitHub verdict to be had.

### 12.2 GitHub Actions has been dead since 08-12, and landing was never blocked by it

**MEASURED HERE** via the GitHub API at 2026-08-15:

- `repos/rrnewton/hermit/actions/permissions` → `enabled=false`
- `repos/rrnewton/reverie/actions/permissions` → `enabled=false`
- the three most recent hermit workflow runs are all **2026-08-12**, all `status=queued`,
  `conclusion=null` — dispatched and never executed.

**So hosted CI could not be measured at all — there was nothing to measure.** An absent hosted
verdict in any report from this period is that, not a failure.

**MEASURED HERE — landing was not blocked by it.** `git log --since='2026-08-14 00:00:00' origin/main`
= **10 commits** on hermit main.

**RELAYED, and explicitly not established:** three services stopped on **three different dates**
(08-11, 08-12, 08-14), which argues against a single sweep. **Who set `enabled:false` is
NOT-DETERMINABLE** — nothing available to me establishes it.

### 12.3 The drift over those four commits — MEASURED HERE, and "trivial" is a behavioural claim

`git diff --stat 1b98ccef5..302a1a9c0`: **+326 / −70 across 7 files**. Per commit:

| commit | shape |
|---|---|
| `d6856c1f9` | +391 / −2 |
| `5465e1451` | +128 / −400 |
| `61b9ba7aa` | +2 / −1 |
| `302a1a9c0` | +198 / −60 |

**"One commit entirely undone" is nearly right and I measured the exact remainder.** `d6856c1f9`
added `scripts/lib/validate_e2e_coverage.rs` (+358) and `ci/expected-e2e-plan.json` (+16);
`5465e1451` removed both. At the tip **`validate_e2e_coverage.rs` is absent** (undone) but
**`ci/expected-e2e-plan.json` is present** — so it is *substantially* undone, not entirely.

**`61b9ba7aa` really is two test-side lines**, and both matter: `set_tid_address.c` changed
`return 0` → `return ok == 4 ? 0 : 1` (a fixture made able to fail), and
`backend-parity-c.toml` gained `assert = { bitwise_parity = true }`.

**The "trivial" characterisation is RELAYED and my line count does not corroborate it on its face** —
+326/−70 is not small. The two are not in conflict: a behavioural characterisation and a line count
measure different things. **A characterisation is not a receipt**, and neither is a diffstat.

### 12.4 §5's count of 2 is still correct at the new head — MEASURED HERE

Manifests asserting `bitwise_parity`: **1** at `1b98ccef5`, **2** at `61b9ba7aa`, **2** at
`302a1a9c0` (`backend-parity-c.toml:84`, `c-programs.toml:3734`). `61b9ba7aa` is the commit that
added the second. §5 stands as written and is current.

### 12.5 Change clones hold work the unpushed gate cannot see — and the count moved because I moved it

**RELAYED:** seven change clones of 28 hold commits on no remote ref, two of them holding **38** and
**36** commits; a disposal path now refuses all seven; devbig014 has the same blind spot and it has
already cost them a label-publisher fix that exists only in a change clone.

**MEASURED HERE, and it differs — the difference is me.** Enumerating `.git` directories under
`/home/newton/temp`, `ignored/isolated-clones` and `ignored/ci-hub-changes` to depth 2 I find **40**
clones, of which **6** hold commits on no *local remote-tracking* ref. **The seventh was
hermit-906's clone, which now reports 0 because I fetched its own pushed rescue ref into it.**
Different denominator (40 vs 28) is a different enumeration scope; I state mine rather than
reconciling to a number I did not produce.

**MY COUNT IS AN UPPER BOUND AND SO WAS THEIRS.** Both read the clone's *local* remote-tracking refs,
which is exactly the check that produced a false refusal on hermit-906 — its work was on the remote
while its cache said otherwise.

> **CORRECTION, 2026-08-15, task `preserve-the-five-recommended-clones` — the paragraph above was
> right to caveat and my own spot-check inside it was WRONG.** I originally reported
> `dev-hermit-who-am-i-rebuild` / `6395b534` as *"genuinely absent, a true positive."* It is not.
> `6395b534` is an **ancestor of `origin/hermit2/standing-dev-hermit-work`** (tip `05023b21`) and has
> been preserved all along. My check was `ls-remote | grep "^<sha>"`, **which matches only refs whose
> TIP is exactly that commit** — a commit preserved as an ancestor reads as absent. See §12.10.
>
> **All six clones are preserved. Not one holds unpushed work.** MEASURED HERE by reachability
> (`git branch -r --contains`) and independently re-run from the primary checkouts rather than the
> clones: `213c86a4` → hermit `ci-hub/resolve-task-closure-policy-contradiction` **and PR #2225**;
> `e1efc42` → reverie same branch **and PR #445**; `80e50e9e` → `ci-hub/p1-a-producer-path` **and
> PR #132**; `3d8b2339` → `recovery/primary-checkout-remote-config-3d8b2339`; `6395b534` → ancestor
> of `hermit2/standing-dev-hermit-work`; and all four `agent-utils-publish.W4M2GC` commits are each
> on 4 origin branches, its HEAD being an **ancestor of `agent-utils` `origin/main`** — that clone is
> *behind* main, not ahead.
>
> **The whole "clones hold unpushed work" figure is an artifact of stale local remote-tracking
> caches**, not of missing remote objects. The clones were made with narrow refspecs and the work
> reached the remote by paths that wrote no tracking ref back. **Fetch before counting.**

**The blind spot itself is structural and stands:** change clones are separate repositories, and no
enumeration of one repository's refs reaches another's objects.

### 12.6 The `1316` figure, with its scope corrected

**RELAYED** (owner): `1316` was a tag-reachability artifact; **real unpushed work in the parent
repository is zero.** §11 recorded that. **§11 understated the scope of the zero:** it is true *of
the parent repository* and false *of the fleet* — six or seven change clones hold commits on no
remote ref (§12.5). The two are consistent because they are different populations, which is §11's
own point turned on §11.

### 12.7 The kvm figure is 25, not 22 — RELAYED

**RELAYED, source not available to me, so NOT independently verified:** the kvm figure is **25**;
**22** is the count carried into qualification; and only **9 are qualified on both arms**. Three
different numbers for three different populations — do not collapse them.

### 12.8 hermit-906 — MEASURED HERE

Rescue branch **exists**: `rescue/hermit-906-unreviewed-who-am-i-identity-resolver-brittleness` at
**`cfd64cb77ca43f2ddbdd3b39c4658f7d7b146f7e`** on `rrnewton/dev-hermit`, confirmed via the GitHub API
and by cloning the branch from the URL alone (both commits present, file content byte-identical).

**The 08-13 seal attests only `5ae9f754`, the first of the two commits. Nothing attests HEAD at
`cfd64cb7`.** Proved by recomputing the seal's digest with the tool's own algorithm: base
`61663764` → `5ae9f754` = `316f8b5d…` (matches the seal); → `cfd64cb7` = `b1c894e0…` (does not).
**Preserving is not vouching.**

### 12.9 Instrument misfires — see §12.10

The fifth misfire (a bare `--since` date silently dropping two commits) was first recorded here.
**It and the six others are collected in §12.10 with the worked example**, so the count and the
conclusion sit together rather than being scattered.

### 12.10 Seven misfires of one shape, and the only thing that has ever caught them

Every one of these produced a confident wrong answer that looked right. They are collected because
the count is the argument: **recognising the pattern did not prevent a single one of them.**

| # | The instrument | It said | The truth |
|---|---|---|---|
| 1 | `head -40` on a ref sweep | 3 artifacts "on no ref" | all 3 were tracked |
| 2 | `awk strftime()` / `uptime -s` | boot at `00:02:37` | `07:02:37Z` — both render **local** time |
| 3 | `grep -c` on a `###`-structured file | 6 rules | **38** — the pattern matched `##` and list items |
| 4 | `grep -c` on a table header | 55 closed tasks | **54** — it counted the column header as a row |
| 5 | `git log --since=2026-08-14` | 8 commits | **10** — see the worked example below |
| 6 | `grep` for a phrase across a line wrap | 0 occurrences | present — the text wrapped mid-phrase |
| 7 | `ls-remote \| grep "^<sha>"` | commit not on the remote | preserved as an **ancestor**; tip-match ≠ reachability |

**The conclusion, and it is not a counsel of vigilance.** A zero from a hand-written pattern is a
claim about the pattern before it is a claim about the corpus. **The only thing that has ever caught
one of these is a second check that disagrees** — not care, not knowing the failure mode, not
re-reading. #5 was caught only because it contradicted a figure supplied from outside. #6 was caught
while writing up #5. #7 was caught because a reachability test contradicted a tip-match, and it had
*already been written into this document as a verified result* before the second check ran.

**So: for any load-bearing negative, run a second measurement by a different method, and prefer one
that would fail differently.** A repeat of the same method is not a check.

#### Worked example: a date boundary that silently drops two commits

```
git log --since=2026-08-14            origin/main   ->  8 commits
git log --since='2026-08-14 00:00:00' origin/main   -> 10 commits
```

Same repository, same ref, same intent, **two commits apart.** Git parses the bare form as an
approximate date that does not resolve to midnight. Nothing errors and nothing warns; the smaller
number is simply wrong and entirely plausible.

**This is the shape that survives review.** A reviewer sees a reasonable command and a reasonable
number and has no reason to question either. The only reason the 8 was caught is that it disagreed
with a figure that came from somewhere else. **Spell the boundary out**, and treat any figure derived
from a bare date as unanchored until it is.

### 12.11 The hang-shaped heuristic has a hard limit: it cannot be applied by substring match

The re-test heuristic that came out of this session is sound — **a hang or timeout carries no
information about cause and is therefore re-testable, while a message-bearing failure names something
that either still holds or does not.** It is why two long-standing record-hang blockers were re-tested
today and did not reproduce.

**MEASURED HERE — the annotation those two now carry.** Both
`record-hangs-on-concurrent-pipes` and `record-hangs-on-concurrent-multiprocess` carry
*"DID NOT REPRODUCE. TREAT THIS NOTE AS HISTORICAL."* **and** *"NOT REPRODUCED IS NOT FIXED. No cause
was ever established for the original."* Exactly two notes carry that second phrase. **RELAYED:**
devbig014 has adopted the annotation practice.

**THE LIMIT, WHICH NEITHER MACHINE HAD STATED AND WHICH MATTERS MORE THAN THE HEURISTIC.** A keyword
is exactly how the next person will try to scale this, and it does not work. **RELAYED:** devbig014
ran one and got 20 rows, alphabetically truncated at the limit, **not one of them a genuine
hang-shaped blocker.**

**MEASURED HERE — I reproduced it on a different corpus and it is worse.** Matching
`hang|timeout|hung|stall` case-insensitively across the 366-file memory corpus returns **242 files —
66% of everything.** The two genuine entries sort at **positions 182 and 183 of 242**, so an
alphabetically truncated top-20 captures **zero** of them. Precision of the naive match: **2 genuine
out of 242 returned, 0.8%.**

**Two independent failures on two corpora, in different directions**: theirs returned too few and all
wrong; mine returned most of the corpus with the right answers unreachable behind truncation. Neither
is fixable by a better keyword, because the discriminator is not lexical — *any* note that merely
mentions a timeout matches, and a genuinely hang-shaped blocker need not use the word.

> **devbig014's sentence is the one to keep:** *telling them apart requires reading what the note
> claims, which is agent work, not a query.*

**This also lands squarely in §12.10's family**, and is its ninth instance — a truncating filter
asserting a negative, arrived at independently on another machine. The pattern now has an
out-of-house witness.

### 12.12 STANDING RULE for devbig030: do not invoke full or privileged profile — use `--portable-only`

**Owner directive, 2026-08-15, effective immediately.** It changes what we run here, not what counts.

**Why — MEASURED HERE (§12 and the dispatch investigation):** **35 full-profile runs on this host
since the 08-11 reboot; 16 died on `privileged-cpuid.faulting`; ZERO passed.** Nothing intermittent.
This host lacks CPUID faulting, the full plan hard-requires that node, so a full run here is
structurally guaranteed to fail — burning real CPU and minting a worthless ledger row each time.

**What to run instead — RELAYED from `hermit-sabre`:** portable-only **completes** here. 1 pass in 7
post-boot, and the six failures were **contingent** (real test failures, clippy, three CPU timeouts)
rather than structural. **A determinate verdict beats a guaranteed non-result.**

**The reporting rule, non-negotiable: never report a portable-only result without naming the profile
beside it.** A portable-only green is indistinguishable from a full green in any summary that omits
the profile. That is the same wrong-noun failure that produced a wrong green SHA reported hourly for
most of 2026-08-15 (§12.1).

**A portable-only pass is NOT a receipt.** Anything needing a qualifying receipt still routes to
devbig014.

**MEASURED HERE — the exact floors, because the two in circulation are not the same thing:**

- `ci-hub/validate/qualifying-receipt.json` is the single canonical predicate and requires
  `commit_anchored: true`, `tree_dirty: false`, **`profile: "full"`**, `selection_mode: "full"`,
  `result: "pass"`, `failures_max: 0`, `executed_tests_min: 1`, plus `accepts_green_class: ["hard"]`
  and a coverage block. **`profile: "full"` is why a portable-only pass can never be a receipt.**
  **There is no `checks == 5` in it.**
- `ci-hub/landing/reconcile_receipts.py:72-75` does test `profile == "full" and result == "pass" and
  checks == 5` — but **that is a cheap local prefilter, deliberately looser, not a second floor.**
  The file says so itself at `:22-23`: the `is_clean_full_pass` certifier (`ci-hub validate-status`)
  is the arbiter and "local-history's `checks==5` prefilter is LOOSER". Do not quote `checks==5` as
  a requirement to satisfy; it selects candidates the real certifier then narrows.

**If a full run is genuinely needed here: say so and stop. Do not run it.** The owner authorises or
reroutes in one message.
