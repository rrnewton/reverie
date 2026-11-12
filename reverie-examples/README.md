# Examples

Example tools built on top of Reverie.

Copying one of these examples is the recommended way to get started using
Reverie.

# chrome-trace: Generates a chrome trace file

This tool is like `strace`, but generates a trace file that can be loaded in
`chrome://tracing/`.

The public DBT command runs the shared ChromeTrace Tool and writes the returned
GlobalState to `--out`. Produce its caller for the command's Cargo profile:

```bash
reverie-dbt/scripts/build-client.sh \
  --caller-manifest reverie-examples/chrome-trace-dbt-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
printf 'hello\n' | target/debug/chrome_trace --backend dbt --out trace.json -- /bin/cat
```

Ptrace remains the default. `--dbt-artifact` or `REVERIE_DBT_CALLER_ARTIFACT`
selects a verified caller manifest; an invalid selection fails without fallback.
The shared Counter1, Counter2, Strace, minimal Strace, ChromeTrace and ChunkyPrint
public DBT caller launcher inherits stdin, stdout and stderr. Guest and controller writes
reach those streams when issued instead of being replayed after completion.
The separate captured-output APIs keep their capture contract. Signal
termination remains an error, and the caller's existing lifecycle and private
RPC environment limits remain.

The separate `HERMIT_DBT_CHROME_TRACE` prototype remains an unconverted
implementation with its existing script consumers. It does not use this
shared Tool's completed-syscall event path.

# counter1: Reverie Counter Tool (1)

This is a basic example of event counting. It counts the number of system
calls and reports that single integer at exit.

This version of tool uses a single, centralized piece of global state.

# counter2: Reverie Counter Tool (2)

This is a basic example of event counting. This tool counts the number of
system calls and reports that single integer at exit.

This implementation of the tool uses a *distributed* notion of state,
maintaining a per-thread, per-process, and global state. Basically, this is
an example of "MapReduce" style tracing of a process tree.

The default thread report goes through the Tool's GlobalState, so a coordinated
caller can report even after the guest closes stderr. The process contributes
its totals once; a thread report does not add them again.

The public DBT command uses the same CounterLocal Tool in an explicit caller
artifact. Produce it for the same Cargo profile as the command:

```bash
reverie-dbt/scripts/build-client.sh \
  --caller-manifest reverie-examples/counter2-dbt-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/counter2 --backend dbt -- /bin/echo hello
```

`--dbt-artifact` or `REVERIE_DBT_CALLER_ARTIFACT` selects a different verified
manifest. An invalid override fails without fallback. Ptrace remains the
default backend. This DBT caller retains its existing initial-process/thread
limits. The separate legacy exact frontend and benchmark still use their
explicit runtime reporter; they have not been migrated to this caller.

# noop: Identity Function Tool

This instrumentation tool intercepts events but does nothing with them. It is
useful for observing the overhead of interception, and as a starting point.

# chunky_print: Print-gating Tool

This example tool intercepts write events on stdout and stderr and
manipulates either when those outputs are released, or the scheduling order
that determines the order of printed output.

The public DBT command runs the actual shared ChunkyPrint Tool and returns its
GlobalState without changing its epoch policy or final flush. Produce the caller
for the command's Cargo profile:

```bash
reverie-dbt/scripts/build-client.sh \
  --caller-manifest reverie-examples/chunky-print-dbt-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/chunky_print --backend dbt -- /bin/echo hello
```

Ptrace remains the default. The normal `--dbt-artifact` override selects a
verified caller without prototype fallback. The separate
`HERMIT_DBT_CHUNKY_PRINT` prototype and its script consumers remain unconverted;
this public caller does not expand their fork, exec or signal support.

# strace: Reverie Echo Tool

This instrumentation tool simply echos intercepted events, like strace.

# strace_minimal: Minimal Strace Tool

The shared `StraceTool` prints syscall arguments with ` = ?` before tail
injection. The public DBT command uses that Tool through an authenticated caller:

```bash
reverie-dbt/scripts/build-client.sh \
  --caller-manifest reverie-examples/strace-minimal-dbt-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/strace_minimal --backend dbt -- /bin/echo hello
```

Ptrace remains the default. `--dbt-artifact` or `REVERIE_DBT_CALLER_ARTIFACT`
selects a verified manifest; missing or invalid artifacts fail without fallback.
The DBT caller retains its initial-process and signal-error limits. Its shared
Tool prints from inside the client: the echo test does not establish safety for
stderr closure/reuse or syscall-hook reentrancy. The separate
`HERMIT_DBT_STRACE` prototype and its consumers remain unchanged.

# chaos: Chaos Tool

This tool is meant to emulate a pathological kernel where:

 1. `read` and `recvfrom` calls return only one byte at a time. This is
    intended to catch errors in parsers that assume multiple bytes will be
    returned at a time.
 2. `EINTR` is returned instead of running the real syscall for every other
    read.

The public DBT command runs the same `ChaosTool` and `ChaosOpts` as ptrace.
Produce the authenticated caller for the command's Cargo profile:

```bash
reverie-dbt/scripts/build-client.sh \
  --caller-manifest reverie-examples/chaos-dbt-runtime/Cargo.toml \
  --profile debug --jobs 8 --check
target/debug/chaos --backend dbt --no-interrupt -- /bin/echo hello
```

Ptrace remains the default. `--dbt-artifact` or `REVERIE_DBT_CALLER_ARTIFACT`
selects a verified manifest; invalid or missing artifacts fail without fallback.
`--skip` counts intercepted startup syscalls too. The per-thread EINTR/retry
state spans eligible reads, so a loader or language-runtime read can consume the
first interruption before application code starts. The raw-syscall EINTR fixture
checks Linux's `-EINTR` result separately from libc's `-1`/`errno` contract; it
does not establish that the public Rust pipe fixture is the first eligible read.

The caller retains its initial-process, lifecycle and signal-error limits. These
pipe tests do not establish `recvfrom`, concurrent-clone or general stderr
close/reuse/reentrancy behavior. The separate `HERMIT_DBT_CHAOS` prototype and
its different option defaults remain unchanged.

## LiteInst backend

Build the shared example preload and its launcher:

```sh
cargo build -p reverie-examples --lib --bin reverie-liteinst-examples
```

Run the exact example `Tool` implementations through `LiteinstBackend`:

```sh
target/debug/reverie-liteinst-examples --tool noop -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool chaos --skip 32 --no-interrupt -- target/debug/reverie-liteinst-env-guest exercise-chaos-read
target/debug/reverie-liteinst-examples --tool counter1 -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool chunky-print -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool counter2 -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool strace --trace write -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool strace-minimal -- /bin/echo hello
target/debug/reverie-liteinst-examples --tool debug --port 1234 -- /bin/true
```

The `debug` selector is intentionally ptrace-assisted: the GDB remote protocol
needs an external controller for registers, breakpoints, and lifecycle stops.
The other eight selectors execute their production `Tool` through LiteInst's
in-guest preload and patch path.

This path supports dynamically linked Linux x86-64 guests. The LiteInst runtime
still rejects guest-installed callable signal handlers and clone/fork injection;
see `reverie-liteinst/README.md` for the complete backend boundary.
