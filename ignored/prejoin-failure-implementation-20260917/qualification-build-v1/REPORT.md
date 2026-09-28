# Reverie source v13 qualification build

The released compile and two inventory stages completed successfully. This attempt did not execute any test or guest. Cargo structured stdout contains one dead_code warning for NativeToolOwner::thread_state, thread_state_mut and tool; it is retained alongside complete stderr and is addressed separately in source v14.

- compile: 30.456822000 CPU seconds, 16.785543209 wall seconds.
- list-lib: 0.218092000 CPU seconds, 0.892074064 wall seconds.
- list-static-elf: 0.263477000 CPU seconds, 0.967491134 wall seconds.

Actual inventories contain 446 library tests and 288 static_elf integration tests. All selected 37 native identities, four separate VM identities and 22 unchanged integration identities are present. The integration selection preserves all 17 leader-exit tests, all ten modes in the exec-worker method and four terminal-fork exec-failure methods. No assertion or selected count was changed.

All three observations passed complete actual service accounting, unchanged resource limits, uncapped output, terminal empty/inactive checks and independent systemctl readback. Full source, lock and emitted executable checks passed before and after each stage.

Both actual v13 ELF files are retained under run-1/retained-elf-v13 with matching original size, mode and SHA. Their identities remain v13 even when subsequent compilation reuses the Cargo target.
