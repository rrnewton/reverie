# Exact final validation commands

Working directory for every command:

`/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c`

The command's combined stdout/stderr is in the named `.log`; the immediately
captured status is in the matching `.exit`.

## Format

```sh
cargo fmt --all -- --check
```

Output: `fmt-check.log` / `fmt-check.exit`.

## Counter-guarded profile

```sh
cargo test -p reverie-ptrace counter_guarded_getenv_requires_exact_provider_code_metadata_and_bookkeeping -- --nocapture
```

Output: `counter-guarded-profile-test.log` / `counter-guarded-profile-test.exit`.
The later full environment-module command reruns this same test against the
final source snapshot.

## Full environment module

```sh
cargo test -p reverie-ptrace target_loader::environment::tests -- --nocapture
```

Final output: `environment-tests-fixed.log` / `environment-tests-fixed.exit`.

## After-loader unit surface

```sh
cargo test --offline --locked -p reverie-ptrace after_loader -- --nocapture
```

Output: `after-loader-tests.log` / `after-loader-tests.exit`.

## Compile checks

```sh
cargo check --offline --locked -p reverie-ptrace --all-targets
```

Output: `ptrace-all-target-check.log` / `ptrace-all-target-check.exit`.

```sh
cargo check --offline --locked -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment
```

Output: `liteinst-after-loader-check.log` / `liteinst-after-loader-check.exit`.

## Real-provider release-profile control (no tracee)

```sh
CC=cc \
CARGO_NET_OFFLINE=true \
REVERIE_LITEINST_AFTER_LOADER_RUNTIME=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-05/stage/libreverie_liteinst.so \
REVERIE_LITEINST_AFTER_LOADER_MARKER=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-05/stage/runtime.marker \
REVERIE_LITEINST_AFTER_LOADER_GRAPH=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-05/stage/graph.manifest \
cargo test --offline --locked --release -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment staged_union_graph_partitions_initial_and_dlopen_dependencies -- --exact --nocapture
```

Output: `real-provider-staged-partition-release-test.log` /
`real-provider-staged-partition-release-test.exit`. The selected test's source
constructs and inspects the configuration only; it never invokes the backend or
starts a tracee.
