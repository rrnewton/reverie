Background: Reverie's KVM backend implements Linux system calls for a guest program. `epoll_pwait2` (x86-64 syscall 441) asks an epoll descriptor for ready registrations with a nanosecond timeout. An eventfd is a kernel counter descriptor that can supply readiness. Host descriptor numbers alone cannot establish guest identity: captured output, virtual signalfd carriers, imported descriptors and exported writers need explicit boundaries.

This change adds only explicit-zero-timeout, null-signal-mask waits over proven guest-created native eventfd registrations. It does not implement blocking waits, temporary masks, general epoll virtualization, or a Hermit replay guarantee.

Issue: https://github.com/rrnewton/reverie/issues/905
Pull request: https://github.com/rrnewton/reverie/pull/907
Final head: 5914bcb59a45038a12400ae30d03ebc0c1382b79
Final base: 36bf964b3a98a0206f2b4d11daf0dd9d9872da22
Tree: e02f3dab6d13e343f99d0d9dc0a9cb3e1d348e5d
Slot: kvm-epoll-pwait2-20261003, generation 1, registered native-kvm-epoll-905-01a0d5c2.

Implementation and qualification are complete. This report is not an independent review approval or a canonical validation receipt. All source reads/writes are released to the parent; no build or test remains running.

## Source and composition

Only five source files changed: `reverie-kvm/src/elf.rs`, `reverie-kvm/src/executor.rs`, `reverie-kvm/tests/static_elf.rs`, `reverie-kvm/tests/support/epoll_pwait2.rs`, and `reverie-kvm/tests/fixtures/epoll_pwait2.c`. The last two are new. No existing assertion, timeout, comparator, test case, signal algorithm, manifest or gate was removed or relaxed.

The initial checkpoint f5973ac9f187a62265dd764f6330facd365f4a2a was on 43fce717f1c7710b7ef7345e4146821422a3aaa6. Replaying onto freshly fetched 36bf produced one `=` in range-diff and an identical whole `reverie-kvm` tree. The new main commit touched only `reverie-ptrace/src/task.rs` and `reverie-ptrace/tests/injected_syscall_group_stop.rs`; Cargo manifests and shared KVM inputs were unchanged. Earlier records retain their original head and source identities. `REBASE.json` records this comparison.

The private branch was pushed with the explicit full SHA refspec and an absence lease. `PRIVATE-PUSH.json` records branch readback at 5914 and unchanged main at 36bf before/after. The exact identity tool output was `[hermit2, unknown-agentname, gpt-6-astra, devbig014, role=impl]`; it reported UNRESOLVED, so the commit preserves that literal output rather than inventing a resolved tag. The actual registered worker is named above.

## Ownership, locks and syscall boundary

- `executor.rs:2043` onward defines positive creator provenance. Eventfd open descriptions carry a one-way SCM_RIGHTS escape bit. Descriptor bindings carry a one-way logical-live bit shared by CLONE_FILES snapshots. Epoll descriptions have family-local records naming their originating table and registrations.
- `executor.rs:2062` stores unsupported history on the shared table itself. `close_range(..., CLOSE_RANGE_UNSHARE)` retains its preexisting result/behavior but taints admission for all current and future epolls. A fork receives a fresh identity with the unsafe bit inherited; CLONE_FILES shares the original identity. Exec preserves it. A later creator or fork cannot erase a known unsupported split.
- `elf.rs:925` and `:950` retire replaced/removed logical bindings under the provenance mutex, then release it before File destruction. The fork helper around `elf.rs:972`, exec retention, shared-table prepare/install, dup/fcntl paths, exit cleanup and generic insertion preserve the stated identity/lifetime boundaries. Unknown imports and virtual signalfd's private carrier do not mint native-eventfd provenance.
- `executor.rs:10891` keeps the existing epoll_ctl return behavior while accounting for successful registrations and conservatively tainting unsafe mutation histories. A changed native fd key cannot silently certify a MOD/DEL against a different registration. Poison or bookkeeping failure is not swallowed into a newly admitted set.
- `executor.rs:13029` invalidates native eventfd export before send. The bit follows all aliases and also applies to future registrations, even when the send subsequently fails with EPIPE. Epoll exports taint their shared proof similarly.
- `executor.rs:11020` implements the new bounded wait. It validates timeout and mask preflight; explicit nonzero/null timeouts and nonnull masks remain ENOSYS. It applies signed low-32-bit fd/count semantics and validates the numerical range for the FULL originally requested event array before reducing the host count to proven membership. Known eventfd-as-epoll yields EINVAL; unproven/unsafe histories yield ENOSYS.
- On this supported Linux >=6.18 contract, `do_epoll_wait` obtains the fd before count/range/type checks. Nine native conflicting-invalid-operand controls bind the ordering on the actual host, Linux 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf. Primary source: https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/eventpoll.c . The retained file and source binding name ep_check_params, do_epoll_wait and ep_send_events.
- Alias construction occurs outside file-table and provenance locks. The proof is rechecked under the leaf provenance lock before the zero-time host syscall. Only kernel copyout through that retained alias occurs while the leaf lock is held. The lock drops before alias finish, callbacks or unwind destruction. This preserves partial mask/data stores, queued events on EFAULT, and one-shot state exactly as the native syscall applies them.
- `executor.rs:657` forwards an existing sticky memory failure as the same terminal SyscallAction failure. A normal native EFAULT remains an errno. Direct/basic and execute_checked tests establish terminal identity and absence of continuation after a cleanup failure. Actual KvmBackend success/fault paths are exercised by the integration matrix; injected alias-cleanup failure in a real VM is not claimed.

