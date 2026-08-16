# Re-derivation of the six restored cells, on the observable the verdict reads

**Task:** `re-derive-the-six-restored-cells-on-the-observable` · agent `hermit-observable`
· devbig030 · 2026-08-15/16 · **portable lane, backend ptrace**.

**This is not a receipt.** It is a read-only screen. No manifest was touched, no product
file modified, nothing enabled, nothing committed to `hermit`.

## What was asked, and what the answer is

Six cells were restored to the exit-gated bucket during the 18-cell re-screen. Re-derive
each one with a bite proof on the observable the verdict actually reads — **the exit status
for replay** — not on `.text`, not on the source, not on the build artefact.

**All six re-derive as EXIT-GATED. 12 of 12 mutations, across two independent mutation
families, move the replay exit status from 0 to 1.** The result agrees with
`hermit-fixtures`' verdict and rests on different mutations and a different, stronger
observable record.

## The instrument, derived from source rather than from a note

Both gate commands were read out of the harness generator, not taken from another agent's
description of it — `hermit/scripts/manifest-to-commands.rs` at
`302a1a9c0fde564db0292d1ea5cabc91343e79bc`:

| what | where | command |
|---|---|---|
| **GATE, replay** | `manifest-to-commands.rs:269-271` | `hermit --log=info --backend ptrace record start --strict --verify --data-dir <cell>/recording --record-timeout 90 -- <guest>` |
| **GATE, verify** | `manifest-to-commands.rs:258-267` | `hermit --log=info run --backend ptrace --strict --verify --no-virtualize-cpuid --max-timeslice=disabled -- <guest>` |
| env for both | `manifest-to-commands.rs:39` | `LC_ALL=C TZ=UTC HOME=… XDG_CONFIG_HOME=… E2E_TMPDIR=/tmp/hermit-e2e E2E_FIXTURE_DIR=…` |
| **OBSERVABLE** | not a harness command | `hermit --log=error run --backend ptrace --strict --no-virtualize-cpuid --max-timeslice=disabled -- <guest>` |

**What the replay verdict reads is the exit status and nothing else.** `commands_for_test`
(`manifest-to-commands.rs:307-393`) emits the replay cell as one shell command line; the
cell's result is that line's exit status. `verify_bitwise_parity` is gated on
`mode == "verify"` (line 353) and on an `assert.bitwise_parity` key that none of these six
declare, so no comparator is attached in either mode. The manifest rows carry
`observation = { status = true, stdout = true, stderr = false }`, but nothing in the replay
path consumes `observation.stdout`.

### That claim was demonstrated on the running command, not only read from source

Under the **replay** gate, a mutant's record/replay comparison **runs to completion and
succeeds**, and the command still exits 1:

```
$ hermit --log=info --backend ptrace record start --strict --verify … -- aio-refusal.M2-oracle
:: Recording... :: Replaying... :: Comparing captured verification logs...
Done processing logs, no substantive differences found (72 | 72 DETLOG messages compared).
:: Success: replay matched recording.
rc=1
```

**The comparator said "match" and the cell fails anyway.** That is the claim "the replay
verdict reads the exit status" shown directly, rather than inferred. Reproduced on
`aio-refusal.M2-oracle` (72|72 DETLOG), `sysv-ipc-refusal.M1-behaviour` (74|74), and
`seccomp-refusal.M1-behaviour` (72|72) — and, with the opposite outcome, on the control
`clone.M1-behaviour` (86|86 compared, `:: Success`, **rc=0**).

**The two gates fail a mutant by different mechanisms.** Under the **verify** gate the same
mutant never reaches a second run at all:

```
:: Run1...
First run errored during --verify, not continuing to a second.
Error: First run during --verify exited in error
rc=1
```

So a refuse arm under `record start --strict --verify` *does* exercise the full
record → replay → compare path; a refuse arm under `run --strict --verify` does not. Neither
failure is a comparator verdict; both are the guest's exit status. This matters when reading
a refuse-arm log: the replay arm's log will contain a `:: Success` line, and that line is not
evidence that the cell passed.

