# Full native library attempt: 426 passed, one failed

The released v2 caller executed the unchanged source at 696 Rust test ELF once over the complete normal 427-test population. The actual terminal libtest summary is 426 passed, 1 failed, 0 ignored, 0 measured and 0 filtered. The sole failure is vm::tests::fatal_worker_ro_delayed_waiter_qualifies. This is a failed full-library run, not a full-library success or a retry pass. The original 37 selected tests all passed again, and their earlier results remain separately preserved.

Native payload exit was 101; the controller then exited 1 on its explicit failure guard. It retained and independently parsed every individual outcome before refusing success. JSON framing and exact population checks succeeded: one 427-test start, 427 unique named outcomes matching the retained listing, and the consistent terminal failed summary. The outer measurement completed in 21.283923545 wall seconds and 8.260615 aggregate CPU seconds, below its unchanged 60-wall/30-CPU bounds. No observer error, stop reason, truncation or cleanup error occurred. The current service and all four prerequisite services were independently read back inactive/dead, MainPID 0, empty control group; final accounting is complete.

The exact stderr failure is:

```text
thread 'vm::tests::fatal_worker_ro_delayed_waiter_qualifies' (...) panicked at reverie-kvm/src/vm.rs:5221:9:
assertion `left == right` failed: waiter must be queued at its original word; waiter result: Ok((-1, Some(110)))
  left: 0
 right: 1
```

Source bounds the finding. vm.rs has identical bytes at base 24cd5bb518b027eddb62a226805210d74d31c3d8 and candidate 696f0476aa46cf29e31b947a89379d80b4542ce3 (Git blob 13aeec30c28f15cb96525f342261efde290d792f, SHA256 449c3287f9aa6de363292d5aaced5427a834a2dcda58779f0e3fa87b522d322d). The delayed control chooses read-only mode and a 150-millisecond waiter delay (:5416). qualify_waiter_enrollment (:5173) first requeues one waiter from its original word to a stack parking word, then requires the reverse FUTEX_CMP_REQUEUE to return 1. Reaching the failing reverse assertion proves from control flow that the first requeue returned 1. The reverse operation returned 0, and the retained waiter diagnostic is ETIMEDOUT (errno 110).

This happens before start_pending_children at line 5374. The fatal worker has not yet been released to perform the exit/store/wake assertions. Thus this result exposes a failure in the unchanged test's enrollment phase; it is not a measured failure of the new proc-fd/stat/fdinfo route. Source equality alone does not prove a base runtime pass/failure, and no such comparison was executed. No retained timestamps identify why the waiter timed out between the two requeues; host scheduling, test fragility, and broader interactions remain unproven. The failure must not be dismissed as environmental or fixed by widening the timeout without evidence.

All 64 bound inputs, all 2550 tracked non-gitlink source files, Cargo.lock, and the actual 106,866,600-byte ELF were independently rehashed after the failed run. The ELF remains b899a2eec14c966aa64dcadd2064177cd412e3a2e57abe68efbf2e327075a783, matching the before-exec authorization and actual payload argv. Local head remains 696 with clean tracked source/index. The normal suite emitted 52 initialization case records with passed=true and 52 verified-child records; no “skipping” diagnostic occurred. The top-level initialization_child helper itself returns when its child-only environment is absent, so ordinary libtest pass counts are not a blanket assertion that every conditional/helper body executed. Existing deliberate panic/poison negative controls printed diagnostics but returned passing test events; they are distinct from the sole failed method.

Exact evidence:

- execution-readback.json SHA256 9b701e2669aae08de50d4005d6c3b222944609781c9d94ed93f213eea380df56 contains all individual names, source/input hashes, actual payload and five fresh service readbacks.
- Raw observer result.json SHA256 8f465b62f9ca251f8d9aee87de1927d15c7faebb1e3701b56262c6232da675dc; stdout 5c63435386d43135e9ab0ea8055fc61b8f9171666d3f011c14d1bbbecaf2085c; stderr 1b538ab47f8b0f5897f8bb2bdb7fc61968161da6050acc337810a0866d406e1d. Full paths are in the readback.
- run-1/native-outcomes.json and run-1/summary.json retain the controller's actual 426/1 result and failure guard. The v1 preparation and earlier compile/native failures remain untouched.

No source, assertion, timeout, comparator, selection, ignored flag or retry rule changed. No retry occurred. This extends component evidence with one real failure; it is not Hermit canonical guest parity, does not reclassify the old census, and does not discharge the unchanged descriptor-reuse, followed-stat or cat obligations for https://github.com/rrnewton/hermit/pull/3047. The already landed Reverie component remains https://github.com/rrnewton/reverie/pull/565 at 596b9adee8473dc0a7e62dce18580eead3d0c5c9; its previously measured 37-test/Clippy evidence remains source-bound.
