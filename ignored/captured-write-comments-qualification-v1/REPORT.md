# Applied comments qualification

All three bounded component checks passed on Reverie `7b28de2b20b6e7fdcdab92e552d4166908b50cad`, tree `33fced78b132b845698bdbc79ff3b6ebac72ca6e`. All 2,620 source entries, branch and index were authenticated before/after and remained unchanged. No runtime tests were run; no test count or parity claim is added.

| Phase | Raw status | Payload seconds | Aggregate CPU seconds |
|---|---:|---:|---:|
| format | 0 | 0.412 | 2.844 |
| core-check | 0 | 1.595 | 3.972 |
| clippy | 0 | 21.255 | 5.240 |

Format checked the same seven Rust component paths using `rustfmt --check --edition 2024 --config skip_children=true`. Core/ptrace used `cargo check --offline --locked -p reverie-core -p reverie-ptrace --message-format=json`. Clippy used `cargo clippy --offline --locked -p reverie-kvm --lib --test static_elf --message-format=json -- -D warnings`. Both Cargo commands emitted exactly one successful completion and no structured diagnostics.

Each phase used pinned nightly-2026-07-29, two Cargo/build jobs, the owned target and lease, 600 aggregate CPU seconds, 900 wall seconds, 16 GiB memory, zero swap, 16 MiB stderr, 64 MiB live stdout guard, and 100 GiB free-space floor. The unchanged observer `137c9b42…` authenticated final accounting and empty terminal units; normal phase completion released each lease handle. No foreign target copy or source/SCM mutation occurred.

The fresh caller changes only the format path admission from uncommitted WIP to this exact committed component and applies the explicitly authorized 600/900 format allowance. Original compile receipts are copied byte-for-byte solely as prior artifact provenance; they are not new compilations/tests. `RUNNER_ORIGINS.json` and `caller-change.patch` retain that distinction. All source, toolchain, dependency and loader identities are bound in the new plans.

This satisfies the comments-only follow-up at the applied tree. Earlier captured-write runtime evidence and its limitations remain separate and immutable.
