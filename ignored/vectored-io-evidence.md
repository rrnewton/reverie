# KVM aggregate vectored I/O repair evidence

## Exact source state

- Worktree: `/home/newton/work/dev-hermit/worktrees/slots/kvm-vectored-io`
- Branch: `codex/kvm-vectored-io`
- PR538 head / commit parent: `3646ba2c662f65b97e94d39f60852b62610ca5a0`
- Final local repair: `90ad5b98fa897f03e817d74b1fa66e68f1b758fb`
- Tree: `962f40b6e487e82feb56b0da113f772376cba389`
- Tracked changes: `reverie-kvm/src/executor.rs` and
  `reverie-kvm/tests/static_elf.rs`; 2853 insertions, 339 deletions.
- Tracked tree clean; no push, merge, or remote-ref mutation.

Superseded local heads: `34299d1a6da58250b361a00f6b5b8c24e61114a8`
and `bacdecd6630b0916f3db2019afad00b3486c80b9`.

## Final implementation

The common `vectored_io` path serves all six x86-64 calls: `readv`, `writev`,
`preadv`, `pwritev`, `preadv2`, and `pwritev2`. Ordinary calls no longer issue
one scalar host syscall per entry. Each bounded guest vector owns one aligned,
anonymous `MAP_NORESERVE` arena. Its host iovec retains the bounded declared
length; an inaccessible suffix is `PROT_NONE`, and later vectors retain their
own entries/backing. The host therefore decides aggregate eventfd size,
pipe atomicity/partial results, stream behavior, datagram discard, and fault
ordering in one call. Successful reads scatter exactly the returned span;
EFAULT reads copy accessible prefixes back because native Linux can modify them
before reporting the fault. Captured output stages the full bounded write before
append.

Before private staging, every imported entry is address-checked against the
guest ABI. This KVM guest is four-level: bootstrap writes PML4→PDPT→PD tables,
sets CR3 to `PML4_ADDRESS`, never sets CR4.LA57, and deterministic CPUID leaf 7
does not expose LA57. Its limit is therefore `(1<<47)-4096` even on this
five-level host. The check is Linux `access_ok`: `len <= limit` and
`addr <= limit-len`; length zero still requires `addr <= limit`.

Native probes corrected two initially overbroad assumptions. Linux first rejects
any individual `iov_len > SSIZE_MAX`, scanning all entry lengths before any data
address validation; exact SSIZE_MAX remains valid. It then clamps each valid
individual length to `MAX_RW_COUNT=0x7ffff000` for `access_ok`, checks every
original entry with that per-entry length (including entries after the aggregate
is full), and caps aggregate execution separately. Thus an individual
`iov_len=SSIZE_MAX` at low address reaches `/dev/null` and returns MAX_RW_COUNT,
but SSIZE_MAX+1 returns EINVAL and a later noncanonical base still returns
EFAULT after a preceding MAX_RW_COUNT vector. Only then does Reverie apply its
documented 16 MiB private staging cap; it never suppresses validation.

Canonical unmapped and PROT_NONE guest ranges are not pre-rejected. Protected
staging lets the endpoint retain Linux-specific behavior. The virtual-signalfd
dequeue/state path was not edited.

## Native measurements

Raw Linux 7.1 calls established:

- `UINT64_MAX` length 0 or 1: EFAULT for all six calls.
- `iovcnt=0` with iovec pointer `UINT64_MAX`: success 0 for all six.
- Base exactly at TASK_SIZE_MAX, length 0: success; length 1: EFAULT.
- A range ending exactly at TASK_SIZE_MAX succeeds; crossing it is EFAULT.
- Invalid zero-length entries before or after a valid nonzero entry are EFAULT.
- A later invalid entry after a 16 MiB first entry is EFAULT for all six.
- `iov_len=SSIZE_MAX+1` and `SIZE_MAX` return EINVAL for all six calls,
  including with an invalid data base. A later oversized entry gives EINVAL
  precedence over an earlier invalid data base, and eventfd remains unconsumed.
- A low-address `iov_len=SSIZE_MAX` reaches the endpoint: empty-file reads
  return 0 and `/dev/null` writes return 2147479552 (MAX_RW_COUNT).
- A later invalid base remains EFAULT after first-entry lengths of 16 MiB,
  MAX_RW_COUNT-1, MAX_RW_COUNT, and MAX_RW_COUNT+1.
- On this LA57 host, fixed no-replace PROT_NONE mapping at `1<<55` succeeds;
  no-copy native operations accept it. The KVM guest must still reject the same
  numeric range under its four-level ABI.
- Eventfd read into canonical PROT_NONE memory returns EFAULT and consumes;
  read into a pre-rejected noncanonical/guest-high range returns EFAULT without
  consumption.

