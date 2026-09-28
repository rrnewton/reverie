# Source v22 format and default-feature Clippy

Both checks passed on the unchanged frozen source v22. format used 1.713332 CPU / 2.365068742 wall seconds; clippy used 8.159869 CPU / 5.237101655 wall seconds.

Commands remain workspace cargo fmt --all -- --check and cargo clippy --locked --offline -p reverie-kvm --all-targets -- -D warnings. Full stdout and stderr were read. No warnings or errors were emitted; output contains ordinary build progress only. Both actual services completed with exit 0, complete accounting, inactive/dead, MainPID 0 and empty control group; independent terminal queries agree. No observer error, bound, cleanup refusal or truncated output occurred. Outputs were bounded and untruncated. Complete source/input comparisons passed afterward.

This is formatting and default-feature lint evidence, not native/VM/guest qualification. All earlier diagnostic records and unexecuted lint-v11/v12 preparations remain unchanged. RESULT.json SHA256 b915d606a0ec9e957358165675911629cfa9045ac2b9d70e20f1b29f21ba4202 retains exact raw hashes, services and terminal observations.
