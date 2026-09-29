# HOLD record: integration-pr529-pr538-hermit

Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393. This file answers the owner's Google Chat
message `3QSxEZA5oY4` ("drive down the worktree slots to exactly match active agents").

It preserves the retention HOLD and every untracked handoff of `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit` on devbig014,
written at 2026-09-29T17:52:19Z before the slot was removed. The HOLD file itself stays in `worktrees/`.

## Registry row and liveness

- Registry row: generation 1, agent `codex-pr529-pr538-hermit`, owner PID 2372246, boot `b597126a-0485-436b-b809-4c28c423193d`, start_ticks 115376848.
- Current boot is `47cceadc-9970-4ca3-87b0-90121ca7e88f` (the host booted at 2026-09-26T17:55:31Z), so the owner is not live.
- HOLD issuer: The HOLD file names no issuer. It was written at 2026-09-19T00:19:44Z with the same reason text as the HOLDs on integration-pr529-pr538-reverie (00:20:28Z) and scorecard-publish-sept14 (00:25:02Z), so it came from a cleanup sweep on the previous boot, not from the slot's own agent.
- `herdr agent list` at 2026-09-29T17:52:19Z: coord wJ:p1, kvm wJ:p38, (unnamed) wJ:p3A, liteinst wJ:p3B, gchat-iris wJ:p3X, parity-green wJ:p5B, scorecard wJ:p5C, kvm-readzero-main-claude-final wJ:p5F. None of these lanes is named in the HOLD text.

## Why the HOLD condition is met

The HOLD says to release "after these files are preserved elsewhere". Every untracked file in the slot, including the three HANDOFF documents, is in the rescue WIP commit below, and the three HANDOFF documents are also reproduced verbatim at the end of this file. The Reverie pull requests this integration tested are resolved: https://github.com/rrnewton/reverie/pull/529 merged on 2026-09-10 and https://github.com/rrnewton/reverie/pull/538 was closed without merging.

## Rescue refs (checked with `git ls-remote` before removal)

- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/integration-pr529-pr538-hermit--wip at `9c339f581de1caa13e9e2b6e1361b74f96e92ead`: dirty Cargo.lock and all 2,481 untracked files (46 MB integration-evidence/, 3 HANDOFF documents).
- https://github.com/rrnewton/hermit/tree/salvage/integration-pr529-pr538-hermit-root at `6c90880ebebcfff80478b555c05f500d6e307716`: the checkout HEAD.

## HOLD file, verbatim

`/home/newton/work/dev-hermit/worktrees/HOLD.9.devbig014.integration-pr529-pr538-hermit.json` (417 bytes, sha256 `9df94143cd1e2de05da753c1f74ae6961bb8d43ae8c9318b74c18f5c9a4c19cc`):

````json
{
  "held_at": "2026-09-19T00:19:44+00:00",
  "machine": "devbig014",
  "reason": "Holds untracked handoff documents under names the remove guard cannot see (_legacy_handoff_artifact hardcodes HANDOFF.md). Protected pending wrkslots_remove_guards_only. Release only after that guard covers prefixed handoffs, or after these files are preserved elsewhere.",
  "schema": 1,
  "slot": "integration-pr529-pr538-hermit"
}
````

## Untracked handoff files, verbatim

### `HANDOFF.md`

2875 bytes, sha256 `2644339e762f67f04e5267a05a444e945c14b5724439c4f4afedd9452a83faa2`.

````markdown
# Exact ignored fixture: PR529 + approved PR538 repair

This worktree is local-only. Nothing was pushed, published, or merged remotely.

