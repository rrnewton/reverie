# Native ptrace test fixtures

On Linux/x86_64, Cargo's `reverie-ptrace/build.rs` builds the six C fixture
executables in `OUT_DIR`. It selects the target C compiler via `cc`, retains
`-std=gnu11 -O0 -g -Wall -Wextra -Werror -UNDEBUG`, and fails the Cargo build on
compiler/linker failure. The timer executable links a renamed-main copy of
`private-continuation.c` with `private-timer-wrapper.c`. No fixture is executed
by the build script or linked into the production library. These native
compilations also occur on non-test Linux/x86_64 builds; no `cfg(test)` gate is
available to a Cargo build script. Other targets do not build the x86 fixtures.

`cohort_bridge_fixture` is a build-only Cargo example using the unchanged Rust
guest source. Ordinary `cargo test -p reverie-ptrace` and workspace `cargo test`
build examples without executing their `main`. The fixture is not an independent
integration test: its modes are driven by the actual ptrace unit tests.

Library-only testing needs an explicit example-build prerequisite:

```sh
cargo build -p reverie-ptrace --example cohort_bridge_fixture
cargo test -p reverie-ptrace --lib -- --test-threads=1
```

Use identical target, profile, features and Rust flags for both commands. A
fresh `cargo test --lib` alone does not build the example and must fail if its
tests need that missing fixture. Explicit target selection such as
`cargo test --all-targets` is not a replacement for the executable example build.

Tests use the Cargo-built C artifacts and the standard profile directory's
`examples/cohort_bridge_fixture` by default. All existing runtime overrides have
priority and must name absolute executable files:

| Variable | Cargo-built fixture |
| --- | --- |
| `COHORT_BRIDGE_FIXTURE` | `examples/cohort_bridge_fixture` |
| `REVERIE_SOURCE_OBSERVATION_FIXTURE` | `OUT_DIR/source-observation` |
| `REVERIE_CLONE3_JOIN_FIXTURE` | `OUT_DIR/clone3-join` |
| `REVERIE_PRIVATE_SIGNAL_FIXTURE` | `OUT_DIR/private-signal` |
| `REVERIE_PRIVATE_REPLAY_FIXTURE` | `OUT_DIR/private-replay` |
| `REVERIE_PRIVATE_CONTINUATION_FIXTURE` | `OUT_DIR/private-continuation` |
| `REVERIE_PRIVATE_TIMER_FIXTURE` | `OUT_DIR/private-timer` |

An invalid override never falls back to a different executable. A missing
fixture is an error, not an ignored test. No PATH/glob/latest-artifact search is
performed. For a separately configured intermediate `build.build-dir`, a
relocated libtest, or non-Cargo tooling, supply the explicit absolute overrides;
the resolver does not infer an unrelated Cargo target directory. Custom
`CARGO_TARGET_DIR` with the standard `deps`/`examples` layout is supported.

The manifest is autocargo-generated. The root `BUCK` target's existing
`cargo_toml_config` does not describe this example or the C build dependency;
that internal export mapping still needs synchronization by its owner. This
Cargo wiring is not a claim that Buck provisions or runs these native fixtures.