**The gate is doing real work despite being fast.** A full pass is 60 Hermit executions in
**4.21 s** wall (`/usr/bin/time`), i.e. ~70 ms each. These guests are a handful of syscalls;
each accept run still records, replays and compares 72–92 DETLOG messages.

**The replay command passes no guest stdout through.** Measured, not assumed: for all seven
accept binaries the replay command's guest stdout is empty, including `c-programs/clone`,
whose accept binary prints 60 bytes under `hermit run`. That is exactly why the bite is
proven under `hermit run` and the gate is read under the replay command — two separate
measurements, never one.

**Instrument provenance.** `hermit` binary
`/home/newton/work/dev-hermit/hermit/target/release/hermit`, version string
`hermit 0.2.0 (2026-08-16, g302a1a9c0fde)`, sha256
`1ec7968570ff9fc033c23b3ee61b01ff8097d8a795e59e10d1ae68d88436bec8`, **rehashed unchanged
after the last run**. This is a *release* build; `hermit-fixtures` used a debug build of the
same commit, so the two derivations do not share a binary. Fixture sources were read by
`git show <SHA>:<path>` with a hard assert on a non-empty read; fixture paths came from each
manifest's `program` key, never constructed.

## The mutations

Two families per cell. Each is anchored on a string **asserted to occur exactly once** in
the source — a uniqueness assert, not a first-match replace, because first-match hit three
fixtures during the 18-cell re-screen (`MEMBARRIER_CMD_GLOBAL` into a `#define` block,
`RUSAGE_SELF` into a line-1 comment).

- **M1 — behaviour.** The guest issues a *different* call at the site, so the contracted
  refusal is never exercised. Where a substitute was needed, `prctl(PR_GET_DUMPABLE, …)` was
  used, after a standalone probe confirmed Hermit permits it (`rc=1 errno=0` under ptrace
  strict, matching native).
- **M2 — oracle.** The guest still issues the *same* call; only the errno the contract
  demands changes (`ENOSYS`/`EOPNOTSUPP` → `EPERM`). The mutant's own diagnostic then reports
  the real refusal that did occur, which proves the syscall was issued and was refused.

**M1 and M2 target different checks from each other, and — deliberately — different checks
from the ones `hermit-fixtures` mutated.** For five of the six, `hermit-fixtures` mutated
check 1; four of my M1 mutations land on check 2 or check 3, so the exit status is shown to
be gated on a check the guest reaches only after passing earlier ones.

## Result — one row per mutation

12 of 12 non-control mutations EXIT-GATED; 1 of 1 control NOT-EXIT-GATED.

| cell | mutation | acc replay | mut replay | acc verify | mut verify | verdict |
|---|---|---:|---:|---:|---:|---|
| aio-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| aio-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| child-subreaper-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| child-subreaper-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| mce-kill-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| mce-kill-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| name-to-handle-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| name-to-handle-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| seccomp-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| seccomp-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| sysv-ipc-refusal | M1-behaviour | 0 | 1 | 0 | 1 | EXIT-GATED |
| sysv-ipc-refusal | M2-oracle | 0 | 1 | 0 | 1 | EXIT-GATED |
| **c-programs/clone (CONTROL)** | M1-behaviour | 0 | **0** | 0 | **0** | **NOT-EXIT-GATED** |

## The observable, and by how much it moved

Guest stdout and stderr under `hermit run --backend ptrace --strict`, with Hermit's own log
lines stripped. The accept arm of every one of the six prints its `ok=` line on stdout and
**nothing at all on stderr**; every mutant prints **nothing on stdout** and a diagnostic on
stderr that **names the exact call site that was mutated and reports the real value
observed**. That last property is what separates a bite from a guest that never ran.

