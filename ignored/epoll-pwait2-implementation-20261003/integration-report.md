Background: syscall 441, epoll_pwait2, asks an epoll descriptor for ready events using a timespec timeout. This increment supports immediate queries over proven native eventfd registrations. The integration tests exercise Linux's output stores and event-consumption rules through the real KVM executor and through both StraceTool ownership routes.

Source-only integration implementation in registered slot kvm-epoll-pwait2-20261003 generation 1, branch codex/kvm-epoll-pwait2-20261003, base https://github.com/rrnewton/reverie/commit/43fce717f1c7710b7ef7345e4146821422a3aaa6 . No build, test, probe, network operation, commit, push, slot lifecycle operation, or production-source edit was performed by this worker. Rustfmt was run only on the new owned support module. This report is an implementation handoff, not a review verdict or execution receipt.

## Written paths

- reverie-kvm/tests/fixtures/epoll_pwait2.c: one guest program with the original seven-check contract, 16 native-paired ABI/copyout modes, and one explicit captured-output refusal mode.
- reverie-kvm/tests/support/epoll_pwait2.rs: six bounded real-KVM tests, comparing direct capture, StraceTool with Host ownership, and StraceTool with Tool ownership. Each tool route requires the exact syscall-441 callback count.
- reverie-kvm/tests/static_elf.rs: only one module declaration and six additions to the existing required-KVM denial control; existing tests and helper code are unchanged.

The parent owns concurrent changes in elf.rs and executor.rs. This worker did not edit those paths. The assigned shorthand tests/fixtures was resolved to the established product directory reverie-kvm/tests/fixtures.

## Exact test names and case counts

| Test | Fixture modes | KVM executions if all prior checks pass |
| --- | --- | --- |
| epoll_pwait2::seven_check_contract_matches_native | contract | 3 |
| epoll_pwait2::empty_and_ready_eventfd_match_native | 0, 1 | 6 |
| epoll_pwait2::raw_abi_and_readonly_timeout_match_native | 2, 3, 12, 13, 14, 15 | 18 |
| epoll_pwait2::inaccessible_output_depends_on_readiness | 4, 5, 6, 7, 8, 9 | 18 |
| epoll_pwait2::scalar_faults_preserve_bytes_and_oneshot_events | 10, 11 | 6 |
| epoll_pwait2::captured_output_and_dup_registrations_are_refused | captured | 3 |

That is 17 native executions (contract plus 16 numbered modes), and 54 KVM executions across the six tests. These are static planned counts, not measurements. The four mode-based tests emit and compare a complete 64-byte report, 8,192-byte output arena, and 4,096-byte readonly timeout page (12,352 bytes total per mode). The guest separately checks every arena byte against explicitly constructed expected bytes. No comparator masks, sorting, alternative errno acceptance, retries until success, or timing tolerance were added.

All six names are registered in repair_prctl_required_kvm_is_not_optional. All six call the unchanged leader_self_exec_bounded helper, retaining its 30-second timeout, 2-second kill allowance, exact `ok <test>\n` libtest execution record and required-device failure behavior. No new execution harness was introduced.

## Semantic coverage

The contract preserves all seven conditions and the exact line `epoll_pwait2 ok=7\n` from https://github.com/rrnewton/hermit/blob/0f028322361f7357203ed4ef841dc63a770a2180/tests/c/epoll_pwait2_readiness.c : ADD succeeds, eight-byte eventfd write succeeds, first wait returns one, returned data.fd equals the eventfd, EPOLLIN is present, DEL succeeds, and the second wait returns zero. The original Hermit file was only read, not changed.

Numbered modes:

- 0/1: empty/ready native eventfd, with one-shot interest and a readonly zero timespec.
- 2: high-register noise on both epfd and maxevents, preserving their signed low 32-bit values.
- 3: null mask with UINT64_MAX sigsetsize.
- 4/5: empty/ready epoll with NULL output.
- 6/7: empty/ready epoll with read-only output.
- 8/9: empty/ready epoll with PROT_NONE output.
- 10: the four-byte event-mask store succeeds at the end of a writable page; the following eight-byte data store faults. Only those four bytes change.
- 11: two separately created eventfds are armed by sequential writes. The first event is copied completely, then only the second event's mask is stored before its data faults. The syscall returns one, and the failed second event remains queued. No sorting is permitted.
- 12/13: zero/negative low maxevents with inaccessible output return EINVAL despite nonzero upper bits.
- 14: invalid epfd precedes zero maxevents and returns EBADF.
- 15: a proven native eventfd used as epfd returns EINVAL.

Every faulted ready case (5, 7, 9, 10, 11) immediately retries into valid output and requires exactly one event, then requires zero from the next call. This distinguishes preserved one-shot state from a consumed/disarmed event lost by temporary-buffer copyback. Retry output and untouched tail bytes remain in the full arena comparison. Both scalar-fault layouts place the data field wholly on the inaccessible page; no unmeasured assumption about an eight-byte scalar store straddling a boundary was added.

The captured mode first requires a successful eventfd wait in the same captured session. It then separately registers captured stdout and dup(stdout) with EPOLLOUT and requires ENOSYS plus a byte-identical 64-byte output array. Each epoll_ctl ADD must return zero; EPERM is a failure, not accepted coverage. The bounded helper launches its own libtest child through Command::output(), so host stdout is a pipe rather than a regular-file carrier. No host descriptor table is changed by this fixture; the duplicate is guest-local. This is an explicit unsupported-boundary test, not a claim of native parity for capture vectors.

## Expected base and mutant failures — not executed

The unimplemented base returns ENOSYS for syscall 441. The contract should print ok=3 and exit 1; each numbered-mode test should reject its first call's expected non-ENOSYS result. The captured-refusal test must also fail the base at its positive eventfd prefix, preventing a blanket-ENOSYS implementation from passing that test.

A forced-zero dispatch should print ok=4 and fail the contract. A host-wait-into-temporary-buffer followed by guest copyback should fail the scalar/one-shot cases when it consumes an event before discovering the guest output fault. A blind forward of captured registrations should fail the refusal assertion after successful ADD. These are source predictions; qualification must execute the real implementation and any required controls.

## Remaining qualification and scope

No runtime or compilation claim is made. Parent still owns production qualification, unit-level lifecycle/provenance/SCM/fork/exec/close-reuse coverage, unsupported mask/timeout boundaries, numeric full-range edge cases, resource-failure controls, and exact-head reviews. The integration tests do not repair or claim to fix the separate consumer recording/replay defect https://github.com/rrnewton/hermit/issues/3574 . Producer work belongs to https://github.com/rrnewton/reverie/issues/905 .

No original assertion, timeout, optional-device behavior, comparison, or manifest row was weakened. The original pselect files and all preexisting integration tests remain unchanged.
