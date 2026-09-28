# First initialized-VM failure

Source freeze remains patch4e56817a6e754e31e69626c184ff2703dc3632d6c23618efb4e1cbc72734a90d on base99d1e4827cce2404442d7c27ab447886a5839326. No source was changed or retried after this failure.

The first cold compile passed, as did actual inventories (464 library tests,289 static_elf tests),10 new exact library controls,9 existing child-exit library controls and3 existing Tool-domain library controls. These22 controls executed successfully with zero ignored/failed tests. They are a selected subset,not full-suite results.

The actual static_elf test `process_alarm_signals::process_alarm_signal_tool_boundary_contract` then FAILED: raw payload status101,one test started,zero passed,one failed,zero ignored. The unchanged inner30-second self-exec returned status101 normally. The observer authenticated terminal empty-cgroup cleanup,all inputs remained unchanged,and no resource bound fired. Full stdout/stderr,result,outcomes and bound binary identities remain retained. The subsequent child-exit VM control was not run.

The inner failure is at support/process_alarm_signals.rs:104: the check that publication has not called the Tool signal hook expected0 observations but read1. The C fixture has `syscall(SYS_getpid, 0x616c726d, mode)` immediately followed by `getpid()` to compare the returned identity. The Tool uses syscall number plus the raw otherwise-unused arg0 marker to recognize its publication.

Disassembly of the bound installed libc supplies a concrete explanation: libc `syscall` shifts its explicit marker/mode into RDI/RSI and does not clear them; `__getpid` only sets EAX39,executes syscall and returns,also leaving RDI/RSI intact. Consequently the unmarked C identity comparison can re-enter the marked publication branch after the first signal has already reached the Tool. This is source/ABI evidence,not a captured failed-guest register trace. Queries and libc hash are retained under first-failure/.

Proposed successor fixture correction: obtain the expected process ID before issuing the marked syscall,then compare the marked result with that saved identity. All queue receipt,coalescing,no-recursive-hook,handler count,pending state,ignored/suppressed,default-fatal and lifecycle assertions must remain unchanged. Root authorization and successor source binding are required before touching the independently reviewed freeze. This diagnosis does not relabel the failed VM test as a pass or claim the remaining modes executed.