- Path: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit`
- Branch: `codex/integration-pr529-pr538-f8bc-hermit`
- Hermit HEAD: `6c90880ebebcfff80478b555c05f500d6e307716`
- Parent: `f8bc4909ffb84277ebd51817fc173b9aba1f13a2`
- Tree: `6302d87c16b60acc859124e6eb2f17410ff0a264`
- Reverie overlay merge: `d0decf28738521b9ae0a33c4b930c5f3f3d43d27`
  (tree `209cf49e90bb83787865e300fb3c56f3add1813d`)
- Ordered merge parents: PR529
  `c632c111619cb47922a72235b3e1130b91355603`, then approved PR538 repair
  `90ad5b98fa897f03e817d74b1fa66e68f1b758fb`

The exact ignored command was run once, with every spawned Hermit process routed
through `/home/newton/work/dev-hermit/bin/safehermit` via `HERMIT_BIN`:

```text
/usr/bin/time -p env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test preadv2_pwritev2_pipe kvm_current_position_vectored_descriptor_matrix_requires_pr529_and_pr538 -- --ignored --exact --nocapture
```

Build succeeded. Test result: one test run, zero passed, one failed, four
filtered, 18.74 seconds. All ten required descriptor snapshots completed:
readv pipe/socket/eventfd; current-position preadv2 socket/eventfd; writev
pipe/socket/eventfd; and current-position pwritev2 socket/eventfd. The earlier
scalarized-eventfd `EINVAL`/guest-fault defect is gone.

The remaining failure is strict verify-log nondeterminism, not vectored syscall
semantics: the retained logs have 2,579 versus 2,572 messages and first differ
at record 336. Run 1 completes
`futex(0x1b51910,265,4,NULL,NULL,-1)=Ok(0)`; run 2 takes
`Futex wait running immediately because it will fizzle (0 != 4)`, then returns
`EAGAIN`, after which close/clone/thread ordering diverges.

Evidence:

- `integration-evidence/exact-ignored-first-attempt-run1.log`, SHA-256
  `8f6920f6261252679867746100f6ac5df78622adee1c4b5bcb82d0f91b444926`
- `integration-evidence/exact-ignored-first-attempt-run2.log`, SHA-256
  `b44c4e9be79a2d423a0a6516861ce1d0bb867d3343d04982a0fc6dfa3f60a9dd`
- `integration-evidence/exact-ignored-first-attempt-safehermit.log`, SHA-256
  `e2fe1e8b76a5128236c02743276a6bcdd55efc3446d8c8261576d9f0dcbf3f31`
- safehermit run ID `20260910T031609Z-3756082`, binary SHA-256
  `f549d63d19c8ca19b803201ad15791be6d564998623ed062ecb6c7fa169ffab1`,
  untruncated, exit 1

The previous handoff and the first integration against old PR538 head 3646ba2c
remain preserved under `integration-evidence/`. The fixed-base 27-cell evidence
and prepared causal comparison arms are in
`/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-diagnostic/HANDOFF.md`.

Do not remove this worktree until the handoff has been consumed.
````

### `integration-evidence/HANDOFF-6c90880-old-pr538.md`

9612 bytes, sha256 `add3533994dfd961a8ba2493155aa3783fb559f98d7a33e4e34c97da74a20818`.

````markdown
# PR529 + PR538 KVM vectored integration handoff

## Scope and provenance

This is a local-only integration checkout. Nothing was pushed, published, or
merged remotely.

- Hermit slot: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit`
- Registered slot name: `integration-pr529-pr538-hermit`
- Branch: `codex/integration-pr529-pr538-f8bc-hermit`
- Tested Hermit HEAD: `6c90880ebebcfff80478b555c05f500d6e307716`
- Parent (the originally requested production commit):
  `f8bc4909ffb84277ebd51817fc173b9aba1f13a2`
- HEAD tree: `6302d87c16b60acc859124e6eb2f17410ff0a264`
- Hermit binary SHA-256:
  `9e5349b31c9d27574975d9d10fbce1a5b326f6c91cd35a6cd9c08182a7c7cc70`
- Guest matrix SHA-256:
  `92c95bfb33ac753f54030ee41f9e60a29182714fb78d67b188d01618c6afec1c`
