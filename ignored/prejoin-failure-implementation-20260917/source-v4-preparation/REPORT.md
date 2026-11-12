# Reverie source-v4 preparation

Exclusive Reverie source preparation on branch `codex/kvm-proc-fd-identity-20260917`, base `114b309413612fafc2657c74e83811c71aac7b19` (tree `16b49eb8c6e85b6b63ac8d2b43179de663726083`). No build, test listing, native control, VM, guest, network, commit, publication, TaskGraph write, or revision-pin change was performed. Existing source-v2, source-v3-preparation, and all failed/passing execution evidence remain preserved.

## Final corrections

The source-v3 corrections remain: the actual host-owned returned-error path publishes before retirement and uses the worker TID; cached worker and missing-final-status errors publish in the shared production post-worker finisher before consuming hooks and descendant joins; first typed cause and first BackendFailure event are retained in one serialized publication; and terminal state after successful local completion selects failed descendant joining. The publication lock remains limited to the synchronous report/local notification, never RPCs or physical joins. Existing normal errno, status, Start, and hook-order assertions remain intact. The adjacent scratch-hide dual-error limitation remains unchanged.

This version corrects the remaining inherited process identity at the actual shared `KvmBackend::prepare_forked_process` boundary. An existing failure context becomes `for_process(child_pid)` with child pid/tid and the same RunFailure. The None path remains None. The redundant Tool-only fork rebind is removed. Ordinary Host forks and nested Host threads therefore retain their own process identity when the actual host returned-error helper applies `for_thread(worker_tid)`.

The explicitly nondefault `native-test-support` feature and its public ownership/gate/RPC entry remain. The real KvmGuest callback adapter now exposes `ElfExecutor::parent_pid()` from the owned lifecycle, so Detcore can distinguish root and fork child startup. Its clock and instruction methods still panic. Native owner fork construction uses the same `for_process` mapping.

Returned current-directory and fork-preparation errors consume supplied Tool/ThreadState through the shared consuming hook path. Child spawn uses `spawn_owned`; refused spawning and gate/driver returned errors flow through consuming finishing. The owned Drop rescue publishes a fatal result, cancels retained gates, joins owned OS handles, and consumes hooks. Explicit finishing removes owned fields first. The existing LoadedStaticElf fixture retains its original `unwrap` behavior for file/proc snapshot preparation; this seam does not claim panic-safe construction before the owner exists. Caller-supplied host worker closures still require a bound or channel rescue because a Rust OS thread cannot forcibly interrupt an arbitrary closure.

## Controls and boundaries

All 34 source-v3 no-VM selectors are preserved exactly in `selected-tests.json`; none were compiled, listed, or executed. The one additional selector is separate:

`vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity`

It constructs a real backend, stages the existing syscall frame, checks a no-context ordinary fork, then calls the actual shared fork-preparation boundary twice with parking disabled. An owned OS thread returns a typed GuestClock failure through the production host-worker helper. Assertions require pid 3 / tid 4 / host-owned-worker phase before retirement, the same RunFailure, inherited Host ownership, and the exact retained typed cause after join. This control requires KVM construction, snapshots and register ioctls and fails if KVM cannot be constructed; it contains no vCPU run or guest instruction execution. It is not included in the no-VM population and has no execution evidence.

The public native seam continues to establish OS-thread/RPC/gate/post-retirement cleanup only. It does not qualify initialized-VM setup, transport release, guest execution, or deterministic scheduling. The new KVM-construction source control does not change that qualification limit.

## Source preparation outcomes

The first scoped rustfmt attempt returned 1 because its unstable `--file-lines` option lacked `--unstable-features`; stdout, stderr and exact argv are retained. The corrected attempt used the absolute installed nightly formatter, a 15-second timeout, `skip_children=true`, and changed line ranges against the bound base (entire new Rust files). It returned 0 in 0.40 seconds. `git diff --check`, also bounded to 15 seconds, returned 0 in 0.09 seconds. These are source preparation outcomes, not compiler or execution results. No assertion, tolerance, comparator, exemption, skip, or expected-success classification was weakened.

`before-format/` preserves all pre-format source bytes. `source/` contains the final eleven owned files. `candidate.patch` is their complete delta against the base, `source-v3-increment.patch` is the distinct corrected-version delta, and `formatting.patch` isolates formatter changes. `binding.json` records exact current source hashes and preparation outcomes; frozen copies were read back and compared with the live source. This report is implementation evidence, not independent review approval.
