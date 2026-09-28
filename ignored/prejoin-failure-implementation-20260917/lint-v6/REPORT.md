# Reverie source v14 lint

The exact released stages passed. Source and Cargo.lock remained byte-bound. Complete stdout and stderr were inspected, including Cargo structured compiler diagnostics. All actual service accounting, output/resource bounds and independent inactive/empty readbacks passed.

- format: 1.685114000 CPU seconds, 2.381170872 wall seconds.
- clippy: 24.622393000 CPU seconds, 13.245061215 wall seconds.

Workspace formatting and default-feature reverie-kvm all-targets Clippy with -D warnings passed. The previously reported default-feature dead_code warning is absent, without lint suppressions. No native test, VM or guest execution is claimed.