- Guest writev fixture SHA-256:
  `fdb075868117422c396fed5d375d4ea70a85085b23d4b39b3b2662881abf66f9`
- Kernel: `Linux 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf x86_64`
- `/dev/kvm` was present and mode `crw-rw-rw-`.

`6c90880e` is a direct child of `f8bc4909`. Its production code is unchanged;
the only changed files are the KVM integration test and its C fixture. The old
runtime capability skip was removed and replaced by an explicit ignored test.
The matrix success assertion remains intact. This was run only with
`--ignored --exact`, as requested.

The paired Reverie checkout is
`/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie`.
Its local merge is `04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c`, with parents in this exact order:

1. https://github.com/rrnewton/reverie/pull/529 head
   `c632c111619cb47922a72235b3e1130b91355603`
2. https://github.com/rrnewton/reverie/pull/538 head
   `3646ba2c662f65b97e94d39f60852b62610ca5a0`

The merge was conflict-free. See the sibling slot's `HANDOFF.md` for the merge
tree and merge transcript.

## Offline dependency overlay and build

`.cargo/config.toml` patches every Reverie package used by Hermit to an
absolute path in the combined Reverie slot. `Cargo.lock` is the corresponding
local mechanical lock update. `agent-utils` was initialized offline at its
exact gitlink `e5074d1026ef92ee8bc9d42d44e9d18625bae225` from the already-present
parent checkout. `cargo tree --offline -p hermit -e normal` showed every used
Reverie package coming from the combined slot; only the unused
`reverie-util` patch emitted an unused-patch warning.

Build command:

```text
cargo test --offline -p hermit --test preadv2_pwritev2_pipe --no-run
```

Result: exit 0 in 39.76 seconds. Cargo compiled the locally patched
`reverie-memory`, `reverie-syscalls`, `reverie-process`, `safeptrace`,
`reverie-core`, `reverie-rpc-transport`, `reverie-kvm`, `reverie-ptrace`,
`reverie-preload`, and `reverie-liteinst`. The only diagnostic was the known
future-incompatibility recursion-depth warning from `reverie-ptrace`.

`.integration-safehermit` is the test binary wrapper. Every ad-hoc Hermit
process below went through `/home/newton/work/dev-hermit/bin/safehermit`.

## Required exact ignored KVM test

Command:

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test preadv2_pwritev2_pipe kvm_current_position_vectored_descriptor_matrix_requires_pr529_and_pr538 -- --ignored --exact --nocapture
```

Result: **failed**, 1 test run, in 0.86 seconds. No assertion was skipped or
weakened. Safehermit run `20260909T231625Z-2997242`, exit 125 because the
Hermit child reported a failed verify run; `truncated=false`.

Exact guest transcript retained in
`/home/newton/work/dev-hermit/ignored/safehermit/20260909T231625Z-2997242/hermit.log`:

```text
:: Run1...
First run errored during --verify, not continuing to a second.
Exit status: exited with code 1
Stdout:
readv-pipe-snapshot-ok
readv-socket-snapshot-ok

Stderr:
readv eventfd snapshot failed: count=-1 errno=22 join=0 release=0 first=0 second=0 poison=0xa5a5a5a5 live-iov=0x3fffeb5c

