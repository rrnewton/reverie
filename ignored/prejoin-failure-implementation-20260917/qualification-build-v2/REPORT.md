# Reverie source v14 qualification build

The exact released stages passed. Source and Cargo.lock remained byte-bound. Complete stdout and stderr were inspected, including Cargo structured compiler diagnostics. All actual service accounting, output/resource bounds and independent inactive/empty readbacks passed.

- compile: 4.930461000 CPU seconds, 3.358937273 wall seconds.
- list-lib: 0.217056000 CPU seconds, 0.845713916 wall seconds.
- list-static-elf: 0.263321000 CPU seconds, 0.987990664 wall seconds.

No compiler-message diagnostics were emitted. Actual inventories are unchanged: 446 library tests and 288 static_elf integration methods. Both emitted ELF records and retained copies are bound under run-1. The static_elf ELF is byte-identical to v13, as expected for this cfg(test)/nondefault-feature helper-only change; the library test ELF has its own v14 identity. No tests or guests ran in this build/list attempt.
