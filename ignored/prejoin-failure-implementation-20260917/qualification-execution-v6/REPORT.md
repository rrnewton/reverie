# Final Reverie VM and static qualification

Commit `9db60ab95587d4cb5e0438dfeca409471eb9baf5` passed the original four VM and 22 static ELF methods in their unchanged order. These are 26 accepted first attempts on this final head, with no ignored method, retry, refusal, or unexecuted stage. The static population retains the ten-mode exec diagnostic method and every original leader-exit and terminal-fork assertion. This is selected Reverie qualification, not a full suite result or a Hermit determinism or canonical parity measurement.

All 26 actual services had independent `/dev/kvm` admission with API version 12 and `REVERIE_REQUIRE_KVM=1`, exact binary/argv binding, normal zero payload exit, complete CPU accounting, and inactive empty terminal cgroups. Raw stdout and stderr were read completely within the existing limits and were untruncated. Fresh independent systemctl queries again found all 26 services inactive, MainPID 0, and empty ControlGroup. The sum of service CPU was 9.369867 seconds; the sum of individual observed stage wall times was 26.561466471 seconds, not an end-to-end duration. Each stage retained its 30 CPU / 60 wall seconds, 16 GiB memory, zero swap and 1 MiB output bounds.

Both real terminal-fork continuation controls produced the original two writes and natural status 0 before return, including wait-event publication where expected. The first typed worker-3 EIO remained singular, and the adjacent error controls retained distinct ENOSPC, EACCES and E2BIG cleanup causes. The cancellation VM control's expected fatal-loop diagnostic remains in raw stderr. The exact diagnostic and status comparators were not changed.

Full source and all 86 bound inputs matched after execution. Compiler-emitted library ELF `daf7a52b631f19790dcac4e1d1fbbbd3326683ca3305a424db6befedb25cee8b` and static ELF `f11f2ec042c41f42ca16e062b8ac4da483998635cdd4d621d259a3be12fe2b85` still match the separately retained final-head copies and their actual build inventories (453 and 288). Native-forward-v1 separately passed the unchanged 44 native controls; it is not counted as VM execution.

The prior source-v22 accounting refusal and authorized retry remain unchanged: its original static-elf-05 refusal is not retroactively accepted, and that older 26-method union still consists of 25 first attempts plus one accepted retry. This final-head attempt is a separate complete run required by changed ELF bytes after forward reconciliation. Earlier source failures also remain preserved.

Known remaining limits include the pre-existing successful-exec pending-RPC cancellation gap, arbitrary unwind cleanup beyond the caught-worker path, previously retained whole-suite fatal_worker_ro_delayed_waiter failure, and unmeasured per-vCPU performance cost. The coordinated Hermit terminal scheduler/ownership integration and actual Hermit guest qualification are separate work.

Bindings:

- Source-v23 binding: `043b859fea40ae2da1a95eab4774e57c1e72eeacf35eafbdc10534f31057119f`
- Caller: `057713080d94f9d1b4ca23b2abc284ac84fc0431e64e859e99ced404199bb4dd`
- Plan: `88d3c517941d7bf46187e71532dc23589d53e0cdb95a966f92dffb49fec185fa`
- Actual launch: `844f7adcb55b853913b592137b40a060486bf7ce7512bddd21e1a26eb1b715f1`
- RESULT.json: `10861f2ebc67b791fe384f841cb94da88358c12859c55e2206a17618c78f527b`
- TERMINAL-READBACK.json: `6d8072023379632a9bbf4191dbe011f65d9fa1e309d987b5b96fba486d86ec99`
