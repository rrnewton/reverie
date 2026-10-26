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

The `dbt` feature also registers `dbt-basics`, which includes the same five
command declarations and Tool definitions. Each caller Tool is compiled into
its own constructor-free Rust cdylib and linked to a Release native client.
`REVERIE_DBT_NOOP_TEST_CLIENT` and `REVERIE_DBT_COUNTER_TEST_CLIENT` must name
those matching clients; missing artifacts fail, with no backend fallback.
The caller path uses the same core Tool/task/lifecycle owner as the in-guest
trap host and a callback-borrowed connection to its real GlobalTool server.
Noop sends no application RPC; installation readiness is a separate control.

```sh
cargo build -p reverie-test-support --features dbt --examples
# Link each cdylib using reverie-dbt/native/CMakeLists.txt and the matching SDK.
cargo test -p reverie-test-support --features dbt --test dbt-basics
```

This first DBT caller path admits only the initial root image. Physical task
creation and exec, signal delivery, and a root death without intercepted exit
are named failures. Instruction subscriptions select actual Tool callbacks;
unsubscribed instructions retain physical results. The eight inherited
function cases remain ptrace-only. No Detcore, determinism, full generic
matrix, or arbitrary thread/fork/exec/signal support is claimed here.

`dbt-native-controls` exercises ordinary injection, consuming exit callbacks
and instruction subscriptions. `dbt-admission` exercises explicit refusals.
Their additional client-path variables are documented beside the tests.
The private DBT SDK/native producer and its caller targets are Cargo-only;
this repository has no DBT Buck producer. Common core/in-guest sources and the
ptrace target remain mirrored in BUCK, including the shared command include.
