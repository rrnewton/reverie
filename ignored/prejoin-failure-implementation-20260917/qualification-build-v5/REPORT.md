# Reverie v22 qualification build

The bounded compile and two inventories passed. Actual library inventory is 453; static_elf inventory is 288. All previous identities remain, plus only the exact new native names. The prepared selection retains all 44 natives, four VM and 22 original static methods. No tests ran in this sequence.

compile used 19.971743 CPU / 13.261101352 wall seconds; list-lib used 0.213395 CPU / 0.840061424 wall seconds; list-static-elf used 0.270120 CPU / 1.140226312 wall seconds. All three services exited 0 with complete accounting and independently inactive/dead, MainPID 0, empty control group. No observer error, bound, cleanup refusal or truncated output occurred. Output was bounded and untruncated.

Cargo structured stdout contains 0 compiler-message rows; complete stderr was read. Both exact compiler-emitted executables were copied to separate inodes under run-1/retained-elf-v22/ and bound by c7d55e6fb017c47e43e3f05fe5d86500d80b4cb32f80701741a3c9237af10dd8. Complete source/input and artifact comparisons passed after execution. RESULT.json SHA256 002926b731704889c77f75bc101bffe22821ffc5f484d089bfc8fb2089442d54 retains all artifact, raw-output and terminal identities.
