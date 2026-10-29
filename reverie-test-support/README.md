# Generic command tests

This package registers the existing `tests/basics.rs` source. Its command
helpers delegate to ptrace's existing runtime, output capture and caller-owned
Tool configuration. The Tools live once in `tests/tool_fixtures/basics.rs`.
Inherited function tests still use the original ptrace helpers.
The test dependency enables ptrace's existing `nightly` output-capture reset,
so inherited `println!` output reaches the guest pipe under ordinary libtest
capture. The workspace's pinned nightly toolchain supplies that API.

Run the complete registered target with ordinary libtest capture:

```sh
cargo test -p reverie-test-support --test basics
```

The initial command selection is:

```sh
cargo test -p reverie-test-support --test basics -- \
  --exact noop_tool_test counter_tool_test error_exit_test \
  run_guest_command_test run_guest_command_test_closure
```

The target also discovers the other existing basics declarations; running the
five command tests does not establish those other results. Sanitizer exclusions
remain unchanged.

The optional `dbt` feature adds five `dbt_` command identities, sharing all
thirteen original command assertions. The original thirteen basics tests and
the default command helpers keep using ptrace. The eight inherited function
tests do not run through DBT.

Produce the two concrete caller artifacts before selecting DBT tests:

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/basics-noop-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/basics-counter-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test basics --test dbt_command -- \
  --test-threads=1
```

The producers must use the consumer's profile and target directory. The tests
select `reverie-dbt-callers/basics-{noop,counter}/manifest.json` beside that
profile's `deps` directory. Each thin runtime alias exports the actual shared
fixture Tool; CounterLocal's existing RPC handler updates the returned real
CounterGlobal. The explicit `test_dbt_cmd_with_config` helper preserves inherited
stdin, cwd and the application environment and captures stdout/stderr. Missing
artifacts and caller activation errors are returned without a fallback.

The `dbt_command` target contains three controls: inherited stdin plus the
unchanged null-input method's negative control; the original count oracle
against actual CounterLocal and actual Noop runtimes; and missing-manifest
refusal. The count control requires successful guest execution and rejects a
setup refusal as evidence. The caller protocol's initial-process domain still
applies; signal termination is a launch error, not supported signal-status
fidelity. These command cases do not establish raw-environment transparency,
function-guest execution, or arbitrary-program support.

## Injected-exit commands

The `exit` target keeps the original ptrace function guest and adds a command
that issues `getpid` followed by `exit_group(0)`. The actual shared
`InjectExitTool` replaces `getpid` with `exit_group(42)`. Both ptrace and the
optional authenticated DBT caller must return exit42 and exactly one exit42
process-hook RPC in the real returned GlobalState. Under DBT the RPC carries
the admitted terminal request; the child wait status separately checks the
actual exit. The original function guest is still ptrace-only.

The command fixture embeds its assembly and writes it into a test-owned
TempDir, so it needs no Cargo-specific runtime paths. Running these command
cases requires an x86-64 Linux C toolchain selected by `CC` or `cc`, supporting
`-nostdlib -static -no-pie -Wl,-e,_start -Wl,--build-id=none`. Missing compiler
capability fails the test. Every compiled image first runs natively and must
exit0 with empty streams; the TempDir lives through the native and traced run.

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/basics-noop-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/exit-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test exit -- \
  --test-threads=1
```

The default target has two tests; `dbt` adds the actual caller positive, a real
Noop negative and a missing-manifest guard. The Noop guest must first succeed
with exit0, empty streams and no application RPCs, then fail the same exit42
oracle. That control tests the first assertion's sensitivity; it does not
independently test each later RPC assertion. The missing-manifest guard first
authenticates the valid exit pair and does not run a guest. Caller initial-process
and signal-error restrictions remain. The helper installs `reverie=trace`
logging independently of `RUST_LOG`; these tests have no local timeout.

## Stat-family commands

The `stat` target preserves the two original ptrace function tests and adds
command forms using the same `LocalState` Tool. It subscribes to all syscalls
and uses the default identical tail forwarding; these tests do not establish
stat-to-fstatat conversion or a separate reinjection mechanism.

The first C guest checks that `open("/proc/self/exe", O_RDONLY)` returns a
positive descriptor, that `stat`, `lstat` and `fstat` succeed, and that the
followed inode equals the descriptor inode but differs from the symlink inode.
The second checks successful `fstatat` and raw `SYS_statx`, both with
`AT_SYMLINK_NOFOLLOW`, and equal returned inodes using `STATX_INO`.
These relations do not prove which executable `/proc/self/exe` names under
instrumentation.

The checked C sources are embedded and compiled in a test-owned TempDir using
`CC` or `cc`. They require an x86-64 Linux C toolchain and libc headers,
with assertions enabled. Dynamic loader and libc startup remain part of each
command run. Each compiled image must first run natively with exit0 and empty
streams, then satisfy the same guest assertions under ptrace or DBT with exit0
and empty streams. No inode values are compared across processes.

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/stat-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test stat -- \
  --test-threads=1
