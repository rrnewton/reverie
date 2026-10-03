Background: the proposed KVM epoll_pwait2 increment performs an immediate readiness query over a guest-created epoll containing proven native eventfd registrations. It does not implement blocking waits, temporary signal masks, arbitrary transferred descriptors, or general epoll descriptor replacement. This addendum settles the locking, descriptor-lifecycle and numerical-address decisions left open in the earlier source report.

Target: https://github.com/rrnewton/reverie/commit/c3fe1dc3493f32b5d81a0a9babbae8ba758bf02d . All product reads were immutable Git-object reads from the shared Reverie repository, not the Q slot or compiled artifacts. No source/index/ref edits, build, test, probe or remote mutation occurred. This is an implementable design recommendation, not a code-review approval or runtime proof.

The prior report remains unchanged at /tmp/kvm-epollpwait2-design-c87ygpse/REPORT.md and is preserved remotely at https://github.com/rrnewton/reverie/issues/891#issuecomment-5969348450 . PR540 remains withdrawn as a port recommendation.

## Decision 1: one family-local provenance lock, with no callbacks inside it

Use one `Arc<Mutex<EpollDomain>>` shared by an executor and its descendants, rather than a graph of per-registration/per-epoll locks. Independent root guests receive different domains. The domain records provenance only: it must contain no File, guest memory, entry gate, retained operand, callback or waker. A record's ordinary destruction therefore cannot close a host descriptor or notify the runtime.

Concrete fields, with names illustrative:

- In `LoadedStaticElf` and `FileTableState`: `epoll_domain: Arc<Mutex<EpollDomain>>`, an opaque `poll_table_id: Arc<()>`, and `native_poll_fds: BTreeMap<i32, Arc<NativePollBinding>>`.
- `NativePollBinding`: an immutable description (`NativeEventFd(Arc<()>)` or `Epoll(Arc<()>)`) plus a one-way `live: AtomicBool`. Each logical descriptor insertion/dup gets a fresh binding; all worker snapshots of that same logical entry share it. All changes to `live` occur under the domain mutex. Use an ordinary conservative atomic ordering initially; no relaxed-memory optimization is needed.
- `EpollDomain`: records keyed by opaque epoll-description identity. A record holds a Weak identity owner, Weak originating table identity, a permanent unsafe-history flag, and registrations keyed by originating guest descriptor. Each registration holds Weak binding identity, native eventfd description identity and the host numeric key actually used for the successful ADD. That integer is only a key check after independent description/binding proof, never an identity oracle.
- A pointer-derived private key is possible because the registry's Weak token prevents that allocation from being reused until the record is removed. A checked monotone ID is also possible. Neither is guest-visible. Registry references to epoll owners and logical bindings are weak; records can be pruned when the last owner is gone. Do not use the shared anonymous inode as the key. Do not add hidden host dup keepers or infer last close from Arc counts.

The existing dispatcher at executor.rs:6409–6461 synchronizes the file table, then releases it for nonmutating operations. Leave epoll_ctl and the new wait outside `mutates_file_table` (the list begins at line 2955). New epoll bookkeeping mutates the shared domain, not descriptor allocation. The new wait must not retain the outer file-table or signal transaction guards.

The lock ordering rule is concrete: existing outer file-table/signal locks may precede the new domain lock; guest memory's permission/host-copy locks may also precede it. **Nothing under the domain lock may acquire a file-table lock, signal lock, guest-memory lock, perform a guest copy, retire a File, or invoke alias construction/finish.** Thus the domain lock is a leaf in the lock graph. In particular, read epoll_ctl's event structure and inspect signalfd/capture metadata before taking it; do the native ctl and bookkeeping while holding it, then release it before any other operation. Descriptor mutation hooks invalidate bindings under it and return replaced Files to the existing deferred retirement path.

The new wait uses this exact sequence:

