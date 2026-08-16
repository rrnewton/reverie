# LiteInst + verify queue, rows 61-79: exit-status refusability, measured 2026-08-15

Promotion of a slice that existed only in TaskGraph notes. The predecessor
(`hermit-liteinst`, task `qualify-queue-rows-61-79`) wrote: *"I wrote NO slice
artifact to disk. Everything for rows 61-79 is in these task notes only ... If
this slice must travel, it needs promoting."* This file is that promotion, plus a
re-measurement that changes seven of the nineteen verdicts.

Population: `ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv`, rows 61-79 of the
79 rows with `backend=liteinst` and `mode=verify`. Re-enumerated independently
here (`awk -F'\t' '$1=="liteinst" && $2=="verify"'`, data begins line 180); the
count is exactly 79 and the slice is exactly 19 rows.

## What this measures, and what it does not

**Exit-status refusability.** For each cell: does the unmutated fixture pass
under LiteInst (accept arm), and does a semantic mutation of the fixture produce
a non-zero guest exit (refuse arm)?

Three limits, stated because each one is a claim someone could otherwise read
into the table below:

1. **The comparator never runs on the refuse arm.** Every refuse arm here is
   caught because the guest's *first* run exits non-zero, at which point hermit
   prints `First run errored during --verify, not continuing to a second` and
   stops. Run 2 never executes. This is refusability through the exit-status
   channel alone. It is not evidence that the log comparator discriminates
   anything.
2. **Not L2, and no cell is enabled.** L2 requires the cell enabled and run by
   the harness under its own assertion. None of these is enabled or asserted.
3. **Not a receipt.** Direct `hermit run` on a portable lane, no full or
   privileged profile. Landing still needs a receipt from a host that can mint
   one.

The accept arms *do* report `bitwise_parity: true` under the canonical
comparator, because the invocation carries `--verify-strict`. That is stronger
than the bare-`--verify` Stripped comparator used by some sibling slices, and it
is noted so the two are not averaged together. It does not upgrade the refusal
claim, which remains exit-status only.

## Environment

| Field | Value |
| --- | --- |
| Hermit | `302a1a9c0fde564db0292d1ea5cabc91343e79bc`, verified equal to live `origin/main` at measurement time |
| Binary | `target/debug/hermit` (**debug**, not release) in a disposable clone |
| Backend | `liteinst`, on **both** arms of every pair |
| Invocation | `hermit run --backend=liteinst --strict --verify --verify-strict [--verify-json=<path>] -- <guest>` |
| Guest build | `gcc -O2`, plus `-D_GNU_SOURCE -pthread -Wno-unused-parameter` for the thread/fork fixtures |
| Host | devbig030, 316 cores, load 39.8-63.3 across the session |
| Profile | portable lane; no full, no privileged |

Hermit confirmed the backend on every run:
`[liteinst host hybrid] activation verified (traps=1, hooks=31); Detcore Tool
active in ptrace host`.

**Relaxation, named rather than glossed:** CPUID interception was **not active**.
Every run logged `ARCH_GET_CPUID reported a valid state, but ARCH_SET_CPUID
returned ENODEV ... continuing without CPUID interception`. None of these
fixtures reads CPUID, so this is recorded as an environment fact, not as a
result — but a fixture that did read CPUID could not be qualified from this host
without re-measuring elsewhere.

**Per-cell manifest facts** (read, not assumed): all nineteen carry
`timeout_seconds = 90` and `observation = { status = true, stdout = true,
stderr = false }`. Nine carry no `build.cflags`. Two carry `guest_args`; see the
argument hazard below. All nineteen carry the same LiteInst disable reason,
*"Initial C-corpus migration preserves the established ptrace baseline; qualify
LiteInst separately"* — a migration default, not a measured LiteInst finding.

## Result

Verdict tokens are the predecessor's, unchanged, and no token is a substring of
another, so exact-match and substring counts agree.

| | Count | Cells |
| --- | --- | --- |
| `REFUSES-LITEINST` | 9 | statmount-enosys, syscall-file-io, syscall-quick-wins, sysfs-enosys, sysv-sem-enosys, sysv-shm-enosys, tee-enosys, ustat-enosys, vmsplice-enosys |
| `BLOCKED-CAPABILITY` | 10 | fork-tree, mmap-fork-shared, order-violation, pipe-chain, pipe-prefill, producer-consumer, thread-contention, thread-stress, thread-sync-determinism, wait-on-child |
| **Total** | **19** | |

`NOT-EXIT-GATED` scored for **zero** rows, for two different reasons that should
not be merged: on the nine it is positively refuted (the exit status moved, so
the fixture demonstrably expresses failure in the channel the verdict reads); on
the ten it is unreachable, because the accept arm never passes.

### The nine that refuse

