# Cross-backend conformance

The same example `Tool`s run the same programs on every first-party backend
through the shared runner (`../backend.rs`), and their observations are
checked against each other and against native Linux. A backend only
intercepts and injects, so a difference a program or a tool can observe is a
backend defect unless it is written down here or in an expected-mismatch
ledger with its issue.

## Combinations

Backends are the shared runner's `BackendKind` values:

- `ptrace`: the tool runs in the tracer.
- `kvm`: the program runs in a KVM guest, the tool in the host. The guest's
  Linux personality is an internal part of this backend; it is not a
  separate backend and has no separate conformance column.
- `liteinst`: the tool runs in the program through a preload, with syscall
  site patching on.
- `in-guest-trap`: the same preload hosting with site patching off, so every
  syscall traps. It is the reference for the patching mode: patching may
  change how a syscall reaches the tool, never what the tool observes.

No page-fault trapping mode is selectable today, so this matrix names a
page-fault gate as absent, not as skipped; when such a mode becomes
a `BackendKind` it joins this matrix like any other backend.

External backends hosted by extension repositories reuse the recorder
comparator from their own entries (see `README.md`); they are outside this
repository's matrix.

Candidate tools are the example tools whose launchers accept `--backend`:
the pass-through `recorder`, `counter1`, `counter2`, `strace_minimal`,
`noop`, `chaos`, `strace`, `chunky_print`, and `chrome_trace`. The `debug`
tool is excluded: its protocol needs an external controller by design.
Only some rows are enforced today: the recorder in the judged e2e run,
and `counter1`, `strace_minimal`, and `noop` in `cargo test`. The remaining
oracles below are designs for later rows, marked as not yet enforced.

## Oracles

Native equality of the raw syscall stream is the recorder's oracle. Other
tools are checked against the recorder's record for the same backend and
program, because in-guest tools start at their preload's constructor (after
the dynamic loader's calls, and again after each `execve`), so they
legitimately observe a suffix of the native stream. The recorder's
comparable record also omits the calls the vDSO normally answers in the
process (`clock_gettime`, `clock_getres`, `gettimeofday`, `time`, `getcpu`),
which a backend may turn into syscalls a tool can see.

- **recorder** (in `run.sh` today): exit code, thread count, and each
  thread's syscalls with their results match a native baseline run twice,
  exactly, or after the late-start alignment for in-guest backends. Known
  mismatches are ledger rows; a stale row fails. Every run gets the C
  locale (`LC_ALL=C`), so the host's locale files, and where they are
  mapped, never reach a record (`README.md`, "Locale").
- **counter1** (enforced in `cargo test`): across separate executions of
  the same program on the same backend, the reported total equals the
  recorder's call count plus the vDSO calls `strace_minimal` saw in its
  own execution. That transfer assumes the program makes the same calls
  every run, as the corpus requires natively; a vDSO call delivered to no
  tool is outside this arithmetic. The program's stdout and exit status
  are its own.
- **counter2** (designed, not yet enforced): the same counting oracle,
  plus its process and thread totals matching the record's thread
  structure.
- **strace_minimal**: with the vDSO calls removed, its printed call-name
  sequence equals the recorder's. One named exception: an in-guest tool
  prints through the guest's own stderr, so a print for a final call made
  after the guest closed stderr (for example `/bin/echo` closes descriptors
  1 and 2 before `exit_group`) has nowhere to go. Tools that must not lose
  terminal events report through tool RPC, as the recorder and counters do.
- **noop**: subscribes to nothing, so it is a transparency bound: stdout,
  stderr, and exit status are the program's own on every backend.
- **chaos** (designed, not yet enforced): this tool mutates results on
  purpose (short reads, injected `EINTR`), so stream equality does not
  apply. Its oracle is a property: the program still produces its correct
  output and exit status, and every read the tool clipped returns at most
  one byte, on every backend.
- **strace**, **chunky_print**, **chrome_trace** (not yet in the matrix):
  their outputs need normalization first (decoded arguments and addresses,
  output ordering under concurrency, trace timestamps), so no oracle is
  enforced for them yet.

At the `cargo test` layer the native anchoring is by call name only:
ptrace and KVM records must equal the native name sequence exactly, and
in-guest records must be a native subsequence containing the run's
landmark calls (a `write` before a final `exit_group` for programs that
print). Syscall arguments and results are compared only by the recorder
row in the judged e2e run. A backend leg a host cannot run is reported
with a `REVERIE_SKIP:` line that the per-crate scorecard counts as
skipped, never as passed (and fails outright when `REVERIE_REQUIRE_KVM`
is set).

The `cargo test` layer has two named ledger differences, both for
the in-guest backends only. The first is the in-guest
`/proc/self/maps` read run segment
(`MAPS_READ_RUN` in `tests/conformance.rs`): in-guest maps lists
runtime mappings, so its content is about 3x the kernel's length
(measured on Ubuntu 26.04 for `/bin/echo`: native 5,449 bytes vs
in-guest-trap 16,287 and liteinst 17,268, all in full 1 KiB guest
reads) and a guest reading it makes a different number of
`read` calls. A record collapses exactly that segment — an
`fstat`, a uniform full-size read run, an `lseek` returning the
run's byte total, a second `lseek`, and `close` — only when the
segment is bound by descriptor to a successful `openat` of
`/proc/self/maps` in the same run's `strace_minimal` output
(the native side binds against a ptrace strace run, whose name
stream the ptrace leg proves equal to the native record);
missing or unparsable strace output, a failed open, descriptor
reuse, or any other path leaves the record literal. Whether
synthetic maps should instead trim runtime mappings is
https://github.com/facebookexperimental/hermit/issues/241.

