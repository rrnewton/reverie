[hermit2, unknown-agentname, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

Keep SaBRe plugin ELF finalizers in the tool domain. The dynamic linker previously ran the plugin's mimalloc cleanup with guest routing active, so allocator clocks and memory advice were incorrectly sent through shared Detcore as guest operations. Wrap only the selected plugin object's finalizers, preserving the original reverse FINI_ARRAY order followed by FINI and restoring the prior domain afterward.

The retained pid-probe reproduction now records 6 guest syscalls instead of 52: the 39 allocator clock calls and 7 madvise calls per run remain native plugin cleanup. Guest and ordinary dependency finalizers still run in their original domain. This is a substantive event-ownership repair; the separate startup cross-backend mismatch remains open.

## Determinism

No comparison policy, log filtering, guest clock, scheduler, or syscall implementation changes. The existing plugin routing guard covers the plugin's complete ELF finalization sequence, just as it covers plugin initialization and ordinary callbacks. Original callbacks execute once in unchanged order; saved domain state is restored, including an already-active plugin domain. Other link maps are untouched.

## Linux Semantics

The plugin still performs its full allocator cleanup, including native clock_gettime and madvise calls. No destructor is dropped, reordered, or suppressed. Objects with only FINI_ARRAY, only FINI, an empty array, or no finalizers retain their behavior. Dynamic clients use their intercepted loader namespace; static clients match the preloaded plugin in the existing loader namespace. Map selection uses the actual mapped base, with missing/ambiguous maps refused.

## Validation

- `cargo fmt --all -- --check`: passed.
- `cargo test --offline -p reverie-sabre --test loader_plugin_finalizers --test late_function_registration -- --nocapture`: 3 tests passed, including the real dynamic/static loader cases, original refusal cases, and 10 map/order/domain controls.
- `cargo clippy --offline -p reverie-sabre --tests -- -D warnings`: passed. Existing vendored libelf C compiler warnings remain unchanged.
- The exact corrected real lifecycle fixture fails under the old loader at the plugin finalizer-domain assertion, then passes with this repair. Guest/client and ordinary-library destructor checks remain active after actual guest main entry. The static fixture tests namespace installation and detours; its raw exit does not establish static ELF-finalizer execution.
- Through `safehermit`, the retained H839 Hermit binary/plugin with the repaired loader executes pid-probe at L2 for SaBRe: strict BitwiseInfoV1 repeat match, 31/31 compared INFO messages, 6 syscalls per run, info logging, no relaxations. Binary/plugin hashes and source identities are retained; this is focused compatible-loader evidence, not a current-main full-profile receipt.
- Cross-backend comparison against the retained ptrace run remains divergent at startup record 3 (root PRNG seed versus post-exec AT_RANDOM). This PR does not claim cross-backend parity or project-wide green status.

No public API, new syscall support, determinization strategy, or Detcore scheduler change is introduced.
