# Exact-head qualification

Target: Reverie base `f7bd85e11dd258112148ed2cba6531501a1a00d9`, exact head `0598b5ffbeb737866372f89224d915efaeb29943`, tree `dab8c59a89f490c6143c197afc70ab6e6a3c8ff7`, pull request https://github.com/rrnewton/reverie/pull/603.

The packet qualification reran each command at that exact checked-out head. Every phase has its exact command, raw stdout and stderr, timestamps, `/usr/bin/time -p` record, and captured exit code under `qualification/`. Each command had a 180-second wall bound with TERM and a 10-second KILL grace. The runner itself exited 0 only after all eight captured statuses were 0.

| Phase | Result | Observed wall | Substance |
| --- | ---: | ---: | --- |
| `git diff --check f7bd85e1...0598b5ff` | 0 | 0.04s | complete diff has no whitespace errors |
| `cargo fmt --all -- --check` | 0 | 2.07s | workspace format check |
| `cargo test -p reverie-kvm --lib process_signal_publication` | 0 | 0.15s | 23 passed, 0 failed, 0 ignored, 730 filtered |
| `cargo check -p reverie-kvm --lib --features native-test-support` | 0 | 0.10s | non-test library configuration compiles |
| `cargo clippy -p reverie-kvm --lib --features native-test-support -- -D warnings` | 0 | 0.15s | warnings denied |
| exact real-KVM child-wait callback test | 0 | 0.40s | 1 passed, 0 failed, 333 filtered |
| `cargo test -p reverie-core` | 0 | 0.64s | 14 library, 4 signal-bridge, 1 validation, and 3 doctests passed; one pre-existing backend documentation example is ignored |
| `cargo test -p reverie-kvm --lib -- --test-threads=1` | 0 | 28.79s | 753 passed, 0 failed, 0 ignored |

The target was already compiled, so these wall times are cached qualification costs and are not clean-build measurements. An earlier same-head run also passed the same substantive matrix, with the serialized library suite taking 52.16 seconds; this packet relies on its own retained rerun rather than relabelling the earlier output.

The qualification does not execute a Hermit consumer, complete guest workload, ptrace/KVM parity comparison, record/replay run, or full workspace test suite. The real-KVM callback test observes exact generated identities and waitability, but no real Tool callback in this patch calls the new child-publication method; that composition belongs to the Hermit follow-up. Killed/core status encoding and post-commit signalfd failure are covered in focused unit tests rather than the real-KVM callback test.