Five paired repeats per row, the accept control run immediately before each
refuse attempt. Accept `rc=0` 5/5 and refuse `rc!=0` 5/5 on every row. Refuse
arms returned in 0.4-0.5 s wall, accept arms in 0.6-0.7 s — against a 90 s
manifest timeout.

Mutation, one operator throughout: **swap the expected errno constant inside the
fixture's own guard**, so a *correct* result now fails the guard and the program
takes its error path.

| Cell | Fixture | Guard edit |
| --- | --- | --- |
| sysv-sem-enosys | `tests/c/sysv_sem_enosys.c` | `errno == ENOSYS` -> `errno == EPERM` |
| sysv-shm-enosys | `tests/c/sysv_shm_enosys.c` | same |
| tee-enosys | `tests/c/tee_enosys.c` | same |
| ustat-enosys | `tests/c/ustat_enosys.c` | same |
| vmsplice-enosys | `tests/c/vmsplice_enosys.c` | same |
| statmount-enosys | `tests/c/statmount_enosys.c` | `errno != ENOSYS` -> `errno != EPERM` |
| sysfs-enosys | `tests/c/sysfs_enosys.c` | same |
| syscall-file-io | `tests/c/syscall_file_io.c` | two mutants, below |
| syscall-quick-wins | `tests/c/syscall_quick_wins.c` | two mutants, below |

`statmount_enosys.c` and `sysfs_enosys.c` are written with the **negated**
phrasing — `if (result != -1 || errno != ENOSYS) { error; return 1; } puts;
return 0;` — where the other five use `if (result == -1 && errno == ENOSYS) {
puts; return 0; } error; return 1;`. A pattern keyed on `errno == ENOSYS` cannot
match `errno != ENOSYS`. That is why the predecessor recorded these two as the
mutation family not matching the source, and its refusal to widen the pattern was
correct. The resolution is not a wider pattern: it is the **same** operator
applied to the mirrored phrasing, with identical semantics.

**Two mutants each for the guard-inversion rows, shallow and deep**, because a
shallow one alone proves very little:

- `mutA` inverts the shared `require_zero` helper (`result != 0` -> `result ==
  0`) and dies at the *first* successful syscall — stderr `getresuid failed`,
  `fstat after fallocate`. It books `rc=1` on almost no executed fixture.
- `mutB` inverts a property check late in the program. `syscall-file-io` mutB
  reached the post-rename readback (`readback /tmp/hermit-file-io-3-c: payload
  mismatch, got "file-io" want "file-io"` — the inverted guard firing on a
  *correct* payload, after open/write/fallocate/truncate/stat/rename/renameat).
  `syscall-quick-wins` mutB reached the `close_range` check, after
  getresuid/getresgid/mmap/munlock/munlockall/munmap/open/write/fsync/sendfile.

Both exit 1. Only mutB shows the fixture actually ran.

### The ten that are capability-blocked

Two repeats per row, `rc=1` 2/2 on all ten, in **two distinct observed shapes**
that a single `-524` count would hide:

| Shape | Count | Cells | Signature |
| --- | --- | --- | --- |
| terminal cleanup | 5 | fork-tree, mmap-fork-shared, pipe-chain, pipe-prefill, wait-on-child | `LiteInst tracee cleanup failed after -524 ENOTSUPP: notifier did not acknowledge terminal cleanup for LiteInst tracee 3`, at a near-constant 4.06 s (4.058 / 4.057 / 4.059 s measured) — the shape of a fixed acknowledgement timeout |
| immediate | 5 | order-violation, producer-consumer, thread-contention, thread-stress, thread-sync-determinism | bare `Error: -524 ENOTSUPP (Operation is not supported)` in 0.4 s |

Re-run without `--verify`, neither group produced the fixture's success line on
stdout, so this is **not** "the guest finished and only teardown failed" — do not
read the cleanup shape as a nearly-passing cell.

This is a documented present-tense backend limit, not a fixture defect, so it is
re-measurable if the limit is lifted. The two shapes plausibly get fixed by
different changes, which is why they are recorded separately.

## The argument hazard, which is live in this slice

`thread-sync-determinism` and `thread-contention` are the only two fixtures here
that take arguments (`int main(int argc, char **argv)`; the other seventeen are
`int main(void)`). Their manifest entries declare
`guest_args = { ptrace = ["cancellation"], kvm = ["cancellation"] }` and
`guest_args = { ptrace = ["contention"] }` respectively — **no `liteinst` key**.

Run with no arguments, both exit non-zero having printed
`usage: ... PATTERN` / `usage: ... contention|epoll` and **no `-524` at all**.
Run with the correct argument, both show the genuine `-524 ENOTSUPP`.