```

The default target lists four tests. The optional `dbt` feature adds two
actual-caller positives and a missing-manifest guard that first authenticates
the real stat artifact. It does not provide function-guest execution through
DBT or extend the caller's lifecycle/signal support. The default forwarding
Tool is behaviorally equivalent to Noop for these assertions, so no wrong-Tool
sensitivity is claimed. The existing helper installs `reverie=trace` logging;
these tests have no local timeout.

## vDSO commands

The `vdso` target retains all four original ptrace function tests and shares
their actual `LocalState` with a command fixture. The fixture resolves the
versioned `LINUX_2.6` clock_gettime, gettimeofday, getcpu and clock_getres entries
from `linux-vdso.so.1`, and checks their mapping against `AT_SYSINFO_EHDR`.
Missing entries fail without substituting libc calls or raw syscalls.

The Tool returns EINVAL for clock_gettime, changes gettimeofday's microsecond
suffix to 345 after injection, writes CPU0, and supplies resolution `(0,42)`.
The direct-entry command checks those exact results. Direct vDSO errors use
negative errno, while the unchanged Rust Instant test still expects a panic.
Its broad `should_panic` attribute is not a precise error-cause assertion.
These tests do not compare timestamps or establish deterministic time.

The embedded C fixture requires an x86-64 Linux C toolchain selected by `CC`
or `cc`, libc headers and libdl, with assertions enabled. Its owned TempDir
lives through both runs. Each compiled image first runs natively, checking
successful calls and valid output ranges. The same image then runs through
ptrace or authenticated DBT with the Tool-specific expectations. Each mode
requires exit0 and empty streams. Native and traced values are not compared
across processes; dynamic startup remains part of each run.

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/vdso-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test vdso -- \
  --test-threads=1
```

The default target has five tests. The optional `dbt` feature adds the actual
caller command and a missing-manifest guard, which authenticates the valid
pair before checking refusal. Unit global state alone does not authenticate
the selected Tool. No wrong-Tool sensitivity, DBT function-guest execution,
or expanded lifecycle/signal support is claimed. The helper installs
`reverie=trace` independently of `RUST_LOG`; these tests have no local timeout.

## Returning injections followed by identical tail forwarding

The `inject_then_tail` target reuses the standalone example's actual `TestTool`
with config 3. It performs three returning injections of selected identity and
getrandom calls, then forwards the original call with `tail_inject`. The original
standalone guest, host and main bodies remain unchanged; these are new command
tests, not execution of that old standalone program.

Each backend test compiles a checked C fixture using `CC` (or `cc`) in an owned
executable temporary directory, runs that same image natively first, and then
uses the actual Tool through ptrace or an explicit authenticated DBT caller.
The guest checks six identity results and an exact eight-byte getrandom result.
All runs require exit 0, stdout `inject-then-tail=ok\n`, and empty stderr. Numeric
IDs and random bytes are not compared across processes. Image bytes are retained
before each launch; independently compiled images need not have the same hash.
A C compiler and executable temporary storage are required; there is no local
timeout in these helpers.

The default target contains one ptrace test. The `dbt` feature adds the actual
caller test and a separate missing-manifest guard, for three tests total. The
real returned global state is `TestTool`. Its empty state does not authenticate
the exported Tool or independently count injections: caller source/artifact
identity and config bind the path, while the guest checks its results. Neither
Noop equivalence nor measured kernel-call multiplicity is claimed.

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/inject-then-tail-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test inject_then_tail -- \
  --test-threads=1
```

DBT keeps the existing initial-process restrictions and signal-error contract.
Buck registers the ptrace target; the optional DBT producer is explicit in the
Cargo validation and CI paths.

## AT_RANDOM through the initial post-exec hook

The `at_random` target reuses the standalone example's actual `TestTool` and
16-byte `PRNG_SEED`. Its post-exec hook writes that seed through the guest's
auxiliary vector. The original standalone guest, host and main bodies remain
unchanged; these are new command tests, not execution of that standalone.

Each positive compiles the embedded C fixture with `CC` (or `cc`) in an owned,
executable temporary directory. The same image and empty argument list run
natively first, then through ptrace or the authenticated DBT caller. Every run
requires exit 0, exactly 16 stdout bytes and empty stderr. Native entropy is
unconstrained; the Tool runs compare every byte with the shared seed. The C
fixture contains no copy of the seed. Compiler output and image bytes are
retained before launch; independent compilations need not produce equal hashes.
These helpers have no local timeout.

The default target has one ptrace test. Feature `dbt` adds the actual caller
positive and an independent missing-manifest guard, for three tests total.
The guard authenticates the current pair before requiring refusal. Unit global
state alone does not authenticate the selected Tool. No probabilistic Noop
negative, initial libc-canary timing, auxv-source branch or exec/re-exec support
is claimed. The existing DBT initial-process and signal-error limits remain.

```sh
reverie-dbt/scripts/build-client.sh \
  --caller-manifest tests/fixtures/at-random-dbt-runtime/Cargo.toml \
  --profile debug --jobs 4 --check
cargo test -p reverie-test-support --features dbt --test at_random -- \
  --test-threads=1
```

Buck registers the ptrace target; Cargo validation and both CI jobs explicitly
produce the DBT caller before its consumers. The maintained testing helper
installs `reverie=trace` independently of `RUST_LOG`.
