# Reverie

## 0.5.0

Changes since the `v0.4.1` tag, through `5041073ee5204c976aa3fdba76b16035bbf7ef76`.
The backend-specific changes below describe their implemented behavior; they do
not imply that every backend supports every Linux syscall or deterministic
replay mode.

- **Signals under ptrace:** report and deliver realtime signals, preserve each
  queued instance and its `siginfo` across injected-syscall retries, and retain
  their ordering through held-signal callbacks. A guest's own `int3` now reaches
  the guest instead of being swallowed. See the
  [realtime-signal issue](https://github.com/rrnewton/reverie/issues/207) and
  [queue-preservation work](https://github.com/rrnewton/reverie/pull/995).
- **Signal masks and instruction traps:** injected syscalls run under the
  guest's mask. Single-stepping preserves the guest's `SIGTRAP` action and mask;
  trapped `rdtsc` and `cpuid` restore the `SIGSEGV` state Linux changed while
  delivering the trap. See the
  [mask issue](https://github.com/rrnewton/reverie/issues/1003),
  [single-step issue](https://github.com/rrnewton/reverie/issues/1084), and
  [signal-state work](https://github.com/rrnewton/hermit/issues/4485).
- **Signal-aware tools:** tools can inspect, replace, or filter signal metadata,
  observe handler returns and child-exit publication, approve caught deliveries
  against kernel state, and set a stopped thread's blocked-signal mask without
  running guest code. Unreported signals carry their reason. Restart-code
  exposure is explicit through `Guest::inject_restartable`; ordinary injection
  retains its existing contract. See the
  [delivery work](https://github.com/rrnewton/hermit/issues/4485),
  [mask-setting work](https://github.com/rrnewton/hermit/issues/4273), and
  [restart API](https://github.com/rrnewton/reverie/pull/1010).
- **Process identity and guest memory:** a `clone` without `CLONE_THREAD` is
  treated as a process. Tool output writes respect guest page protection at
  every size, and small scalar writes are published all-or-nothing. Canonical
  auxiliary vectors and the vDSO are installed only for a freshly executed
  image. See the [clone fix](https://github.com/rrnewton/reverie/pull/992) and
  [memory-write issue](https://github.com/rrnewton/reverie/issues/1064).
- **KVM address space:** match Linux's initial stack, interpreter and PIE image
  layout; report the executed filename in `AT_EXECFN`; return the old break on
  a failed `brk`; implement `MADV_DONTNEED`, `MAP_FIXED_NOREPLACE`, and shared
  anonymous mappings across `fork`. Large regular-file mappings are streamed
  instead of buffered whole, and large zeroed ranges remain lazily resident.
  See the [layout issue](https://github.com/rrnewton/reverie/issues/960),
  [fixed mapping issue](https://github.com/rrnewton/hermit/issues/4428), and
  [streamed mapping work](https://github.com/rrnewton/reverie/pull/1066).
- **KVM syscall and signal behavior:** preserve temporary masks for
  `rt_sigsuspend` and `pselect6`, queue supplied signal metadata, deliver
  `SIGPIPE` for broken pipes, and deliver CPU divide/invalid-instruction faults
  as `SIGFPE`/`SIGILL`. Support nonblocking inotify, host lock queries, user
  extended attributes, `FIONBIO`, and socket `SO_ERROR`. Time, randomness,
  affinity, scheduling and priority operations use the shared Linux provider;
  process/procfs policy belongs to the Tool. Unsupported signal delivery,
  process-group changes and `flock` report explicit refusals.
- **In-guest runtime:** share syscall dispatch, guarded Tool memory, signal
  control, protected runtime descriptors and launcher ownership across
  in-process backends. Preserve native descriptor state before native lifecycle
  operations. Unsupported thread creation and ordinary exec paths terminate
  with named refusals. The LiteInst preload uses the guest's own glibc without
  build-root library search paths; see the
  [preload compatibility issue](https://github.com/rrnewton/reverie/issues/980).
- **Release packages:** align all workspace versions and internal requirements
  at 0.5.0. Keep the established `reverie-utils` package alias and keep
  `nostd-print` private because its registry name belongs to another project.
  Publish the dependency-free `reverie-loader` support crate so
  `reverie-inguest`'s existing optional launch mechanics resolve from crates.io.
  The DynamoRIO, e9patch and SaBRe implementation crates and their dedicated
  tools remain source-only. `reverie-sabre-stats` contains the shared Rust
  statistics ABI, not the SaBRe third-party runtime. Generated Cargo manifests
  must be synchronized with their internal autocargo source before re-export.

## 0.4.1

- Support stable Rust in the public crate graph and fix libunwind/LZMA linking
  on Ubuntu.
- Provide an explicit ptracer-thread interface for older-kernel operation,
  used by Hermit's ptrace path and qualified on Ubuntu 24.04's stock Linux 6.8.
  The generic Safeptrace interface for sibling-thread waits requires
  `PIDFD_THREAD` support from Linux 6.9 or newer and fails clearly below it.
  Linux 5.15 remains a design target, with no qualification claim yet. See the
  [compatibility follow-up](https://github.com/rrnewton/reverie/issues/943).
- Select Native production waits only from an original retained thread pidfd,
  avoiding the strict explicit interface's repeated target checks on that
  path. Linux 6.8 keeps the strict explicit interface. Managed Native waits
  retain the original controller and unfinished capabilities across callback
  refusals and cancellation; Native memory access keeps its existing generic
  semantics. This does not close the existing numeric ptrace exec/reuse
  limitation in [the lifetime issue](https://github.com/rrnewton/reverie/issues/860).
- Eight SDK controls require physical performance counters. Their `EACCES`
  prerequisite failures in a stock VM are expected and do not block this
  release; they do not establish precise-preemption support.
- The unchanged exec/callback regression intermittently misses its
  three-second raw `ESRCH` deadline (three failures in five stock Linux 6.8
  runs). This is a test gating bug; no plain ptrace failure has been
  demonstrated. See
  [the callback issue](https://github.com/rrnewton/reverie/issues/951).
- Nested KVM qualification is deferred; see
  [the nested KVM issue](https://github.com/rrnewton/reverie/issues/940).

## 0.1.0 (December 1, 2021)

 - Initial release
