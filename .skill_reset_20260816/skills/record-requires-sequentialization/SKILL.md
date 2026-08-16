---
name: record-requires-sequentialization
description: "hermit record/replay is fundamentally coupled to sequentialize_threads=true; QEMU can't be recorded because it can't boot sequentialized"
---

hermit record/replay is FUNDAMENTALLY coupled to `sequentialize_threads=true`, not a conservative default (investigated 2026-07-23, task impl-qemu-recording-exploration).

- `hermit-cli/src/metadata.rs:145-156` `record_or_replay_config()` hardcodes `sequentialize_threads:true`; comment: record & replay must use the EXACT SAME config or replay diverges.
- DEFINITIONAL: `detcore-model/src/config.rs:415` `should_trace_schedevent() = sequentialize_threads && !debug_externalize_sockets`. Schedule tracing is *defined* to require sequentialization. Gated at `detcore/src/lib.rs:800,1056,1309`; asserted at `detcore/src/tool_global.rs:842,1469,1590` (`trace_schedevent` starts with `assert!(guest.config().sequentialize_threads)`).
- Model: record = sequentialize + RCB preemption (default `preemption_timeout=200000000` vns). Each `SchedEvent` carries `end_rip` + RCB logical time; that precise total-ordered schedule only EXISTS because the scheduler steps threads one at a time.

Recording a schedule WITHOUT sequentialization is NOT a shortcut:
- REPLAY infeasible for racy programs (QEMU): sub-syscall memory interleaving (data races, lock-free atomics, untrapped futex fast paths) is invisible to hermit; RCB counts unstable across CPU migration. Would desync immediately (`die_on_desync`). Only "works" for race-free programs that already record fine.
- Empirical in the dated experiment: non-sequential schedule tracing panicked,
  while `hermit record start -- qemu…` hung with no serial output because that
  QEMU setup required non-sequentialized execution.

BOTTOM LINE for that revision: QEMU recording was blocked by the combination of
record's sequentialization requirement and QEMU's scheduler hang. Reconfirm on
current Hermit main before presenting it as current; this product-specific fact
belongs in Hermit's skill tree.
