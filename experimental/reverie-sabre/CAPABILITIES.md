# reverie-sabre capabilities

Status as of 2026-07-21: experimental Linux x86-64 backend. The restored
runtime can run dynamically linked programs under the pinned SaBRe loader and
the riptrace demo, but it is not a drop-in replacement for
`reverie-ptrace`.

## Supported runtime behavior

| Area | Current behavior |
| --- | --- |
| Syscalls | Intercepts rewritten syscall instructions and invokes the synchronous in-process `Tool::syscall` callback. The default implementation performs the real syscall. |
| Guest memory | Exposes direct local-process memory through `LocalMemory`; there is no remote memory or register API. |
| Threads | Creates backend records lazily when an intercepted thread is first observed. Start and exit callbacks are emitted at most once for a tracked thread. Repeated pthread create/return/join waves are covered by the conformance gate. |
| Process exit | `exit_group` prevents new thread records, requests exit on tracked threads, and waits for their exit callbacks. Configurable timeout handling is supported. |
| Signals | Central handlers mediate standard catchable signals. Guest `rt_sigaction` registration and query are virtualized so the central handler remains installed. SIGINT, SIGTERM, and SIGCHLD handler delivery is covered by the conformance gate. |
| Signal exclusion | Tool and guest signal callbacks run through the per-thread exclusion sequencer. Its bounded queue coalesces overflow instead of panicking in signal context. |
| Fork and exec | Basic fork/wait and `execve` have smoke coverage. `execve` relaunches the child through SaBRe. |
| Timing and detours | Supports RDTSC callbacks, selected VDSO callbacks, and macro-generated function detours. |
| Global state | Uses a synchronous generated RPC client to a host-side service. |
| Loader inputs | Validated with dynamically linked x86-64 guests and the loader revision in `SABRE_UPSTREAM.toml`. |

SIGCHLD keeps children waitable when its guest disposition is `SIG_DFL`.
Default terminating actions coordinate tracked-thread exit and use the
conventional `128 + signal` process exit code.

## Conformance gate

The gate compiles two native workloads and runs each unchanged under the
ptrace `counter2` example and the SaBRe `riptrace` tool:

- `thread_lifecycle`: 128 pthread create, syscall, return, and join cycles.
- `signal_forwarding`: installs and queries handlers, forks and waits for a
  child, verifies SIGCHLD, SIGINT, and SIGTERM delivery, then resets SIGCHLD to
  `SIG_DFL` and confirms the next child remains waitable.

Build upstream SaBRe at the pinned revision, then run:

```bash
SABRE_BINARY=/path/to/SaBRe/build/sabre \
  experimental/reverie-sabre/conformance/run.sh all
```

The ptrace or SaBRe half can be isolated while diagnosing a failure:

```bash
experimental/reverie-sabre/conformance/run.sh ptrace
SABRE_BINARY=/path/to/sabre \
  experimental/reverie-sabre/conformance/run.sh sabre
```

A passing gate requires both workloads to exit zero on both backends before
the 30-second per-run timeout. Set `SABRE_CONFORMANCE_TIMEOUT` to override
that timeout and `SABRE_PLUGIN` to test a non-default plugin path.

Unit-level runtime checks are:

```bash
cargo test -p reverie-sabre
```

## Known limitations

- The SaBRe backend has a separate synchronous `reverie_sabre::Tool` API.
  Existing async `reverie::Tool` implementations cannot switch backends.
- Thread observation is callback-driven. A native thread that never reaches an
  intercepted runtime boundary has no backend record. Join itself is kernel
  behavior, not a distinct SaBRe tool event.
- Signal mediation is not kernel-exact. Handler masks, `SA_NODEFER`,
  `SA_RESETHAND`, alternate stacks, and the original `ucontext_t` are not
  reproduced. `SA_SIGINFO` handlers receive siginfo but a null context.
- Standard signal overflow may be coalesced at the 64-entry deferred queue.
  Realtime-signal ordering and payload guarantees are not implemented.
- Synchronous fault signals such as SIGILL and SIGSEGV, plus SIGKILL and
  SIGSTOP, are not centrally mediated. The SIGSTKFLT disposition is reserved
  as the runtime's controlled-exit signal.
- Tool callbacks can observe signals but cannot replace, suppress, or redirect
  delivery through a shared backend-neutral contract.
- There is no tool-facing register, stack, remote injection, subscription,
  CPUID, timer, or PMU interface comparable to `reverie-ptrace`.
- `execveat`, static binaries, non-x86-64 guests, loader distribution, and
  broad clone/vfork/exec stress coverage remain unsupported or unverified.
- RPC is blocking, reserves guest file descriptor 100, and injected-process
  formatting may allocate.

This backend is an extension under `experimental/`; it does not change shared
Reverie core abstractions. See `ASSESSMENT.md` for provenance and loader
build details.