HERMIT_INTERNAL_FAILURE class=cli-error
Error: First run during --verify exited with code 1
```

Conclusion: the earlier initial pipe/socket `readv` `EFAULT` / guest page
fault has disappeared with the exact PR529+PR538 combination. The first two
matrix cases complete. A distinct eventfd semantic defect then stops the
matrix before its later cases.

## Exact eventfd failure mechanism

The fixture submits a single 8-byte eventfd value as two 4-byte iovecs
(`tests/c/preadv2_pwritev2_pipe.c`, lines 393-425). Native Linux treats that
aggregate `readv` length as 8 and succeeds.

The combined KVM implementation's ordinary `readv` path
(`reverie-kvm/src/executor.rs`, lines 2504-2528) iterates each guest iovec and
calls its scalar `read()` once per segment. The first host eventfd read is
therefore only 4 bytes. Linux eventfd requires an 8-byte read and returns
`EINVAL` (`errno=22`). PR538's new positioned-vectored path does not replace
this legacy ordinary `readv` implementation.

Native control command:

```text
timeout --kill-after=5s 30s ./target/tmp/preadv2-pwritev2-pipe/preadv2_pwritev2_pipe matrix
```

Exit 0. It printed every exact matrix marker, ending in
`vectored-descriptor-matrix-ok`, including
`readv-eventfd-snapshot-ok`, `writev-eventfd-snapshot-ok`,
`preadv2-eventfd-snapshot-ok`, and `pwritev2-eventfd-snapshot-ok`.

## Current-position KVM isolation

Command:

```text
timeout --kill-after=5s 70s /home/newton/work/dev-hermit/bin/safehermit --sh-bin /home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/target/debug/hermit --sh-deadline=60 --log=error run --backend=kvm --strict --panic-on-unsupported-syscalls --base-env=minimal -- /home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/target/tmp/preadv2-pwritev2-pipe/preadv2_pwritev2_pipe record-pipe
```

Exact stdout/result:

```text
safehermit: run_id=20260909T232339Z-3245108
safehermit: binary_sha256=9e5349b31c9d27574975d9d10fbce1a5b326f6c91cd35a6cd9c08182a7c7cc70
preadv2-snapshot-ok
preadv2-iobuf-addresses first=0x3fffeb7d second=0x3fffeb7e poison=0x3fffeb7f
pwritev2-atomic-snapshot-ok
pwritev2-iobuf-addresses first=0x428180 second=0x427980 poison=0x427180
pwritev2-large-ok
preadv2-pwritev2-record-pipe-ok
safehermit: elapsed_secs=0
safehermit: bytes_written=0
safehermit: truncated=false
safehermit: exit_code=0
safehermit: unit_result=success
```

There was no guest stderr. This isolates and confirms that KVM current-position
`preadv2`/`pwritev2` pipe behavior works in the exact combined checkout.

## Ptrace controls

Both commands used `.integration-safehermit`, so all spawned Hermit processes
went through the parent safe wrapper.

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test preadv2_pwritev2_pipe current_position_preadv2_and_pwritev2_match_blocking_pipe_semantics -- --exact --nocapture
```

