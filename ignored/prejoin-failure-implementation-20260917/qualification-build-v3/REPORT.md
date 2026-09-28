# Reverie source v16 qualification compilation

The released qualification-build-v3 compile and both actual inventories passed. Compilation used 22.480408 CPU / 14.193068956 wall seconds; library inventory used 0.215289 CPU / 0.845490513 wall and static_elf inventory used 0.265669 CPU / 0.956987417 wall. Structured compiler stdout has zero compiler-message diagnostics; complete stderr was read. This stage executed no test methods.

The actual inventories contain 448 library methods and 288 static_elf methods. All 39 selected native controls, four separate VM methods and the 22 original static_elf lifecycle methods are present exactly once. The actual v16 library ELF is byte-identical to the already measured native artifact; the newly compiled static ELF has its own emitted SHA. Both files are copied under run-1/retained-elf-v16 with size, mode, hash and separate-inode readback.

All three actual services have complete CPU accounting and independently confirmed inactive/empty terminal state, with no observer error, deadline or diagnostic cap reached. Source, lock, external inputs and actual artifact bytes were checked again afterward. Run-1 retains actual launch, full raw inventories, compiler artifacts and service results. VM/static execution remains a separately authorized next stage, with the full original cohort and unchanged assertions.