The second is where `brk` falls (`HEAP_GROWTH` in
`tests/conformance.rs`). The in-guest runtime grows the guest's
`brk` heap before `main` and adds `LD_PRELOAD` to the guest's
environment, so the call at which a program such as `sh` grows its
heap depends on the runtime and on the environment's size. The
in-guest order check therefore leaves `brk` out and compares only
its count: an in-guest record may hold no more `brk` calls than the
native one. Every other call is still compared strictly, and the
ptrace and KVM legs compare every name exactly, `brk` included.
Making the guest heap match native is
https://github.com/facebookexperimental/reverie/issues/98.

A backend that cannot host a tool or a program must refuse by name
(`REFUSED:`), never fail silently or pass vacuously. A refusal is its own
scorecard verdict, separate from a mismatch.

## Asynchronous signal mode

`../tests/async_signal_mode.rs` runs one fixture under `noop` on every
backend and asserts each backend's actual values against native Linux.
Turning asynchronous signal mode on (`F_SETFL` with `O_ASYNC`, or
`FIOASYNC`) makes the kernel signal the description's owner. KVM's Linux
provider never gives a host description an owner, so it forwards the mode
only where no process outside the run can own the description and the
file's `fasync` picks no owner itself: the standard input its embedder
declared, a description the run created and never sent out of it, and the
memory devices without `fasync` (null, zero, full, mem and port). Turning
the mode off is always forwarded, and a request that changes nothing is
answered with 0. Every other request turning it on is refused by name
(`UnsupportedOperation::AsyncSignalMode`, ENOSYS to the guest), where the
other backends give Linux's answer. These are expected differences, each
counted as a refusal on KVM and never as parity:

| Case | Linux (ptrace, LiteInst, in-guest-trap) | KVM |
|---|---|---|
| `/dev/random` or `/dev/urandom`: `FIOASYNC` on; `F_SETFL` with `O_ASYNC` | 0; 0 | refused; refused |
| `/dev/kmsg`: both (its operations are not audited on the supported host kernels) | `ENOTTY`; 0 | refused; refused |
| an inherited or received pipe, FIFO or socket: both | 0; 0 | refused; refused |
| an inherited regular file or directory (`> out.txt`): both | `ENOTTY`; 0 | refused; refused |
| an inherited terminal or other character device: both | 0; 0 | refused; refused |
| `O_ASYNC` already on outside that domain, `F_SETFL` changing another settable flag (`O_NONBLOCK` included) | 0 | refused |
| a description the run sent in an `SCM_RIGHTS` message, through any alias, after a fork or exec: both | Linux's | refused |
| sending a description of that domain while the mode is on | 0 | refused, nothing sent or marked |

The fixture enforces the `/dev/urandom` and inherited standard input
rows; the others are enforced by reverie-linuxemu's unit tests
(`executor/async_signal_mode_tests.rs`), where kmsg's row is a table
proof, as are mem's and port's, since a test may lack the privileges to
open them.

## Corpus

`corpus.tsv` is the base corpus for the recorder: single-threaded system
programs, shell fork/exec and pipes, and interpreter startups. Each program
must first agree with itself in two native runs or it has no baseline.

Still missing, in order: a multi-threaded program, a signal-handling
program, a program whose exit closes its standard streams (covered by
`echo` above only as a tool-output case), a statically linked program
(expected result: the in-guest backends refuse by name, since a preload
needs a dynamic loader), and a program exercising the intercepted
instruction events (`cpuid`, `rdtsc`). Compiled fixture programs for these
belong beside the existing fixture guests, built by the run that needs
them.

## Where each check runs

- `cargo test` runs the environment-independent part
  (`../tests/recorder.rs`, `../tests/conformance.rs`): a few stable
  programs, every backend this host has, no ledger rows (only the two
  named in-guest differences above).
- `run.sh` runs the full recorder corpus against the ledger for this
  host's environment; `validate.sh` and CI call it. Its output is the
  conformance scorecard for the recorder row: per backend, counts of
  `match`, `mismatch` (ledgered or unexpected), `refused`, and
  `no_result` over the corpus.
- The LiteInst allocator-fixture producer
  (`scripts/build-liteinst-test-runtime.rs --for-ci`) is part of
  conformance, not a local side step: it qualifies the compiled fixture
  cohort the in-guest tests then consume. `cargo test -p
  reverie-liteinst-artifact` is NOT a substitute for the producer: unit
  tests use synthetic witnesses and cannot catch producer/cohort drift in
  real compiled bodies. The producer therefore also runs in CI on the
  pinned toolchain, including for bot-authored changes, reporting red or
  green truthfully as best-effort signal. CI is never the merge gate: the
  gate is local validation (`validate.sh` hard-fails on a producer
  refusal) and its validation ledger. The ledger's consumer should treat
  a missing or failing producer row for a main head as a red main, not a
  skip; that alert lives outside this repository.
- Extending the judged corpus run from the recorder to the counting and
  printing tools above is the next step after the cargo-test oracles: the
  comparator gains a tool dimension and the ledger a tool column, so a
  known difference names its tool, cell, backend, and issue.
