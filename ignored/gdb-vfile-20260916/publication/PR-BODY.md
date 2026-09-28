[hermit2, degraded-dev-hermit, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

GDB can probe `vFile:lstat` while loading replay symbols. Reverie currently treats that unsupported operation as malformed and closes the connection. Return the protocol-required empty reply so GDB can continue negotiating; this addresses the disconnect observed in the retained failing Hermit CLI run.

## Summary

Match the complete Host-I/O operation name through its second colon. Unknown nonempty operations get an empty response; existing supported operations keep their parser failures and filesystem behavior. No `lstat` filesystem access or invented successful Host-I/O result is added.

Four focused tests exercise parser boundaries and the real framed reader, relay and session over a duplex connection. They require the exact empty reply followed by a successful `!` request on that same connection, while malformed supported operations still close it.

## Validation

- Original production plus identical added tests: compilation passed, then 37 passed and two intentionally failed—the `lstat` parser assertion and the real connection's `UnexpectedEof`.
- Repaired source: all 39 GDB-stub tests passed in 0.120 seconds, with zero ignored. The other 138 library tests were filtered out of this focused run.
- Separate old/new targets and executable hashes bind the actual production variant. Targeted `cargo clippy --locked -p reverie-ptrace --lib --tests -- -D warnings` passed in 16.132 seconds; formatting passed.
- Native controls used 4 CPU, 8 GiB memory, zero swap, 1024 tasks and a 900-second backstop. Both scopes are terminal and memory/pids events were zero.

No existing assertions, bounds, checksum checks or comparators were relaxed. The pre-existing empty-path hex-decoder panic remains outside this narrow repair. These are native protocol results; the Hermit CLI guest failure has not been rerun and no full-main-green result is claimed.

Task: vision-ci-signal-is-trustworthy-end-to-end