Earlier endpoint measurements retained by the final tests: eventfd and datagram
reads with an accessible4+fault4 vector modify the prefix and consume/discard;
pipe and stream reads modify the prefix but retain input; all four writes fault
without commit; `/dev/null` accepts crossing8+later4; eventfd inaccessible4+
valid4 retains aggregate size and consumes.

## Direct consumer/type inventory

Direct dispatcher consumers are all six vectored calls above. Host-worker
`readv` is backend-owned under `ThreadOwnership::Host`; regular-file and
standard-stream reads remain Tool-visible; deterministic random input,
captured stdout/stderr, synthetic procfs, and virtual signalfd use their
documented special routing around or inside the common helper.

Translated host descriptions include regular files, memfds, directories,
`O_PATH`/`O_DIRECT`, pipes/FIFOs, AF_UNIX/AF_INET/AF_INET6 stream/datagram/
seqpacket sockets, eventfd, timerfd, epoll, pidfd, ioctl-returned descriptors,
duplicates/replacements, inherited descriptions, SCM_RIGHTS recipients, and
standard streams. Tests directly discriminate regular/empty files, `/dev/null`,
eventfd, pipe, AF_UNIX stream/datagram, direct I/O, captured output, descriptor
replacement, and virtual signalfd ordering.

## Regression evidence

- Unit `vectored_iovec_address_validation_matches_guest_and_native_linux_ordering`
  runs raw-native and mediated matrices across all six calls. It covers SSIZE_MAX+1 and SIZE_MAX EINVAL precedence, max,
  wrap, exact/crossing four-level ceiling, first address above the ceiling,
  zero-length alone/adjacent, count zero, later invalid entries after 16 MiB and
  MAX_RW_COUNT, per-entry clamping, conditional LA57-native behavior, and
  eventfd consume/preserve state.
- Real-KVM `vectored_iovec_address_validation_uses_four_level_guest_abi_on_kvm`
  runs a native mode and a guest mode. All six calls cover `/dev/null`/empty-file
  suppression and regular-file state; readv/current-preadv2 and writev/current-pwritev2 cover eventfd and pipe non-mutation.
  The native high-address arm runs only after a successful safe high mapping;
  the KVM arm always enforces four-level rejection.
- Existing native/KVM shape fixtures still cover eventfd, pipe, stream,
  datagram, later vectors, current-position calls, destination prefix, and
  endpoint state.

## Restored-defect mutations

All mutations were restored before final validation:

1. Disabling the all-entry SSIZE_MAX preflight made the focused unit return
   0 instead of EINVAL and made the mandatory real-KVM fixture exit 30.
   The request reached its endpoint when import should have rejected it.

2. Guest limit 47→56: mandatory KVM fixture failed, exit 99; readv returned 0
   instead of pre-invocation EFAULT.
3. Address validation based on aggregate-remaining length: focused unit failed;
   a later ceiling-crossing range after MAX_RW_COUNT returned 0 instead of
   EFAULT.
4. Zero-length address-check exemption: focused unit failed; a zero-length base
   above the guest ceiling returned 0 instead of EFAULT.
5. Retained earlier mutations also reject scalarization, partial-vector
   truncation, loss of later vectors, loss of EFAULT prefix copyout,
   max-count suffix loss, and captured-output prefix append.

## Final exact-head commands

```text
env CARGO_BUILD_JOBS=1 REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm -- --test-threads=1
217 lib + 3 counter + 2 erestartsys + 44 static_elf + 3 strace + 6 vmcall
= 275 passed; 0 failed; 0 ignored/skipped; 7.09s wall

env CARGO_BUILD_JOBS=1 cargo clippy -p reverie-kvm --all-targets -- -D warnings
passed; 3.08s cargo time, 3.13s wall on final tree

cargo fmt --all -- --check
passed

git diff HEAD --check
git diff 3646ba2c662f65b97e94d39f60852b62610ca5a0..HEAD --check
git show --check --oneline HEAD
all passed
```

Focused final runs: 14/14 vectored units in 3.15s; 3/3 required-KVM vectored
fixtures in 1.73s; new required-KVM address fixture 1/1. No ad-hoc Hermit binary
was invoked.

## Goalpost and residual assessment

No assertion, comparator, label, or KVM gate was weakened. Existing tests that
used `UINT64_MAX,len0` merely as an ignored placeholder now use the valid
four-level ceiling with length zero; the dedicated matrix separately requires
the invalid form to return EFAULT. Native-high comparison is conditional for
portability, but mediated unit and real-KVM rejection are unconditional.

The intentional 16 MiB private staging bound remains, so no-copy endpoints may
report a shorter successful mediated write than native MAX_RW_COUNT. Guarded
arenas consume bounded virtual address space but use `MAP_NORESERVE`. Captured
output remains an in-memory sink without pipe backpressure. The separate
virtual-signalfd state residual and all integration worktrees were untouched.