Exit 0: 1 passed, 0 failed, 2.59 seconds. This includes the fixture's strict
trace and strict verify assertions; none was relaxed.

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test writev_determinism sibling_signal_interrupts_scalar_and_vectored_partial_pipe_writes -- --exact --nocapture
```

Exit 0: 1 passed, 0 failed, 0.82 seconds. This is the strict ptrace
sibling-signal scalar/writev control with its exact partial-progress
assertions intact.

## KVM sibling-signal narrowing

The exact `writev_determinism interrupted-writes` fixture cannot reach its
intended sibling-signal assertions on this exact combination. Safehermit run
`20260909T231857Z-3092456` is bounded and untruncated; its durable log is
`/home/newton/work/dev-hermit/ignored/safehermit/20260909T231857Z-3092456/hermit.log`.

The first blocking mechanism is exact: the fixture probes pipe progress with
`ioctl(FIONREAD)`, while this KVM combination returns `ENOTTY`:

```text
finish syscall #64: ioctl(5, FIONREAD, 0x3fffeabc) = Err(Errno(ENOTTY))
interrupted FIONREAD: Inappropriate ioctl for device
```

The root exits while worker threads are still live. During that abnormal
teardown both worker `writev(6, ..., 2)` calls return `EFAULT`, and one worker
then faults:

```text
finish syscall #8: writev(6, 0x2351e20, 2) = Err(Errno(EFAULT))
finish syscall #9: writev(6, 0x1b50e20, 2) = Err(Errno(EFAULT))
reverie-kvm guest thread 4 tool loop failed: guest exception vector 14 at 0x116ea2000 (CR2=0x116ea2000)
```

The remaining workers prevent clean process completion, so safehermit reaches
its 60-second deadline (exit 124). This does not establish the intended signal
contract because the prerequisite `FIONREAD` capability failed first.

The fixture's `signal-interrupt` mode avoids `FIONREAD`, but the KVM backend
explicitly reports `backend_supports_parked_write_signal_interruption: false`.
Run `20260909T232141Z-3180065` remained in internal writev retry polling and
hit safehermit's 64 MiB output cap after about 7 seconds (`truncated=true`,
exit 125). That is an unsupported parked-write signal delivery boundary, not
an `EFAULT` result from the intended assertion path.

No implementation changes were made, and no extra PR (including any
`FIONREAD` work) was folded into this exact integration.

## Worktree state

Expected local-only changes are `Cargo.lock`, `.cargo/config.toml`,
`.integration-safehermit`, and this `HANDOFF.md`. The registered owner-lease
process was PID 2372246 when this handoff was written. Do not remove this
worktree until the handoff has been consumed.
````

### `integration-evidence/HANDOFF-before-approved-90ad.md`

9612 bytes, sha256 `add3533994dfd961a8ba2493155aa3783fb559f98d7a33e4e34c97da74a20818`.

````markdown
# PR529 + PR538 KVM vectored integration handoff

## Scope and provenance

This is a local-only integration checkout. Nothing was pushed, published, or
merged remotely.

- Hermit slot: `/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit`
- Registered slot name: `integration-pr529-pr538-hermit`
- Branch: `codex/integration-pr529-pr538-f8bc-hermit`
- Tested Hermit HEAD: `6c90880ebebcfff80478b555c05f500d6e307716`
- Parent (the originally requested production commit):
  `f8bc4909ffb84277ebd51817fc173b9aba1f13a2`
- HEAD tree: `6302d87c16b60acc859124e6eb2f17410ff0a264`
- Hermit binary SHA-256:
  `9e5349b31c9d27574975d9d10fbce1a5b326f6c91cd35a6cd9c08182a7c7cc70`
- Guest matrix SHA-256:
  `92c95bfb33ac753f54030ee41f9e60a29182714fb78d67b188d01618c6afec1c`
- Guest writev fixture SHA-256:
  `fdb075868117422c396fed5d375d4ea70a85085b23d4b39b3b2662881abf66f9`
- Kernel: `Linux 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf x86_64`
- `/dev/kvm` was present and mode `crw-rw-rw-`.

`6c90880e` is a direct child of `f8bc4909`. Its production code is unchanged;
the only changed files are the KVM integration test and its C fixture. The old
runtime capability skip was removed and replaced by an explicit ignored test.
The matrix success assertion remains intact. This was run only with
`--ignored --exact`, as requested.

The paired Reverie checkout is
`/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-reverie`.
Its local merge is `04afeec5f92a3b56bae1bc6523b1b45a2d87dc8c`, with parents in this exact order:

1. https://github.com/rrnewton/reverie/pull/529 head
   `c632c111619cb47922a72235b3e1130b91355603`
2. https://github.com/rrnewton/reverie/pull/538 head
   `3646ba2c662f65b97e94d39f60852b62610ca5a0`

The merge was conflict-free. See the sibling slot's `HANDOFF.md` for the merge
tree and merge transcript.

## Offline dependency overlay and build

`.cargo/config.toml` patches every Reverie package used by Hermit to an
absolute path in the combined Reverie slot. `Cargo.lock` is the corresponding
local mechanical lock update. `agent-utils` was initialized offline at its
exact gitlink `e5074d1026ef92ee8bc9d42d44e9d18625bae225` from the already-present
parent checkout. `cargo tree --offline -p hermit -e normal` showed every used
Reverie package coming from the combined slot; only the unused
`reverie-util` patch emitted an unused-patch warning.

Build command:

```text
cargo test --offline -p hermit --test preadv2_pwritev2_pipe --no-run
```

Result: exit 0 in 39.76 seconds. Cargo compiled the locally patched
`reverie-memory`, `reverie-syscalls`, `reverie-process`, `safeptrace`,
`reverie-core`, `reverie-rpc-transport`, `reverie-kvm`, `reverie-ptrace`,
`reverie-preload`, and `reverie-liteinst`. The only diagnostic was the known
future-incompatibility recursion-depth warning from `reverie-ptrace`.

`.integration-safehermit` is the test binary wrapper. Every ad-hoc Hermit
process below went through `/home/newton/work/dev-hermit/bin/safehermit`.

## Required exact ignored KVM test

Command:

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test preadv2_pwritev2_pipe kvm_current_position_vectored_descriptor_matrix_requires_pr529_and_pr538 -- --ignored --exact --nocapture
```