The fork snapshot basis is retained verbatim in `FORK-SNAPSHOT-BASIS.md`: execute_checked_inner installs the authoritative table into the invoking executor before preparing the fork action. Both direct and Tool paths later clone that retained coherent snapshot. `fork_child` is not itself a resynchronizing API. A close overlapping the fork can be ordered after that snapshot, and the child's duplicated File legitimately keeps the prior open description alive. A future caller bypassing this preparation would need to establish that prerequisite. This source trace is not a general concurrent-scheduler approval.

## Final qualification at 5914

The named population was declared before execution in `DECLARED-POPULATION-FINAL.json` and checked against complete unique binary inventories. There are 1,111 library tests (1,100 prior + 11 new), 427 static integration tests, and zero ignored tests in either inventory.

| Phase | Actual outcome | Guard wall | Test/build wall where reported |
| --- | --- | ---: | ---: |
| Final focused unit tests | 11/11 passed, 0 ignored | 23.5403 s | 0.54 s libtest |
| Final native/KVM matrix | 6/6 passed, 0 ignored | 33.2745 s | 27.55 s compile + 5.41 s libtest |
| Full KVM library | 1,111/1,111 passed, 0 ignored/filtered | 54.7060 s | 54.29 s libtest |
| Required-KVM refusal control | 1/1 passed, all six added names required | 0.7641 s | 0.26 s libtest |
| KVM Clippy, all targets, -D warnings | exit 0 | 20.3836 s | 20.05 s Cargo |
| Workspace docs, RUSTDOCFLAGS=-Dwarnings | exit 0 | 42.5605 s | 42.25 s Cargo |
| Final cargo fmt --all -- --check | exit 0, no changes/output | 3.5534 s | — |

The documentation build includes a 26.27 s DynamoRIO build in the private target. Cargo's build script printed cache-status warning messages; those are retained. This is a passing rustdoc warnings-denied check, not a claim that the output contained no text labeled warning.

The six integration tests execute 17 native runs and 54 KVM runs: 18 direct, 18 StraceTool with Host thread ownership, and 18 with Tool ownership. The original seven-check fixture runs unchanged in meaning and requires exact `epoll_pwait2 ok=7`. Sixteen ABI/copyout modes compare the full 12,352-byte report/output/timeout arena against native plus independent explicit expected C buffers. An additional captured-output mode checks actual registrations of stdout and a dup alias are refused, with a positive ordinary-eventfd prefix. It is a refusal test, not a native-equivalence claim. Exact Tool callback counts total 31 per ownership mode (62 total), asserted by source on each completed run.

Integration success uses the existing bounded exact-name completion helper. Successful child stdout is consumed and checked rather than printed in outer libtest output; route totals are established by the fixed test loops and their exact named child-completion receipts, not by an independently retained per-route text log. The temporary guest C ELF files are compiled and retired by the existing helper and are not archived; their exact source and the executing Rust test binary digests are retained.

The unit tests cover native error order and full array boundaries; pending guest signal/mask preservation; failed SCM export through all aliases and future registrations; unknown/imported/nested/captured/changed-host-key refusal; fork/CLONE_FILES/exec/dup/close/reuse lifetimes; persistent unshare history; provenance poisoning; and success, EFAULT and terminal cleanup notifications that can reenter both table and provenance locks. The cleanup case verifies the unchanged exact 32-counter alias oracle and retained ownership ledger. A successful SCM receive is not a new runtime test in this patch; unproven insertion and the actual failed-send path are covered, while the generic receive path is source-traced.

## Failed diagnostics and causal controls are retained