| cell | mutation | stdout | stderr | the mutant's own diagnostic |
|---|---|---|---|---|
| aio-refusal | M1 | 9B → 0B | 0B → 56B | `io_submit returned 1 errno 0 (Success), expected ENOSYS` |
| aio-refusal | M2 | 9B → 0B | 0B → 74B | `io_setup returned -1 errno 38 (Function not implemented), expected ENOSYS` |
| child-subreaper-refusal | M1 | 15B → 0B | 0B → 53B | `PR_GET_CHILD_SUBREAPER rc=1 errno=0 (want -1/ENOSYS)` |
| child-subreaper-refusal | M2 | 15B → 0B | 0B → 55B | `PR_SET_CHILD_SUBREAPER rc=-1 errno=38 (want -1/ENOSYS)` |
| mce-kill-refusal | M1 | 13B → 0B | 0B → 37B | `PR_MCE_KILL_GET not refused: errno 0` |
| mce-kill-refusal | M2 | 13B → 0B | 0B → 38B | `PR_MCE_KILL_SET not refused: errno 38` |
| name-to-handle-refusal | M1 | 20B → 0B | 0B → 71B | `name_to_handle_at(.) returned 3 errno 0 (Success), expected EOPNOTSUPP` |
| name-to-handle-refusal | M2 | 20B → 0B | 0B → 89B | `name_to_handle_at(/) returned -1 errno 95 (Operation not supported), expected EOPNOTSUPP` |
| seccomp-refusal | M1 | 13B → 0B | 0B → 48B | `PR_GET_SECCOMP not refused with ENOSYS: errno 0` |
| seccomp-refusal | M2 | 13B → 0B | 0B → 49B | `seccomp(2) not refused with EOPNOTSUPP: errno 95` |
| sysv-ipc-refusal | M1 | 13B → 0B | 0B → 37B | `msgget rc=3 errno=0 (want -1/ENOSYS)` |
| sysv-ipc-refusal | M2 | 13B → 0B | 0B → 39B | `semget rc=-1 errno=38 (want -1/ENOSYS)` |
| **clone (CONTROL)** | M1 | **60B → 64B** | 0B → 0B | *(none — the mutant exits 0)* |

**Read the M2 rows as the strongest single piece of evidence here.** Each one shows the
mutant reporting the *genuine* Hermit refusal it received — `errno 38` (ENOSYS), `errno 95`
(EOPNOTSUPP) — while failing only because the contract was made to demand something else.
A binary that never ran, or that was refused a launch, cannot print that.

## Controls

**1. The instrument returns the other answer.** The brief required suspecting the instrument
when a result comes back uniform, and it did come back uniform. `c-programs/clone`, the known
scaffolding-only cell, was run through the identical pipeline: its observable moved (stdout
`…"Hello from child!"` → `…"CORRUPTED by mutation"`, 60B → 64B) and **the replay exit status
stayed 0, as did the verify exit status.** The pipeline is therefore not stuck on
"EXIT-GATED"; it produced `NOT-EXIT-GATED` on the one cell that should get it, in the same
run, from the same code path.

**2. A zero-mutation control, from the environment rather than the source.** Run natively,
with no mutation whatsoever, **all six accept binaries exit 1** — because the syscalls
Hermit refuses succeed on the host: `io_setup returned 0`, `PR_SET_CHILD_SUBREAPER rc=0`,
`semget rc=14`, `name_to_handle_at(/) returned 0`. The same unmodified binary exits 0 under
Hermit and 1 natively. That gates the exit status on the refusal contract without touching
a byte of source, so it is immune to every mutation-placement hazard.

**3. Repeat measurement.** The whole screen was run **three times**: 20 rows per pass
(7 accept + 13 mutant), 60 Hermit executions per pass.

- **Hermit-measured values: 0 of 20 rows disagree across all three passes.** Every
  observable rc, stdout, stderr, replay rc and verify rc is identical in passes 1, 2 and 3.
- **Exactly one value moves, and it is in the native arm:** `sysv-ipc-refusal`'s native
  `semget` returns id 14, then 15, then 16 — a host-global System V identifier that
  increments per creation. That is the host nondeterminism the fixture exists to refuse,
  showing up in the one arm that is not under Hermit.

*This bounds flake only.* Same harness, same commands, same host, so any systematic error is
present in all three passes.

## Two disagreements, recorded rather than smoothed over