1. Complete input copies and numerical preflight with no domain lock. Retain the executor snapshot's epoll File and opaque description.
2. Briefly lock the domain, establish admission and the registration-count upper bound, save the required alias capacity, then unlock.
3. Construct `UserWriteAlias` with no domain, file-table or signal guard. Its short-copy retirement and construction-failure finalizer may notify synchronously; memory.rs:1958 explicitly warns about this.
4. Re-lock the domain; recheck the same epoll owner/table, every current binding's liveness and identity, unsafe-history flag, and that the current registration bound fits the already-created alias. If it grew beyond capacity, refuse this concurrent-history case with ENOSYS and no host poll. No retry or silent truncation. A count decrease or mask/data modification can be accepted if all current proof conditions still hold.
5. Perform the actual zero-time host call while the domain and alias are held. Capture result/errno. Leave the lexical domain-lock block.
6. Finish the alias after the domain guard is gone. A cleanup failure remains the existing sticky backend failure; no guest continuation or extra copyout follows it. Declare the alias before the inner guard so unwind drops the guard first. The finalizer at memory.rs:566–609 already releases both memory locks before notification.

This serializes a successful wait with binding retirement, epoll_ctl and export invalidation. Close either invalidates first and the wait refuses, or it follows the host wait. It does not rely on a racy live-flag check followed by an unprotected host call. No readiness wait or potentially blocking sendmsg runs under the new lock. Ordinary kernel epoll_ctl bookkeeping can still incur normal kernel latency; this is not a hard wall-time guarantee.

The capacity optimization remains valid only for a proven membership set. After validating the original requested count/range, use host count `max(1, min(requested, current_registration_bound))`. Each epitem contributes at most one event to that scan. Membership cannot change during the call. A proven empty set still calls the kernel with capacity one; it does not fabricate readiness. With one admitted registration per live guest descriptor, the present `GUEST_NOFILE_LIMIT = 1 << 20` gives a derived maximum of 12,582,912 output bytes, not a new arbitrary I/O cap. Aliasing/protection faults remain decided by the kernel stores.

## Decision 2: distinguish logical entry retirement from snapshot destruction

The one-way binding flag is required because stale executor snapshots keep Files and metadata alive after another CLONE_FILES worker closes or replaces the logical entry. Dropping such a snapshot is not another logical close. Conversely, checking only a Weak's liveness would mistakenly admit a logically closed entry retained by a snapshot.

Lifecycle integration points at the target commit:

| Path | Required treatment |
| --- | --- |
| `epoll_create1` / `eventfd2` | On successful insertion, mint the correct description and a fresh binding before authoritative publication. Legacy eventfd already routes through eventfd2. A virtual signalfd's eventfd carrier must never receive NativeEventFd provenance. |
| `LoadedStaticElf::insert_file` (elf.rs:921) | Under the domain, mark any old tracked binding dead and remove its metadata before replacing the logical entry. Keep the replaced File for deferred retirement. The caller attaches fresh provenance only when its actual creation/dup path proves it. Generic inserts default to unknown. |
| `remove_file` (elf.rs:939), `close` (executor.rs:16045) | Invalidate and remove tracked binding under the domain. Retire the File only after guards are released through the existing FileRetirement machinery. |
| Dup/dup2/dup3/F_DUPFD/F_DUPFD_CLOEXEC | Extend `DuplicateFdSource` (executor.rs:9814) and both duplication helpers to carry description identity, but create a new destination binding. Replacing the destination invalidates the old binding. Dup2(old, old) changes nothing. A registration tied to a replaced binding is no longer admitted, even if another alias still keeps the native OFD alive. |
| `FileTableState::prepare_from_elf` / `install` (executor.rs:2835/2881) | Clone/install the same domain, table ID and binding Arcs with the Files. Do not mark bindings dead merely because a private snapshot map is replaced. Existing descriptor-entry tokens still govern host-handle installation; the new map does not replace them. |
| Fork (elf.rs:954; executor.rs:3494) | Share the domain and description identities, but mint a new table ID and fresh bindings for the copied logical table. An inherited epoll retains its original record/table identity, so the child's wait is refused under this initial scope. Parent binding liveness is unaffected by closing a child copy. A ctl through the child must invalidate the shared epoll record before native mutation. |
| CLONE_FILES thread construction (executor.rs:3620) | The current constructor first calls the fork-clone helper, then shares the parent's authoritative file table. Explicitly restore/share the parent's poll table ID and binding Arcs too; otherwise child close would invalidate a private token and leave parent proof stale. Subsequent installs maintain the shared bindings. |
| Exec (elf.rs:1080; executor.rs:4410) | Preserve domain/table identity and surviving binding Arcs. Under the domain mark CLOEXEC bindings dead before they are filtered out; do not invalidate survivors. Continue the existing delayed File retirement. An epoll whose watched CLOEXEC entry disappeared is refused afterward. |
| close_range / rollback paths | Ordinary close_range uses the close hook. Pair-insertion rollback (`remove_inserted_file`, line 10162) and received-rights rollback use remove/close and inherit invalidation. CLOEXEC-only changes defer invalidation until exec. Current CLOSE_RANGE_UNSHARE does not actually split the table in this source; conservatively mark the table's known epolls unsafe when that unsupported history is requested, rather than implying this increment repaired its semantics. |
| `release_files_on_exit` (executor.rs:3964) | Drop this executor's binding map with its private Files; do not globally invalidate entries in a still-shared authoritative table. The last table/snapshot release makes weak binding proofs expire naturally. Replace the detached empty state's poll metadata consistently. No new lock dependency is added to terminal exit. |
| `ReadEndpoint` (executor.rs:7237) | Its temporary remove/reinsert lends the exact same snapshot File. Leave binding metadata and liveness untouched. It is not a descriptor replacement. |
| `open_guest_fd_path` (executor.rs:9381) | This reopens through procfs, not dup. Do not copy NativeEventFd/Epoll provenance onto the result. O_PATH aliases remain unknown. No positive classification from a matching inode/readlink is allowed. |
| Other accepted creators: open/creat, pipes/socketpair, accept, sockets, memfd, timerfd, pidfd | Generic insert clears replaced tracked metadata; new descriptors stay unknown for this increment. `execute_accept`'s insertion phase already holds the authoritative table guard. No fixture-specific labels are attached. |