- Exact base control: base elf/executor bytes from 43f were temporarily restored with the new integration fixture. `base-build` succeeded (38.0695 s); `base-contract` failed with cargo 101 after 0.6091 s. Native passed; the first direct KVM guest exited 1. Host/Tool were not reached. No unprinted guest check count is claimed. Exact original/base/restored source hashes are in `source-pre-baseline/`.
- `repair-units-001` is a failed harness diagnostic: guard 94, child terminated -15, 85.3445 s. Five cases completed successfully, the cleanup child timed out with 124, the later success/EFAULT test started but did not complete, and three cases were not reached. The new test waker dropped its prior futures Shared subscription inside that subscription's wake while futures-util held its waker mutex. The retained dependency source proves the recursive lock. The repair retains subscriptions until outside callback completion; it changes no assertion, deadline or product lock. The owned process tree was terminated and reaped; this batch is not a pass.
- `repair-units-002` was 9/10, cargo 101 (23.3625 s). The cleanup test saw `Ok(-14)` plus an already-sticky terminal gate, rather than the stronger explicit terminal action required for this new handler. This does not demonstrate real VM continuation. The new441 dispatcher now forwards the existing failure identity without replacing its cause; the test remained strict. `repair-units-003` then passed 10/10, and the later final suite contains the additional unshare test.
- `mutant-zero` forced a successful native wait's reported count to zero. The original contract rejected it, cargo 101 (29.1452 s), first direct guest exit1; later routes not reached.
- `mutant-copyback` redirected new441 to the older temporary-copyback helper. Native scalar-fault mode10 passed; first direct guest exited80 and cargo failed101 (27.8064 s). The remaining scalar mode/routes were not reached. This shows the partial copyout/one-shot contract rejects a superficially successful temporary-buffer implementation.
- `unshare-red` first proved the late-creator bug: old source only tainted epolls already present during an unsupported split. The new regression failed101 (24.6469 s), actual later wait1 versus expected ENOSYS(-38). CLOEXEC-only and existing-epoll controls had completed; the later sibling check had not. The shared-table bit repair then passed, including clean/tainted fork-created epoll controls (23.7393 s).
- `mutant-eventescape` removed eventfd's outgoing-rights escape store. The regression failed101 (23.2687 s), actual ready1 versus ENOSYS(-38). Mutation/restoration packets prove exact source restoration before the final build.

Every phase retains the actual command, raw combined output, real return code, original time bound, source/lock before and after, test binary hashes, CPU/disk measurements and generation-bound owned-child retirement. No unexpected red was reclassified as passing, and no automatic retry-to-green occurred.

## Bounds, identities and remaining limits

All execution was serial under the slot's guard: nice10, eight available CPUs, Cargo jobs8, debug0/incremental0, a private target/cache, fresh admission >=280,000,000,000 bytes and one-second stop checks below265,000,000,000 bytes, and a 128 MiB output bound. Focused tests kept a300 s outer bound, full library1800 s, Clippy/docs1200 s. The original integration helper deadlines were unchanged. Every final phase has empty remaining_descendants; the full-library guard reaped adopted zombies as recorded rather than assuming leader exit proved retirement. No ad-hoc Hermit binary was invoked.

The newly resolved Cargo.lock is frozen at SHA256 f19530a3aa1d79d5771cdf17d36d127922495973ab7cbfc56a726326e8cecee6. Full lock bytes are retained at `resolve-lock/Cargo.lock`. No dependency override or resolution change occurred between baseline, mutants and repair.

Final Rust test ELF hashes: library d2da7943507289ebbd6d8dde3b2795f8dc98f9fd0b1843654bccf12224bcc9e8; static integration 52b73649be37f8e627842b2bd84128820435fb7553f795c9968ca9a5864bf680. `FINAL-SOURCE.json` binds all five source files, tree, lock and actual kernel; `final-inventories/BINARY-INVENTORY.json` retains every discovered name.

Blocking/temporary-mask, unknown/imported/nested/escaped registrations, cross-table old epolls and unsupported table-split histories intentionally remain refused. Existing select/poll/old-epoll behavior is not repaired by this increment. Pending guest signals are neither consumed nor reordered by this immediate handler; runtime delivery policy is unchanged. Numeric guest-user range follows the existing four-level guest policy, not arbitrary host LA57 behavior.

Consumer limitations remain separate: https://github.com/rrnewton/hermit/issues/3574 concerns replay; https://github.com/rrnewton/hermit/issues/3633 records that Hermit's current epoll event-output extent is absent from its generic I/O-buffer logging and generic pointer formatting can expose addresses. Byte-exact producer ABI tests and the original seven fixture assertions do not establish complete consumer output hashing or full deterministic replay.

Goalpost assessment: no existing assertion weakened; no tolerance/exemption/skip/comparator relaxation; no failed run relabeled; no check deleted. New ENOSYS boundaries state the supported increment rather than fabricate Linux success. New mandatory-KVM names strengthen device refusal coverage. Independent full-PR review and any landing/canonical evidence remain the parent's responsibility.
