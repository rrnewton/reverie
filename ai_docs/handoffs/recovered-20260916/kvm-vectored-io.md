# HANDOFF: aggregate KVM vectored I/O repair

## Exact result

Completed and amended locally. No push, merge, or remote-ref change occurred.

- Worktree: `/home/newton/work/dev-hermit/worktrees/slots/kvm-vectored-io`
- Branch: `codex/kvm-vectored-io`
- PR538 head / sole parent: `3646ba2c662f65b97e94d39f60852b62610ca5a0`
- Final local commit: `90ad5b98fa897f03e817d74b1fa66e68f1b758fb`
- Commit tree: `962f40b6e487e82feb56b0da113f772376cba389`
- Changed tracked files: `reverie-kvm/src/executor.rs` and
  `reverie-kvm/tests/static_elf.rs`
- Commit diff: 2853 insertions, 339 deletions.
- Tracked tree is clean. This handoff and `ignored/` are intentionally untracked.

The earlier local heads `34299d1a...` and `bacdecd6...` are superseded by this
amended commit.

## Repair

All six vectored calls (`readv`, `writev`, `preadv`, `pwritev`, `preadv2`, and
`pwritev2`) use one aggregate host operation. Each bounded guest vector keeps
one independently guarded host mapping with the same bounded length and order;
partial vectors are not split or truncated at their accessible prefix. This
preserves eventfd, pipe, stream, datagram, regular-file, `/dev/null`, direct-I/O,
descriptor-replacement, captured-output, and copyout behavior.

Iovec import now rejects invalid guest address ranges before invoking an
endpoint. The limit is the guest ABI's fixed four-level
`TASK_SIZE_MAX=(1<<47)-4096`, not the LA57 host process limit: bootstrap installs
a PML4 and never enables CR4.LA57, and deterministic guest CPUID does not expose
LA57. Linux first rejects any individual `iov_len > SSIZE_MAX`, scanning all
entry lengths before any data-base validation. It then clamps each valid length
to `MAX_RW_COUNT=0x7ffff000` for `access_ok`, checks every entry (including
zero-length and later entries), and caps the aggregate. The separate 16 MiB
private staging cap is applied only afterward.

Consequences covered explicitly:

- Any individual length above SSIZE_MAX returns EINVAL before any data-base
  validation, including when another entry has an invalid address.
- `UINT64_MAX` with length 0 or 1 is EFAULT; `iovcnt=0` ignores the iovec pointer.
- Base exactly at the guest limit is valid only for length 0; a range ending
  exactly at the limit is valid, while one crossing it is EFAULT.
- An individually huge vector can be accepted when its first MAX_RW_COUNT bytes
  fit; its unreachable suffix is not range-checked.
- Later entries remain range-checked even after the aggregate reaches
  MAX_RW_COUNT or the private 16 MiB staging cap.
- Canonical unmapped/PROT_NONE guest ranges still reach the endpoint through
  protected staging, retaining endpoint-specific consumption and fault rules.

The virtual-signalfd dequeue/state path was not edited.

## Exact-head validation

```text
env CARGO_BUILD_JOBS=1 REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm -- --test-threads=1
275 passed, 0 failed, 0 ignored/skipped; 7.09s wall
  217 lib, 3 counter, 2 erestartsys, 44 static_elf, 3 strace, 6 vmcall

env CARGO_BUILD_JOBS=1 cargo clippy -p reverie-kvm --all-targets -- -D warnings
passed; 3.08s cargo time, 3.13s wall on the final tree

cargo fmt --all -- --check
passed

git diff HEAD --check
git diff 3646ba2c662f65b97e94d39f60852b62610ca5a0..HEAD --check
git show --check --oneline HEAD
all passed
```

Focused evidence was also green: 14/14 vectored unit tests (3.15s), 3/3
required-KVM vectored fixtures (1.73s), and the new exact real-KVM fixture alone
(3.43s for the repaired boundary run). `REVERIE_REQUIRE_KVM=1` makes missing KVM a
failure rather than a skip.

## Mutation evidence

Each mutation was applied, observed to fail, and restored byte-for-byte:

- Disabling the all-entry SSIZE_MAX preflight made the focused unit return 0
  instead of EINVAL and made the real-KVM fixture exit 30; both endpoints were
  reached when the request should have been rejected.
- Replacing the guest 47-bit limit with the host 56-bit limit made the mandatory
  KVM fixture exit 99: readv returned 0 where pre-invocation EFAULT was required.
- Validating only aggregate-remaining length erased a later invalid range after
  MAX_RW_COUNT; the unit observed mediated readv 0 versus required EFAULT.
- Exempting zero-length vectors let the first non-four-level address through;
  the unit observed mediated readv 0 versus required EFAULT.
- Earlier aggregate-shape mutations (scalarization, partial-vector truncation,
  dropping later entries, disabled EFAULT copyout, max-count suffix loss, and
  captured-prefix append) remain rejected by the retained endpoint fixtures.

## Residuals

No assertion, comparator, label, or KVM gate was weakened. Native high-address
checks are conditional only on successfully reserving a non-destructive
five-level `MAP_FIXED_NOREPLACE|PROT_NONE` page; the mediated unit and real-KVM
arms always require the four-level rejection. The intentional 16 MiB private
data cap remains and can make a successful no-copy write shorter than native's
MAX_RW_COUNT result. No scheduler or virtual-signalfd state was changed.

Detailed measurements and command evidence are in
`ignored/vectored-io-evidence.md`.
