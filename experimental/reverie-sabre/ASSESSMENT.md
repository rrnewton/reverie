# reverie-sabre backend assessment

Status as of 2026-07-24, measured at Reverie commit
`d0bf6cc8dd70c9218853ed992a6a3b63d14ff007`: the backend builds and runs
dynamically linked Linux x86-64 programs through both the native synchronous
`reverie_sabre::Tool` API and the constrained adapter for the shared
`reverie::Tool` API. It remains experimental and is not a deterministic
Detcore backend or a drop-in replacement for `reverie-ptrace`.

## Build and test evidence

The following commands completed on x86-64 Linux with rustc
`1.99.0-nightly (be8e82435 2026-07-11)`, CMake 3.31.8, and GCC 11.5.0:

```sh
cargo build -p reverie-sabre -p reverie-sabre-strace \
  -p riptrace -p riptrace-tool
cmake -S third-party/sabre -B target/sabre -DCMAKE_BUILD_TYPE=Release
cmake --build target/sabre --parallel 4
cargo test -p reverie-sabre -p reverie-sabre-strace \
  -p riptrace -p riptrace-tool -- --test-threads=1
SABRE_BINARY=target/sabre/sabre SABRE_CONFORMANCE_TIMEOUT=60 \
  experimental/reverie-sabre/conformance/run.sh all
```

The focused Cargo run passed 41 `reverie-sabre` tests with zero failures;
the other three selected packages currently contain no tests. The conformance
gate passed `thread_lifecycle` and `signal_forwarding` under both the ptrace
counter example and SaBRe `riptrace`. The SaBRe legs observed 1,126 and 25
syscalls respectively.

The pinned upstream source is
`srg-imperial/SaBRe@05816ee066a7284bee8afd0e73eeb44455b254b4`. Its tests are
custom `smoketests` and `tests` build targets, not CTest tests. They were not
run in this assessment because the host does not provide the `lit` executable;
`ctest` reported no tests and the `smoketests` target stopped with
`lit: command not found`.

No Hermit assurance level is established by these Reverie-only checks. They
exercise runtime compatibility, not deterministic repeat execution.

## Program matrix

The shared-adapter column used `reverie-sabre-strace` with
`REVERIE_SABRE_STRACE_QUIET=1`. The native column used `riptrace --quiet
--summary`. Both used the pinned loader and their debug plugin artifacts.

| Guest or behavior | Shared `reverie::Tool` adapter | Native synchronous tool |
| --- | --- | --- |
| `/bin/true` | exit 0 | exit 0 |
| `/bin/false` | exit 1 propagated | exit 1 propagated |
| `/bin/echo sabre-hello` | exit 0, stdout `sabre-hello` | exit 0, stdout `sabre-hello`, 86 syscalls |
| `/bin/cat` on a two-line file | exit 0, exact contents | exit 0, exact contents |
| `/bin/sh -c 'exec /bin/echo exec-ok'` | exit 0, stdout `exec-ok` | exit 0, stdout `exec-ok` |
| `/bin/sh -c '/bin/echo child-ok; wait'` | exit 0, stdout `child-ok` | exit 0, stdout `child-ok` |
| Executable `#!/bin/sh` script | exit 0, stdout `script-ok` | exit 0, stdout `script-ok` |
| `/usr/bin/python3 -c 'print(6*7)'` (Python 3.9.25) | exit 0, stdout `42` | exit 0, stdout `42`, 747 syscalls |

Without quiet mode, the shared adapter also completed `/bin/echo` and emitted
173 syscall diagnostic lines. This confirms that quiet mode suppresses output,
not interception.

Two loader boundaries were reproduced independently of Reverie's plugin:

- `python3` on this host resolves to Meta's custom `fbpython` with
  `/usr/local/fbcode/platform010/lib/ld.so`. It exited 139 before either
  Reverie tool observed a syscall. The same binary also exited 139 under
  upstream SaBRe's `sbr-id` identity plugin, while `/usr/bin/python3.9` passed.
- A statically linked Go executable aborted with exit 134 in upstream
  `loader.c` at the explicit unsupported-static-ELF assertion. Both Reverie
  front ends fail at the same loader boundary before observing a syscall.

## Backend model

SaBRe loads a plugin into the guest and rewrites syscall instructions in the
main executable, dynamic loader, and selected libraries. The callback is
synchronous and operates on local guest memory. The repository currently has
two tool surfaces:

- The native `reverie_sabre::Tool` API used by `riptrace` supports synchronous
  callbacks and reaches process-global state through a blocking Unix-socket
  RPC service.
- `ReverieAdapter<T>` runs a shared `reverie::Tool` inside the plugin. It keeps
  shared global and per-thread state in process. A handler must finish on its
  first poll; `tail_inject` is the only supported pending future. Other pending
  futures fail closed with `EIO`.

`execve` re-enters the pinned SaBRe loader around the replacement image so the
plugin remains installed. `execveat` returns `ENOSYS`. Forked native-tool
children recreate their tool and RPC transport lazily. Thread observation is
callback-driven, so a thread that never crosses an intercepted boundary is not
represented.

## Current limitations

- Cargo builds the host and plugin artifacts, but the SaBRe loader is an
  opt-in submodule with a separate CMake build.
- The validated envelope is dynamically linked x86-64 Linux ELF programs.
  Static programs are unsupported, and nonstandard dynamic loaders such as the
  tested `fbpython` loader can crash before plugin initialization.
- The shared adapter supports only immediately-ready handlers and
  `tail_inject`; it does not make arbitrary async Reverie tools backend-neutral.
- There is no Detcore scheduler, deterministic time/randomness policy, PMU
  preemption, CPUID emulation, or deterministic signal delivery.
- SaBRe has no ptrace-equivalent tool-facing full-register, remote-memory,
  subscription, timer, or PMU interface. See `CAPABILITIES.md` for the detailed
  event and signal envelope.
- Signal mediation is intentionally incomplete: synchronous faults, precise
  `ucontext_t`, alternate stacks, realtime-signal guarantees, and several
  `sigaction` flags are not reproduced.
- `execveat`, static binaries, non-x86-64 guests, loader distribution, and
  broad clone/vfork/exec stress coverage remain unsupported or unverified.
- The native RPC path is blocking and reserves guest file descriptor 100.

The backend is suitable for experimental low-overhead syscall tracing within
this envelope. It should not be treated as a production isolation boundary or
as evidence of deterministic execution.