So the `BLOCKED-CAPABILITY` verdict is right on the merits, but only the
argument-bearing run establishes it. A no-argument run reaches the same `rc=1`
by a completely different route and would attribute a usage error to the
backend.

This is not hypothetical and it already has a number: `mode_guest_args` in
`scripts/manifest-to-commands.rs` returns an empty vector when the backend key is
absent, and its own doc comment records the prior incident — *"A guest that
requires an argument and is not given one prints usage and exits non-zero. Before
this channel existed, every consumer of the manifests invoked such a guest bare,
and the resulting non-zero exit was recorded as a determinism failure — see
rrnewton/hermit#1815."*

There is a second-order trap for whoever enables these cells:
`ci/manifest-plan/src/main.rs` **rejects** `guest_args.<backend>` for a backend
that is not enabled. So `guest_args.liteinst` cannot be pre-staged today; it has
to be added in the same change that enables the LiteInst cell. Enabling the cell
without it yields a usage-error red that looks like a backend failure.

## Why seven verdicts changed

The predecessor scored `sysv-sem-enosys`, `sysv-shm-enosys`, `vmsplice-enosys`,
`syscall-file-io` and `syscall-quick-wins` as timing out at 180 s, twice each,
each attempt paired with an accept control that passed. Here the same five refuse
in 0.4-0.5 s, 5/5, at a comparable host load (39.8-46.7 against its 51.2 / 38.4 /
37.8). The gap is roughly 400x, so it is not a contention judgement call.

**The cause of its stalls is not established.** Its mutants were never written to
disk, so its exact mutation is unrecoverable and was not reconstructed. What can
be said is that at the same head, the same backend, the same comparator and a
comparable load, these rows refuse promptly and repeatably. The other two changes
(`statmount-enosys`, `sysfs-enosys`) have a fully identified cause: the mirrored
guard phrasing described above.

## Screens applied

- **Inert mutation.** Step 0 proven on all 11 mutants by comparing the `.text`
  section (`objcopy -O binary --only-section=.text | sha256sum`), *not* whole
  binaries, which are defeated by the ELF build ID. Each source edit was
  separately asserted non-empty. All 11 differ.
- **Inert assert.** No assertion was added to any fixture.
- **Not-exit-gated.** Scored for zero rows; see above for the two distinct
  reasons.
- **Wrong environment.** Excluded by positive evidence, not assertion. The accept
  arms *pass*, which they cannot do natively — semget, shmget, tee, ustat,
  vmsplice, sysfs and statmount do not return ENOSYS on this host, so a native
  accept run exits 1. Independently, mutant stderr names the guest path as
  `/tmp/hermit-file-io-3-c`, pid 3, a virtualized pid. Together these also
  exclude the "hermit refuses to launch a guest under host `/tmp`" failure, which
  produces `rc=1` with no guest output and reads as a caught mutant: a guest
  produced its own text on every run here.
- **Unexercised path wearing a non-zero exit.** Checked *first* on the two
  guard-inversion rows, as directed: both declare `int main(void)`, `grep -ci
  usage` returns 0 in both, and neither declares `guest_args`. Checked across the
  slice, it caught the two argument-taking fixtures above.
- **Stale fixture.** All 19 accept-arm sources verified byte-identical to
  `302a1a9c0` by git blob hash, and `302a1a9c0` verified equal to live
  `origin/main` at measurement time. No stale-checkout verdict inversion is
  possible here.

## Reproduction

```bash
CLONE=ignored/qualify-queue-rows-61-79-20260815/hermit   # at 302a1a9c0, clean
EV=ignored/takeover-6179-20260815                        # mutants, binaries, logs
gcc -O2 -o $EV/bin/sysv_sem_enosys.accept $CLONE/tests/c/sysv_sem_enosys.c
sed 's/errno == ENOSYS/errno == EPERM/' $CLONE/tests/c/sysv_sem_enosys.c \
  > $EV/src/sysv_sem_enosys.mutant.c
gcc -O2 -o $EV/bin/sysv_sem_enosys.mutant $EV/src/sysv_sem_enosys.mutant.c
cd $CLONE
./target/debug/hermit run --backend=liteinst --strict --verify --verify-strict \
  -- $EV/bin/sysv_sem_enosys.accept    # rc=0
./target/debug/hermit run --backend=liteinst --strict --verify --verify-strict \
  -- $EV/bin/sysv_sem_enosys.mutant    # rc=1
```

`ignored/takeover-6179-20260815/` holds every mutant source, binary and log. It
is gitignored, so it does not survive host loss; this file is the part that
travels.

## Standing

Nine of nineteen have both arms measured under LiteInst. Ten are blocked by a
present-tense backend limit in two distinguishable shapes. **Nothing was
enabled, no manifest was edited, no product source was modified, and no cell
landed.** These nine become landable when a receipt exists, not before.