**1. The premise about whose bite proof was `.text`-keyed is refuted by the record.** The
dispatch states that `hermit-fixtures` flagged its own six as resting on a `.text`-keyed
bite proof. Its artifact does not show that. `results.json` from the 18-cell re-screen
records `guest_accept` and `guest_mutant` for all six — observable pairs captured under
`hermit run` — and its task note says it rebuilt the proof on an observable mid-task. **The
`.text`-keyed proof it flagged was `hermit-li2`'s**, the recorded reason for the original
NOT-EXIT-GATED verdicts. The re-derivation was still worth doing and is not weakened by
this; the premise for it was just misattributed.

**2. The re-derivation was worth doing for a different reason than the dispatch gave, and
it is a real gap.** `hermit-fixtures` captured only `p.stdout`, so its mutant observable for
all six reads `(1, "")` — exit 1 with empty stdout. **Empty stdout with rc=1 is also the
exact signature of a guest that never launched**, which is a live failure mode in this
corpus. The fixture's own stderr diagnostic was being produced the whole time and was not
recorded. Capturing stderr converts "the output disappeared" into "the fixture named the
call site I mutated and reported the value it actually saw." That is the substantive
improvement in this pass.

**3. The dispatch's native warning does not hold for these six.** The brief says "a
refusal-shaped fixture succeeds natively and both binaries print nothing." Measured: all six
accept binaries **fail** natively, rc=1, each printing a specific diagnostic. Running under
Hermit is still mandatory, but for the opposite reason — natively the *accept* arm already
fails, so a native measurement could not separate accept from mutant at all.

## Limitations — read these before using the number

- **EXIT-GATING IS NECESSARY, NOT SUFFICIENT.** These six are still **not qualified**.
  Qualification is exit-status refusability under the stripped comparator with
  `parity=false` on every accept arm, and no accept/refuse qualification was run here. Do
  not count these six as qualified or as plan members.
- **Not a receipt.** devbig030 cannot mint one. Portable lane only; no full or privileged
  profile was run.
- **One backend, one host.** ptrace on devbig030. Says nothing about dbt, kvm, sabre, or
  liteinst.
- **Replay is `ci=false` with `backends_enabled = []` for all six** in
  `tests/e2e/manifests/backend-parity-c.toml`. No replay cell exists for them today. The
  mode they are actually enabled in is `verify`/ptrace, which is why the verify gate was
  measured alongside; it moved identically, 0 → 1, on all 12 mutations.
- **A negative is never established by mutation.** One mutation that bites and moves the
  exit establishes gating. No number of mutations that fail to move it establishes the
  opposite — that is only ever a statement about the mutations tried. This report claims the
  positive only.
- **`hermit-fixtures`' artifact was read** (to identify which six cells and which mutations
  to avoid duplicating). Its verdicts were not negotiated, adopted, or reconciled; the
  measurements here are independent.
- **A refuse arm proves the exit status moves; it does not exercise the accept arm's
  comparator.** Under replay the comparator ran and passed on every mutant, so nothing here
  says the record/replay comparison would catch a determinism divergence in these guests.
- **The native control arm has a side effect on this shared host.** Natively, `semget`
  succeeds, and each native run of `sysv-ipc-refusal` leaks one System V semaphore set. The
  three my runs created (ids 14, 15, 16 — each recorded in the captures above) were removed
  with `ipcrm -s`. **Fourteen sets with the identical signature (key `0x0`, owner `newton`,
  `nsems=1`, ids 0–13) predate this work and were left alone**; they are consistent with
  earlier native runs of this same fixture by other agents.

## Reproduce

```
/home/newton/work/dev-hermit/ignored/observable-rederive-20260815/
  rederive.py          the screen; run `python3 rederive.py` from that directory
  render.py            renders TABLE.txt and rows.json from the captured JSON
  results-pass1.json   pass 1, full capture
  results-pass2.json   pass 2, full capture
  results-pass3.json   pass 3, full capture
  rows.json  TABLE.txt generated, not hand-written
```

Gitignored: survives context loss, not host loss, and devbig014 cannot read it. A tracked
copy is promoted alongside this report.
