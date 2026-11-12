# Reverie source v13 lint

The exact released lint-v5 caller completed successfully. Workspace formatting and workspace all-targets/all-features Clippy with -D warnings both passed. Sources and Cargo.lock remained byte-bound before and after both commands. Each observer result passed complete accounting, unchanged bounds, uncapped output and terminal inactive/empty checks, followed by the independent systemctl readback.

- format: 1.660288000 CPU seconds, 2.258303472 wall seconds.
- clippy: 37.149967000 CPU seconds, 20.371760375 wall seconds.

Clippy stderr retains existing Cargo build-script warning messages for third-party build caches and libelf fallthroughs. No Rust lint error occurred. This is lint evidence only, not native test, VM, Hermit guest or parity evidence. Source v12 native outcomes remain attributed to v12.
