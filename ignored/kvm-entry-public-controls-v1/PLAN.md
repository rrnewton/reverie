# Public Tool memory failure controls

Root accepted the plan and authorized a narrow cfg(test) ElfExecutor dispatch observer. Product source remains frozen until root explicitly releases it. Draft writes remain under this directory.

Four separate declarations cover public direct/ELF routes and ordinary RPC/injection independently. Each declaration enumerates read/write times poison before bytes/after a real completed two-byte vector portion, then healthy read/write counterparts: six subcases per declaration, 24 total. Fatal cases stop in thread start with zero guest exits, no new ordinary RPC constructor or target syscall dispatch, unchanged ftruncate target, exact cause retention, actual callback destruction before reporting, and one consuming thread/process hook each. Healthy cases complete the copy and operation, return normally from thread start, and run the installed direct syscall/HLT or static ELF exit(0).

The additional copy observer is per GuestMemory handle and cfg(test), receives the issuing EntryOrigin at the first nonempty copy and after each completed vector portion, and is armed only for the Tool copy. It preserves the original usize-only after_vector_copy hook unchanged. No callbacks run under backing/address-space mutexes. Hook poison uses the real EntryGate, stores only weak OperationOrigin receipts plus notification-only PendingFailure, and does not retain FailureContext/RunFailure in the shared observation.

The cfg(test) syscall observer is per handle carried by ElfExecutor's actual bound memory lifetime, invoked at its execute boundary before effects, and filters the armed valid ftruncate request. Its count is independent of the target memfd's observed length. Direct execution counts the same target at its actual provided executor. No global dispatch override or test-only substitute executor is used for ELF.

Public direct reporting remains by-value GlobalTool reporting, while ELF has actual FailureContext/RunFailure; the copy observer records the presence/absence of that context and requires the actual callback receipt be destroyed before either reporting path. The test will not infer one ownership route from the other.