For every epoll_ctl on a known epoll, hold the domain around host mutation and metadata reconciliation. A successful safe ADD records exactly its binding and native key; safe DEL removes that record; MOD preserves membership. Mark unsafe *before* native mutation for a foreign table, unknown/non-eventfd/captured target, nested epoll, changed/dead binding, or a previously registered binding whose current translated host key differs. The last case covers two workers sharing a logical table but carrying differently numbered host duplicates. A later unexpected native result inconsistent with the proven set also makes it unsafe. Once unsafe, discard detailed admission records if desired but retain the shared marker until the description dies. DEL does not magically clear unsafe history. Existing ctl return values are not relabeled.

SCM_RIGHTS is an explicit escape boundary. In `translate_outgoing_rights` (executor.rs:12587), identify a known epoll from the snapshot metadata and set its shared unsafe marker under the domain **before** the host send can run. Release the domain before proceeding. Select the simple permanent marker: even a later malformed rights element, EAGAIN or failed send leaves that epoll outside new-wait admission. This is an explicitly conservative accepted-history boundary, not a claim that failed send changed Linux's epoll. It avoids an export-in-flight rollback/unwind protocol. Do not change the send's native result. All current successful outgoing descriptor transfer goes through sendmsg and this helper; sendmmsg and pidfd_getfd are not dispatched in this source. Any future exporter must use the same hook before it may coexist with this admission.

`install_received_rights` (executor.rs:12781) never restores positive epoll/eventfd provenance: imported, peeked, truncated or rollback-installed rights remain unknown. Receiving a locally sent epoll does not clear its original record's escape marker. An unknown imported carrier cannot be made safe by dup, fork or a matching inode. These rules also prevent capture metadata loss from becoming positive native-eventfd evidence.

No global watcher graph or strong reference to a File is required. Invalidating a binding under the single domain makes every record's next proof check see it without finding and locking every watching epoll. This is why the family-local leaf lock and separate logical binding are preferable here to per-epoll locks plus watcher lists.

## Decision 3: check the full original numerical range, using existing guest policy

Reuse `validate_guest_iovec_address(address, length)` (executor.rs:7718) for this purpose, despite its current iovec-oriented name. It only checks arithmetic and the established four-level guest user ceiling; it does not require mapping presence or write permission. Do not use `range_is_valid`, a writable-prefix probe or GuestMemory bounds as access_ok.

