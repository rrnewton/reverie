# Correction to the cargo-v9 report

The original REPORT.md statement that compilation had no compiler warnings was incorrect. Compile stdout contains one structured Rust dead_code warning for NativeToolOwner::thread_state, thread_state_mut and tool at native_test_support.rs lines 155, 162 and 169. The earlier inspection read compile stderr but omitted Cargo compiler-message diagnostics in stdout.

The original report and raw output are preserved unchanged. Compilation completed and the exact 37 selected native tests passed; this correction does not change their source, executable identity or outcomes. Future reports inspect both complete stderr and structured stdout diagnostics. The v13 all-features Clippy pass is a separate feature configuration and does not establish default-feature lint cleanliness.