Result: **failed**, 1 test run, in 0.86 seconds. No assertion was skipped or
weakened. Safehermit run `20260909T231625Z-2997242`, exit 125 because the
Hermit child reported a failed verify run; `truncated=false`.

Exact guest transcript retained in
`/home/newton/work/dev-hermit/ignored/safehermit/20260909T231625Z-2997242/hermit.log`:

```text
:: Run1...
First run errored during --verify, not continuing to a second.
Exit status: exited with code 1
Stdout:
readv-pipe-snapshot-ok
readv-socket-snapshot-ok

Stderr:
readv eventfd snapshot failed: count=-1 errno=22 join=0 release=0 first=0 second=0 poison=0xa5a5a5a5 live-iov=0x3fffeb5c

HERMIT_INTERNAL_FAILURE class=cli-error
Error: First run during --verify exited with code 1
```

Conclusion: the earlier initial pipe/socket `readv` `EFAULT` / guest page
fault has disappeared with the exact PR529+PR538 combination. The first two
matrix cases complete. A distinct eventfd semantic defect then stops the
matrix before its later cases.

## Exact eventfd failure mechanism

The fixture submits a single 8-byte eventfd value as two 4-byte iovecs
(`tests/c/preadv2_pwritev2_pipe.c`, lines 393-425). Native Linux treats that
aggregate `readv` length as 8 and succeeds.

The combined KVM implementation's ordinary `readv` path
(`reverie-kvm/src/executor.rs`, lines 2504-2528) iterates each guest iovec and
calls its scalar `read()` once per segment. The first host eventfd read is
therefore only 4 bytes. Linux eventfd requires an 8-byte read and returns
`EINVAL` (`errno=22`). PR538's new positioned-vectored path does not replace
this legacy ordinary `readv` implementation.

Native control command:

```text
timeout --kill-after=5s 30s ./target/tmp/preadv2-pwritev2-pipe/preadv2_pwritev2_pipe matrix
```

Exit 0. It printed every exact matrix marker, ending in
`vectored-descriptor-matrix-ok`, including
`readv-eventfd-snapshot-ok`, `writev-eventfd-snapshot-ok`,
`preadv2-eventfd-snapshot-ok`, and `pwritev2-eventfd-snapshot-ok`.

## Current-position KVM isolation

Command:

```text
timeout --kill-after=5s 70s /home/newton/work/dev-hermit/bin/safehermit --sh-bin /home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/target/debug/hermit --sh-deadline=60 --log=error run --backend=kvm --strict --panic-on-unsupported-syscalls --base-env=minimal -- /home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/target/tmp/preadv2-pwritev2-pipe/preadv2_pwritev2_pipe record-pipe
```

Exact stdout/result:

```text
safehermit: run_id=20260909T232339Z-3245108
safehermit: binary_sha256=9e5349b31c9d27574975d9d10fbce1a5b326f6c91cd35a6cd9c08182a7c7cc70
preadv2-snapshot-ok
preadv2-iobuf-addresses first=0x3fffeb7d second=0x3fffeb7e poison=0x3fffeb7f
pwritev2-atomic-snapshot-ok
pwritev2-iobuf-addresses first=0x428180 second=0x427980 poison=0x427180
pwritev2-large-ok
preadv2-pwritev2-record-pipe-ok
safehermit: elapsed_secs=0
safehermit: bytes_written=0
safehermit: truncated=false
safehermit: exit_code=0
safehermit: unit_result=success
```

There was no guest stderr. This isolates and confirms that KVM current-position
`preadv2`/`pwritev2` pipe behavior works in the exact combined checkout.

## Ptrace controls

Both commands used `.integration-safehermit`, so all spawned Hermit processes
went through the parent safe wrapper.

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test preadv2_pwritev2_pipe current_position_preadv2_and_pwritev2_match_blocking_pipe_semantics -- --exact --nocapture
```

Exit 0: 1 passed, 0 failed, 2.59 seconds. This includes the fixture's strict
trace and strict verify assertions; none was relaxed.

```text
env HERMIT_BIN=/home/newton/work/dev-hermit/worktrees/slots/integration-pr529-pr538-hermit/.integration-safehermit cargo test --offline -p hermit --test writev_determinism sibling_signal_interrupts_scalar_and_vectored_partial_pipe_writes -- --exact --nocapture
```

Exit 0: 1 passed, 0 failed, 0.82 seconds. This is the strict ptrace
sibling-signal scalar/writev control with its exact partial-progress
assertions intact.

## KVM sibling-signal narrowing

The exact `writev_determinism interrupted-writes` fixture cannot reach its
intended sibling-signal assertions on this exact combination. Safehermit run
`20260909T231857Z-3092456` is bounded and untruncated; its durable log is
`/home/newton/work/dev-hermit/ignored/safehermit/20260909T231857Z-3092456/hermit.log`.

The first blocking mechanism is exact: the fixture probes pipe progress with
`ioctl(FIONREAD)`, while this KVM combination returns `ENOTTY`:

```text
finish syscall #64: ioctl(5, FIONREAD, 0x3fffeabc) = Err(Errno(ENOTTY))
interrupted FIONREAD: Inappropriate ioctl for device
```

The root exits while worker threads are still live. During that abnormal
teardown both worker `writev(6, ..., 2)` calls return `EFAULT`, and one worker
then faults:

```text
finish syscall #8: writev(6, 0x2351e20, 2) = Err(Errno(EFAULT))
finish syscall #9: writev(6, 0x1b50e20, 2) = Err(Errno(EFAULT))
reverie-kvm guest thread 4 tool loop failed: guest exception vector 14 at 0x116ea2000 (CR2=0x116ea2000)
```

The remaining workers prevent clean process completion, so safehermit reaches
its 60-second deadline (exit 124). This does not establish the intended signal
contract because the prerequisite `FIONREAD` capability failed first.

The fixture's `signal-interrupt` mode avoids `FIONREAD`, but the KVM backend
explicitly reports `backend_supports_parked_write_signal_interruption: false`.
Run `20260909T232141Z-3180065` remained in internal writev retry polling and
hit safehermit's 64 MiB output cap after about 7 seconds (`truncated=true`,
exit 125). That is an unsupported parked-write signal delivery boundary, not
an `EFAULT` result from the intended assertion path.

No implementation changes were made, and no extra PR (including any
`FIONREAD` work) was folded into this exact integration.

## Worktree state

Expected local-only changes are `Cargo.lock`, `.cargo/config.toml`,
`.integration-safehermit`, and this `HANDOFF.md`. The registered owner-lease
process was PID 2372246 when this handoff was written. Do not remove this
worktree until the handoff has been consumed.
````
