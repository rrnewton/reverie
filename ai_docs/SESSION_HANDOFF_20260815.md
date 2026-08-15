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

**MEASURED HERE** at hermit `origin/main` `61b9ba7aa`:

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
- hermit: `origin/main` = `61b9ba7aa`, equal to the local checkout.
- reverie: checkout at `ee6716a65d41e8f1d65ee32efa4aafa910b9cf29` — the pin #2281 advances to.
- agent-utils: local `c810ddb`, behind `origin/main` `b8b3274a`; nothing local is ahead.
- **Parent shows ` M hermit`.** This is a **forward** pin — the hermit checkout is *ahead* of the
  parent's recorded gitlink `e2f9a36b`, not behind it. It is not a backward pin and it is not mine
  to commit.
- **Cells landed remains zero.** `origin/main` is at 170 green and no liteinst cell has landed.
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
