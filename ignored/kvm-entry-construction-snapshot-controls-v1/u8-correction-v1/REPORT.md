Author correction after completed v5 qualification

Root reported the actual failure of memory::entry_snapshot_tests::partial_sparse_fallback_waits_for_reopen_and_overwrites_full_destination at the exact dirty-prefix comparison. v5 is preserved as failed evidence. This addendum does not claim the corrected test passed.

Mechanism: the fixture's `dirty = [0xa5; 257]` and `prefix = [0; 258]` had unconstrained integer element types before their pointers were cast for pwrite/pread. Both defaulted to i32. The `.len()` values supplied to libc were byte lengths, so pwrite copied only 257 bytes of a 1028-byte i32 buffer and pread filled only 258 bytes of a 1032-byte i32 buffer. The assertion consequently compared the wrong representation. The intended source/fallback contract was never 257 i32 elements; it was 257 deliberately dirty bytes followed by one untouched zero byte.

Exactly three literals change:
- `[0xa5; 257]` in the actual pwrite source becomes `[0xa5_u8; 257]`.
- `[0; 258]` in the actual pread destination becomes `[0_u8; 258]`.
- `[0xa5; 257]` in the exact prefix assertion becomes `[0xa5_u8; 257]`.

The actual byte lengths 257 and 258, expected trailing zero, real pwrite/pread return checks, distinct two-waiter requirement, held close, full 1 MiB + 4096-byte fallback comparison, timeouts and all other source bytes remain unchanged. No comparator is widened and no failure is reclassified. The correction makes the fixture issue and inspect the originally specified byte operation; it does not alter production fallback behavior.

The captured before file authenticates against the frozen v5 source manifest 98a223d3e0d5a3569701a817a001723fad7e7cfbbc54e937ed71e423bd14af72. Exact replacement equality was checked mechanically: the after file is precisely the before file with those three replacements and nothing else. SOURCE.json records both hashes; SOURCE.patch is the complete change.

No compiler, formatter, test or guest run was performed. Source is released for another reviewer's independent assessment and root's fresh qualification. No claim is made about the separate clock-fixture correction or overall v5 success.
