# Hermit-on-Narf POC implementation progress

Date: 2026-09-24

## Binding implementation target

Deliver two clean components:

1. An upstreamable Narf change that adds principled first-class interception
   hooks for syscalls and, in later phases, other nondeterministic instructions.
2. A general `reverie-narf` backend for arbitrary Reverie tools and supported
   guests. Shared process-global tool state stays directly in Narf/Reverie's
   shared address space; it is not moved through IPC. Interception is a native
   kernel/backend mechanism, not ptrace emulation, signal machinery, binary
   rewriting, or polling.

The completion gate remains one real x86_64 guest with identical observable
behavior and byte-identical versioned fail-closed canonical INFO logs under
Linux/Hermit ptrace and actual Narf/`reverie-narf`, plus a rejected
negative-control mutation.

## Registered work locations

- Parent progress worktree: slot `narf-poc-progress`, generation 1, branch
  `codex/narf-poc-progress`, rooted at
  `/home/newton/work/dev-hermit/worktrees/slots/narf-poc-progress`.
- Hermit worktree: slot `narf-poc-hermit`, generation 1, branch
  `codex/narf-poc-hermit`, initial revision
  `173929ab38290bd3c5c21fcfda91a58284cd4f61`.
- Reverie worktree: slot `narf-poc-reverie`, generation 1, branch
  `codex/narf-poc-reverie`, initial revision
  `1d92000607542a9effc8c7cb4e87ad024e813b79`.
- Narf checkout (as required, outside `dev-hermit`):
  `/home/newton/temp/agent_checkouts/narf-hermit-poc`, branch
  `codex/reverie-interception-hooks`, upstream base
  `87b7cd1048a9492fccb4339558038c06d862e0b4`.

## Narf syscall-interception milestone

Current local Narf head:
`c3a92bbbe38b59649316f5d39ddd0f61fba690ef`.

Commits:

- `70ed22719f190fb8362b0ff94015aa46d35a2518` — first-class,
  table-owned syscall interception and specification.
- `c3a92bbbe38b59649316f5d39ddd0f61fba690ef` — type-safe callback
  authority, atomic one-shot global publication, unknown-syscall lifecycle
  parity, and stronger executable tests.

The current interface provides an immutable `SyscallInvocation` snapshot,
entry control (`Continue` or `Complete`), a return-only replacement callback,
and an observation-only context-managed callback. Interceptors never receive
the mutable `TrapContext`; only the dispatcher can own register, park,
redirection, and signal state. `try_install_global` publishes null-to-pointer
with compare-and-exchange and returns a rejected table without changing the
active identity. The out-of-crate kernel-test reset deliberately retires
without reclaiming old tables, avoiding use-after-free by concurrent readers.
Known and unknown x86 `syscall` numbers now traverse the same ptrace-entry,
interception, ptrace-exit, signal/timer, and reschedule lifecycle.

## Validation at `c3a92bbbe38b59649316f5d39ddd0f61fba690ef`

- `cargo check -p narf-userspace --tests`: passed. Captured output SHA-256
  `8437eb56a8568ff4b1fd5e7013f8dcd6ae329db7e6dc65a942b84408a626ebc2`.
- `cargo check -p narf-userspace --features
  kernel-test,narf-scheduler/kernel-test`: passed. Output SHA-256
  `72790fb20591ffca05fec7bd68ac5aec42c026d5abcb9340c6aa9f7296339775`.
- `cargo clippy -p narf-userspace --features
  kernel-test,narf-scheduler/kernel-test -- -D warnings`: passed. Output
  SHA-256
  `90187a9c57ce9e96d293582bf1f0fb321122ca55b5458cd5e2599e770a9e32da`.
- `cargo fmt --package narf-userspace --check` and `git diff --check`:
  passed. Workspace-wide formatting remains intentionally unclaimed because
  upstream has unrelated pre-existing drift in `memory/src/tests.rs`.
- `cargo xtask test --arch x86_64 --subsystem userspace/syscall --hw-profile
  minimal`: passed in QEMU with clean kernel exit and no panic markers. The
  executed subsystem cases were:
  `smoke_syscall_interceptor_complete_and_global_publication`,
  `smoke_syscall_interceptor_contract`, and
  `smoke_syscall_from_raw_matches_tables`, all `[ OK ]`. Output SHA-256
  `f793ac65d08c3e687d0cd901925ef424db40466b4ba1755ccc94e258b872aa3c`.

The executable tests prove local dispatch, exact versioned snapshots including
RIP/RSP, native result replacement, immutable context-managed completion,
unknown `InvalidOp`, `Complete` suppressing its native handler, and global
publication retaining the original active table after a rejected replacement.
They do not yet prove a real userspace `syscall` instruction combined with
ptrace and a pending signal, AArch64 runtime parity, the future
`reverie-narf` backend, or cross-kernel canonical trace equivalence.

## Audit state

The first separate adversarial review covered exact head
`70ed22719f190fb8362b0ff94015aa46d35a2518` and returned **CHANGES
REQUESTED**. Its byte-identical report is tracked at
[`audits/narf-syscall-interception-70ed227.md`](audits/narf-syscall-interception-70ed227.md)
(SHA-256
`145c1bf6fb0d0f46cb0b0631341b1e08ee561a16c41813003df242cc118e7d07`).
It identified four substantive gaps: mutable trap-context authority, replaceable
global identity and unsafe reset reclamation, an early-returning unknown syscall
path, and insufficient executable coverage. Head `c3a92bbb...` addresses the
first three and adds the bounded tests listed above. A fresh exact-head review
has been requested; no claim of independent approval is made here.

## Next gates

1. Resolve every substantive finding from the fresh exact-head Narf review.
2. Add the narrow nondeterministic-instruction hook needed by P0, with the same
   first-class ownership and authority discipline.
3. Implement the general `reverie-narf` backend in the registered Reverie
   worktree, then the minimal Hermit integration/tool surface in its registered
   worktree.
4. Run the real guest on both backends, compare fail-closed versioned INFO logs
   byte-for-byte, and show the negative mutation is rejected.
5. Obtain exact-head adversarial and deterministic-scheduling reviews where
   applicable, open the upstream Narf PR for human owner/maintainer review, and
   land only through each repository's supported policy.
