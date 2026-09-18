# Exact final commands

Working directory:
`/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c`

Each command's combined output is the corresponding `.log`; its immediately
captured status is the corresponding `.exit`.

```sh
cargo fmt --all -- --check
cargo test -p reverie-ptrace runtime_dynamic_pointer -- --nocapture
cargo test -p reverie-ptrace target_loader::environment::tests -- --nocapture
cargo test --offline --locked -p reverie-ptrace target_loader:: -- --nocapture
cargo test --offline --locked -p reverie-ptrace after_loader -- --nocapture
cargo check --offline --locked -p reverie-ptrace --all-targets
cargo check --offline --locked -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment
```

The no-tracee release-profile control was:

```sh
CC=cc \
CARGO_NET_OFFLINE=true \
REVERIE_LITEINST_AFTER_LOADER_RUNTIME=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-06/stage/libreverie_liteinst.so \
REVERIE_LITEINST_AFTER_LOADER_MARKER=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-06/stage/runtime.marker \
REVERIE_LITEINST_AFTER_LOADER_GRAPH=/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c/ignored/dev-hermit-15-liteinst-after-loader-mapidentity-da5b85be/candidate-06/stage/graph.manifest \
cargo test --offline --locked --release -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment staged_union_graph_partitions_initial_and_dlopen_dependencies -- --exact --nocapture
```
