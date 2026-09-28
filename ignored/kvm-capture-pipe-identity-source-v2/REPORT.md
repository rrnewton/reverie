# Capture pipe identity V2: two compile corrections only

This unexecuted successor fixes the two actual V1 compile errors. It does not change production behavior. V1 metadata passed; V1 compile failed raw 101, before any of the 63 declarations ran. Its complete failure and private lease release remain unchanged in the prior packet.

The required GlobalTool::receive_rpc implementation now panics if called. This completes the test-only trait contract and strengthens its existing requirement: a capture allocation failure must return before Tool initialization or RPC dispatch. init_global_state still panics. The real EMFILE controls still require the original image, no recorded Tool failure, exact recovered descriptor counts and actual first/second pipe and relocation paths. No successful setup or RPC is substituted.

The old proc_executable_link_marks_unlinked_and_replaced_files_deleted test now passes None to readlink_at_impl, the exact no-capture meaning of its old false value. This /proc/self/exe path obtains the opened executable's real fd/path; it does not use capture metadata. All deleted/replaced-file, output-buffer and size assertions are byte-identical. No selector, timeout, comparator or result gate changes.

All other 2,624 product entries, including every production line, are identical to V1. The complete seven-path patch against qualified V6 and exact two-path V1-to-V2 delta are supplied. The source carrier still adds only the identical V6 lock; neither V1 nor V6 is rewritten or requalified by this preparation. The fresh caller retains all 63 declarations, the accepted tighter 30-second per-test wrapper, original outer bounds and first-failure stop. No compilation, tests, guests or independent source approval have occurred on V2.

The earlier focused search guessed reverie/src/global_tool.rs and returned ENOENT/status 2. The corrected direct source is reverie/src/tool.rs:119–149. Both the actual trait and full readlink helper/existing caller were read; this was a path-read error, not an executed product failure.
