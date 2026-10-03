Background: Reverie's KVM syscall441 implementation translates a guest epoll descriptor and lets the host kernel copy events through a retained writable alias. Its intended comparison is exact KVM-versus-native behavior on the same execution platform. Linux changed the order of conflicting-invalid-argument errors. A fixed newer-kernel errno is therefore neither a portable native oracle nor authority to raise the project's host requirement.

Target: https://github.com/rrnewton/reverie/pull/907 at https://github.com/rrnewton/reverie/commit/5914bcb59a45038a12400ae30d03ebc0c1382b79 . Source-only repair design; no source/index/ref mutation, build, probe, test or independent approval. The completed Claude report was read only as the finding to resolve; the ongoing Codex report was not read or contacted. Current code-review and GitHub proxy skills were read. Ordered paper/scheduler/vision grounding is reused from the completed prior task; this correction introduces no scheduler change.

Recommendation
==============

Use **guaranteed-invalid host-fd delegation only when the guest descriptor is absent**, rather than a process-global cached kernel-order classifier. The host call always uses fd=-1, explicit zero timeout, NULL mask and size0. Its output pointer is a non-dereferenced representative of the FULL original guest numeric-range validity. This yields the current host's actual validation precedence without examining release strings, retaining an ABI cache, changing native/KVM equality, or touching an epoll/event object.

Verified kernel facts
=====================

Primary files were retrieved read-only through approved `with-proxy gh`, decoded and verified against the Git blob hashes returned by GitHub. Full files and commit/blob/SHA256 bindings are retained in primary/ and LINUX-SOURCES.json.

- Linux6.1: https://github.com/torvalds/linux/blob/830b3c68c1fb1e9176028d02ef86f3cf76aa2476/fs/eventpoll.c#L2221 . Lines2229–2239 check maxevents, then access_ok of maxevents*sizeof(epoll_event), then fdget. Badfd+count0 gives EINVAL; badfd+count1+noncanonical pointer gives EFAULT. Type validation follows at2246. Timeout copying/validation precedes do_epoll_pwait at2308; signal-mask handling precedes do_epoll_wait at2276.
- Linux6.6: https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/fs/eventpoll.c#L2283 . Lines2291–2301 have the same count/range/fd order, followed by type validation2308. Timeout and mask prefixes are at2370 and2338.
- Linux6.18: https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/eventpoll.c#L2451 . Lines2458–2463 fdget first, then ep_check_params. The latter at2406 checks count, range, then type. Thus the two badfd combinations return EBADF. Timeout and mask prefixes are at2521 and2489.

All three primary sources reject invalid fd before ep_poll is reached. None can deliver an event through fd=-1. A NULL mask causes no requested signal-mask installation; the local timeout is input only. No version-transition commit or global minimum kernel is inferred from these three samples.

Precise production delta
========================

The frozen handler is https://github.com/rrnewton/reverie/blob/5914bcb59a45038a12400ae30d03ebc0c1382b79/reverie-kvm/src/executor.rs#L11020 . Keep its timeout/mask validation and explicit zero-time/unmasked admission unchanged (11021–11045). Keep all provenance, escaped-history, current/future-table, alias, terminal-failure, copyout and notification behavior unchanged.

1. Decode both fd and maxevents from the signed low32 register word. When `host_fd(state, guest_fd)` succeeds, keep the existing count/full-original-range/type/provenance order. That ordering is the same on all three kernels once fd existence has succeeded. Do not add a call or change behavior on the successful valid-fd path.
2. When the guest fd is absent, use a small raw-call helper:
   - Always host fd **-1**, never the guest numeric descriptor. A closed guest fd1 must not address the supervisor's live fd1.
   - Preserve the decoded signed count, including0, negative values and values larger than EP_MAX_EVENTS. Do not clamp to membership, reject locally, or substitute count1.
   - For a positive count compute the original count times12 and call the existing numerical guest-range validator on the original guest address and that full length. This computation is independent of known membership and mapped/writable pages. Its maximum is (2^31−1)*12 = **25,769,803,764** bytes, below x86-64 usize/u64 limits. A checked multiplication can make the invariant explicit. Even an oversized positive count remains unchanged in the raw call; legacy Linux must still choose count EINVAL before access_ok.
   - If that full numerical range is valid, pass NULL as the representative host output pointer; otherwise pass usize::MAX. For nonpositive counts pass NULL: the legacy count check necessarily decides before the output pointer, and newer kernels decide fd first. There is no meaningful negative-sized array to validate.
   - Pass a valid local {0,0} timespec, NULL sigmask and sigsetsize0. The guest's NULL mask size remains semantically ignored; no guest address is exposed as a host output pointer.
   - If raw result==-1, capture errno immediately and return exactly that negative errno. ENOSYS, EPERM or EACCES are retained as actual host refusal, not classified as either supported order or relabelled successful readiness. An unexpected nonnegative result is impossible in these kernel sources: fail closed with ENOSYS, never fabricate success. Tests must plant0 and a positive result to verify that boundary.
3. Existing numeric guest policy stays fixed: executor.rs7869 validates against the established four-level `X86_64_GUEST_USER_LIMIT`, and the full-array boundary tests remain exact. NULL/usize::MAX projection preserves this policy even on a host with a wider user-address space. It must not replace the guest limit with host TASK_SIZE or merely test the shortened alias range. The same executor already uses a related safe numeric-address projection in zero_read_host_address at7087–7100; that is a code precedent, not a substitute for this proof.

Why the projection preserves the errors
======================================

