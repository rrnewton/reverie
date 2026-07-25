# Examples

Example tools built on top of Reverie.

Copying one of these examples is the recommended way to get started using
Reverie.

# chrome-trace: Generates a chrome trace file

This tool is like `strace`, but generates a trace file that can be loaded in
`chrome://tracing/`.

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

# noop: Identity Function Tool

This instrumentation tool intercepts events but does nothing with them. It is
useful for observing the overhead of interception, and as a starting point.

# chunky_print: Print-gating Tool

This example tool intercepts write events on stdout and stderr and
manipulates either when those outputs are released, or the scheduling order
that determines the order of printed output.

# strace: Reverie Echo Tool

This instrumentation tool simply echos intercepted events, like strace.

# chaos: Chaos Tool

This tool is meant to emulate a pathological kernel where:

 1. `read` and `recvfrom` calls return only one byte at a time. This is
    intended to catch errors in parsers that assume multiple bytes will be
    returned at a time.
 2. `EINTR` is returned instead of running the real syscall for every other
    read.

## Cross-backend counter harness

bench.rs builds the selected backend adapter, runs one program, and emits a
CSV row containing the program, backend, wall-clock time, and total intercepted
syscall count:

    ./reverie-examples/bench.rs --backend ptrace -- /bin/echo hello
    ./reverie-examples/bench.rs --backend all -- /path/to/program

The ptrace path uses counter2. DBI uses its per-thread prototype counters, KVM
uses the same thread-exit aggregation pattern as counter2, and SaBRe uses
riptrace with quiet summary output. Compilation is excluded from wall_time_ms.

DBI requires the pinned DynamoRIO source to be active. KVM requires writable
/dev/kvm and an x86-64 ELF supported by its bounded Linux personality.
SaBRe requires either SABRE_BINARY or the pinned SaBRe source; when the source
is active the harness builds the loader in target/sabre. The all-backend mode
reports unavailable optional backends on stderr and continues with the rest.
Counts are backend-observed: prototype startup and interception coverage differs,
so totals for the same program are not expected to match across backends.
