# Exact source-validation commands

Working directory:
`/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-15-liteinst-bootstrap-rdtsc-01a0a13c`

```sh
CARGO_NET_OFFLINE=true cargo test --offline --locked -p reverie-ptrace --lib target_loader::environment::tests -- --nocapture
CARGO_NET_OFFLINE=true cargo test --offline --locked -p reverie-ptrace target_loader:: -- --nocapture
CARGO_NET_OFFLINE=true cargo test --offline --locked -p reverie-ptrace after_loader -- --nocapture
cargo fmt --all -- --check
CARGO_NET_OFFLINE=true cargo check --offline --locked -p reverie-ptrace --all-targets
CARGO_NET_OFFLINE=true cargo check --offline --locked -p reverie-liteinst --test after_loader --no-default-features --features liteinst-after-loader-experiment
```

No Hermit, guest, candidate, runtime-marker generation, or staging command was
run for this packet.