For absent guest fd, only these two orders are relevant in the verified sources:

| Original arguments | Linux6.1/6.6 | Linux6.18 | Projected host operands |
|---|---|---|---|
| count<=0 or count>EP_MAX_EVENTS | EINVAL | EBADF | same signed count; NULL or invalid representative is immaterial |
| legal positive count, invalid full guest numeric range | EFAULT | EBADF | fd=-1, same count, usize::MAX |
| legal positive count, valid full guest numeric range | EBADF | EBADF | fd=-1, same count, NULL |

The helper does not choose or normalize an errno. The actual host kernel does. The representatives preserve the only relevant property of the pointer, numerical access_ok, because the invalid fd prevents any access to pointed-to storage. Ordinary readability/writability cannot matter on this path. No descriptor is allocated, closed, polled or modified; no readiness/one-shot state can change; neither guest nor host output memory is written; no guest pending signal is consumed or reordered. Like every syscall, policy auditing and ordinary asynchronous host signal delivery are outside a claim of literal zero process side effects. A raw syscall intercepted by a changing per-thread seccomp/user-notification policy is not a universal Linux-kernel ABI theorem; this design avoids caching such a response and preserves actual host denial.

Unsupported-host limit: this helper propagates ENOSYS if the host does not provide441 and never turns an unfamiliar result into success. The existing valid-call forwarding likewise cannot deliver events on such a host. This proposal does not claim all existing preflight errno branches emulate a host where the entire syscall is unavailable, and adds no test skip or minimum-version policy. Exact same-run native controls must not be reported green if host support is absent or policy denies the test.

Why not cache a probe
=====================

A read-only `epoll_pwait2(-1,NULL,0,&zero,NULL,0)` **can** distinguish the two verified implementations: EINVAL means count-first; EBADF means fd-first. Both return before type/readiness/copyout. It could initialize an enum with an explicit Unsupported case. However, a global OnceLock would add a cache-lifetime assumption about the invoking environment, require an unsupported-initialization policy, and still infer full ordering from one observed combination. Delegating only the actual absent-fd branch is smaller, has no syscall overhead for valid waits, and directly obtains the error in the current host context. There is no need for uname parsing, a host-version allowlist, retries or a new minimum kernel.

Test repair without a comparator exemption
==========================================

No cases, assertions about guest bytes, deadlines, callback requirements or completion/device guards are removed. The fixed expectation was wrong for a supported alternative Linux ABI; repair production and select one exact native-context expectation rather than allowing the guest to choose between errors.

A. Unit native matrix (existing executor.rs37495): retain every row and the exact `KVM == native` assertion. Independently record a host-only invalidfd/count0 control to classify the observed native context. Require the control to return -1 and exactly one known primary-source order; unknown/denied/unsupported outcomes are a failed qualification, not a skip. Bind the expected native matrix to that recorded order: the first two rows are exactly (EINVAL,EFAULT) on legacy and exactly (EBADF,EBADF) on newer hosts; every other expected row is unchanged. Keep full-range boundary checks, low32 cases and pending/refusal controls. These context-dependent constants must not be derived from the KVM result or its new helper.

B. Add bounded unit controls for the new raw invalid-fd boundary. Capture its exact operands using a test-only injected raw-call seam; plant both historical kernel-order outcomes, unsupported/denied errno and impossible nonnegative results. Cover nonpositive and oversized signed-low32 count, full-range crossing versus shortened membership, high register noise, valid/inaccessible-but-numerically-valid pointers, and the closed-guest-fd/ambient-host-number case without changing host stdio. Assert no provenance lock, alias construction, host object polling or guest memory/signal changes. Keep actual native tests in A: injected controls supplement them rather than replacing native evidence.

C. Integration mode14 currently hardcodes EBADF in both C and Rust. Keep mode14, its one guest syscall, the complete64-byte report, full8192-byte arena,4096-byte read-only timeout, and native/direct/Host/Tool equality. Let the Rust host harness measure the independent native control once and pass **one exact expected errno** as an explicit fixture input (for example a strict `14:<errno>` argument). Pass identical bytes to native and all three KVM paths. C must require the tested result/error to equal that exact input; the Rust report assertion must require the same exact value. The guest must never run its own calibration to choose the expected error, which could make a broken guest self-certify. Preserve all other modes and expected bytes unchanged. Record this added host-only calibration separately; it is not an extra guest441 call or a new readiness pass.

D. Causal controls after implementation: replacing invalid-fd delegation with constant EBADF must fail the planted legacy controls; constant EINVAL must fail current-order controls; returning0 for unexpected raw success must fail its refusal test; projecting every address as NULL must fail the full-range legacy control. Restore exact source bytes between mutants. Actual current-host native equality and all prior full-arena/one-shot regressions remain mandatory. A successful forced legacy seam is evidence of adapter behavior, not a claim that a real6.1/6.6 kernel was executed; obtain real older-host evidence if available and label absence honestly.

The change accepts different host ABI inputs while requiring exact agreement between backends under each input. It does not promise bitwise equality across different kernel ABIs. Existing host-native syscall forwarding already makes that environmental distinction; the fix must state it plainly rather than claim fixed newer-kernel errno improves the same-host parity contract.

Disposition and release
=======================

Concrete F1 repair route established; implementation is still forbidden until both ongoing immutable-head readers have released. No runtime result or approval is claimed. No compare exemption, accept-either guest result, case skip, tolerance, changed timeout, lower count, or hidden support-policy change is proposed. All source reads released with this report.
