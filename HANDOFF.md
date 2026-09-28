# KVM fchmodat2 implementation

Status: committed locally; not pushed, published, or merged.

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/kvm-fchmodat2`
- Branch: `codex/kvm-fchmodat2`
- Base: `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`
- Head: `d50dea47054ff98f8b4ecab93fe140649b8802a5`
- Exact patch: `ignored/0001-Support-fchmodat2-in-the-KVM-executor.patch`

## Change

- `reverie-syscalls/src/syscalls.rs`
  - Makes legacy `fchmodat` a three-argument typed syscall.
  - Adds the distinct four-argument typed `fchmodat2` syscall.
  - Tests round-trip retention of all six raw registers while requiring display
    to show exactly the Linux-defined argument count.
- `reverie-kvm/src/executor.rs`
  - Dispatches `SYS_fchmodat2`.
  - Accepts only `AT_SYMLINK_NOFOLLOW` and `AT_EMPTY_PATH`.
  - Reads the guest pathname before validating a relative directory fd, then
    resolves and holds the target before changing its mode.
  - Keeps real procfs targets read-only.
  - Masks the mode to `07777`.
  - Supports an empty pathname through a translated guest descriptor.
  - Preserves Linux symlink behavior, including `EOPNOTSUPP` when the selected
    object is a symlink.
  - Retains the existing held-descriptor fallback for hosts without
    `fchmodat2`, while refusing to follow a held symlink in that fallback.

## Complete source caller set

At Hermit main `8a6a3e996d63655bb4cacba54c4dd4a4c9e9f561`, direct guest source callers are:

1. `backend-parity-c/fchmodat2-flags`: direct syscall 452; currently selected
   for ptrace and not selected for KVM because KVM returned `ENOSYS`.
2. `c-programs/syscall-file-metadata`: direct syscall 452; currently selected
   for ptrace and SaBRe and not selected for KVM. Its deterministic log also
   exposed all six raw registers while `fchmodat2` was untyped.
3. `backend-parity-c/fchmod-bits`: calls legacy `fchmodat`; currently selected
   for ptrace and KVM. It is affected only by correcting the displayed syscall
   from four arguments to the Linux three-argument ABI.

The Hermit consumers reached when its Reverie revision changes are:

- `detcore/src/lib.rs`: classification remains number-based, but its comment
  saying `fchmodat2` is untyped becomes stale.
- `detcore/src/syscalls/helpers.rs`: its directory-fd reporting match has
  `Fchmodat` but not yet `Fchmodat2`.
- `hermit-cli/src/recorder.rs`: its subscription and path-mutation match have
  `fchmodat`/`Fchmodat` but not yet `fchmodat2`/`Fchmodat2`.
- `hermit-cli/src/replayer.rs`: its path-mutation and confined-directory-fd
  matches have `Fchmodat` but not yet `Fchmodat2`.

The Reverie workspace all-target compile covers every enum consumer inside the
Reverie repository. The Hermit consumers above require an explicit update and
compile when Hermit advances its Reverie revision.

## Linux behavior measured before implementation

The native probe on devbig014 returned:

- ordinary path and regular-file `AT_SYMLINK_NOFOLLOW`: success;
- a symlink selected with `AT_SYMLINK_NOFOLLOW`: `EOPNOTSUPP`;
- empty path on a regular `O_PATH` fd with `AT_EMPTY_PATH`, alone or combined
  with `AT_SYMLINK_NOFOLLOW`: success;
- empty path on a symlink `O_PATH` fd: `EOPNOTSUPP`;
- invalid flags plus inaccessible path and bad fd: `EINVAL`;
- valid flags plus inaccessible path and bad fd: `EFAULT`;
- empty path without `AT_EMPTY_PATH` plus bad fd: `ENOENT`;
- empty path with `AT_EMPTY_PATH` plus bad fd: `EBADF`;
- missing path: `ENOENT`.

The probe source and binary are retained under `ignored/` in this slot.

## Passing checks

- Focused typed syscall test: 1 passed.
- Focused KVM executor test: 1 passed.
- `cargo test -p reverie-syscalls`: 24 passed, 0 failed.
- `cargo test -p reverie-kvm --lib`: 204 passed, 0 failed in 0.07 seconds
  after compilation.
- `cargo check --workspace --all-targets`: passed in 41.81 seconds.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed in 4.18
  seconds. Existing C compiler fallthrough warnings from vendored SaBRe code
  were printed but did not produce a Rust lint failure.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed before commit.

## Negative checks

- Temporarily disabling the syscall-452 dispatch made the KVM test fail with
  `-ENOSYS` where success was required.
- Temporarily adding a fifth typed `fchmodat2` argument made the display test
  fail with four separators instead of three.
- Temporarily restoring a fourth typed legacy `fchmodat` argument made the
  exact display assertion expose the otherwise-unused register value.

All three temporary changes were reversed before the final test runs and
commit.

## Limits

- No Hermit guest cell was run from this Reverie-only slot; the two currently
  unselected KVM cells require a reviewed Reverie revision and a Hermit revision
  update before that measurement.
- The older-kernel held-descriptor fallback was not executed because this host
  implements `fchmodat2`; the existing fallback was retained and its symlink
  case was made fail closed.
- Only the x86-64 host build was executed. The syscall number exists in the
  pinned `syscalls` crate on all supported architectures.

There is no unresolved Linux behavior question from the cases in scope.