Concrete preflight within the zero-time/null-mask surface:

```rust
let fd = args[0] as libc::c_int;
let count = args[2] as libc::c_int; // Linux ignores upper register bits.
let host_fd = resolve_snapshot_fd(fd).ok_or(EBADF)?;
const EVENT_BYTES: usize = 12; // Assert x86-64 libc::epoll_event layout in tests.
if count <= 0 || count > i32::MAX / EVENT_BYTES as i32 {
    return EINVAL;
}
let full_len = (count as usize) * EVENT_BYTES;
validate_guest_iovec_address(args[1], full_len)?;
// Only now type/provenance admission and a possibly smaller host operand.
```

The timeout copy/validation and non-null-mask size/copy checks precede that fragment, as described in the first report. Known NativeEventFd used as epfd can give native EINVAL after the numerical prefix. Unknown/imported descriptor types remain ENOSYS, even if the host would have returned EINVAL or accepted an epoll; do not label that refusal as full type/errno parity.

The predicate is `len <= L && address <= L - len`, where `L = (1 << 47) - 4096` is the existing guest policy (executor.rs:88). Linux v6.18's dynamic-size __access_ok computes end, rejects wrap and checks end <= USER_PTR_MAX: https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/arch/x86/include/asm/uaccess_64.h#L100 . The defining initialization is USER_PTR_MAX = TASK_SIZE_MAX at https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/arch/x86/kernel/cpu/common.c#L2586 . Thus no new minus-one adjustment is justified by the older Linux-6.12 zero-read comment in Reverie. Linux's five-level/other address policies are not silently adopted; this increment preserves Reverie's fixed four-level ABI. The retained source files are Git-blob verified. A transient question sent during this audit about a possible off-by-one was resolved by reading this definition; it is not a product finding.

Required boundary controls: `L - 12, count=1` reaches ordinary empty/ready behavior; `L - 11, count=1` returns EFAULT; address above L and wrapping addresses fail; low-word negative/zero/excessive counts fail EINVAL; upper register noise is ignored. Critically, address `L - 12` with requested count four must fail the full-array check even if the epoll has only one registration and the eventual alias could fit one event. Numerically valid unmapped/null/read-only output remains permitted through preflight and produces kernel-dependent zero versus EFAULT according to readiness. Keep the original seven fixture checks, exact full-buffer effects and existing time bounds unchanged.

## Existing tracking and implementation handoff

Read-only GitHub searches (complete results, retained JSON) found the broad producer issue https://github.com/rrnewton/reverie/issues/891 and historical unmerged https://github.com/rrnewton/reverie/pull/536, not another dedicated open producer implementation issue. During this audit root created and read back https://github.com/rrnewton/reverie/issues/905 ; use that dedicated issue and do not file a duplicate.

A separate open consumer defect is https://github.com/rrnewton/hermit/issues/3574 . Its reported recording/replay failure is epoll_pwait/epoll_pwait2 running live in both phases and seeing one event versus zero. Reading the issue body establishes that report, not a fresh measurement or a current-source repair verdict. The producer change must not claim to fix recording/replay merely because an unchanged immediate-readiness cell passes.

Before source qualification, enforce the leaf-lock invariant in code review and add a reentrant cleanup-notification control that can acquire both the domain and file-table locks. Lifecycle controls must exercise shared-worker close/reuse, dup alias, fork-private close, exec filtering, exit with a surviving shared table, SCM failure/success/peek and numeric host-key mismatch. They should distinguish supported exact behavior from explicit ENOSYS with unchanged output; no historical failing lifecycle test becomes a pass by relabeling it as Linux parity. Unknown/imported, unsafe-history, temporary-mask, blocking/null-timeout and foreign-table cases stay refused in this increment.

No new permission or design question remains for the three decisions: use the leaf domain lock, shared logical-binding liveness plus explicit permanent escape invalidation, and the existing numeric-only predicate on the full original array. Implementation and qualification must still demonstrate these properties. This report grants neither approval nor execution authority. All source reads are released on delivery; Q was never read for this task.
