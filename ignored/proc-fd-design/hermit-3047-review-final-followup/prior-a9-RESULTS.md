# Hermit 3047 external continuation result

The one released continuation completed with actual reviewer exit **0** and the literal verdict **APPROVE** for https://github.com/rrnewton/hermit/pull/3047 at `a9d3b1faa4502963e33bcb3e7e8a213a4e1a3bb0`, tree `340e146bb4e3943f33393cc71792bcc8a3b31079`, base `b03fd6c16a438060f0013d58948643d8f3d81ab5`. This reports the external reviewer's verdict; it does not report a new build or runtime result.

The exact literal report is `REVIEW.md`, 17,609 bytes, SHA256 `f8d2cca4a3a8235bd759b9b4bf6f7526d70769ebf99533c71d02b288d4d6c08f`. It was extracted without additions from the single successful terminal `result.result` and is byte-identical to the last public assistant text. The complete report was read after extraction. No hidden thinking was relayed or used as a verdict.

## Execution and input readback

- Tool session: `64761`, completed. Actual start `2026-09-17T06:59:58.290086+00:00`; finish `2026-09-17T07:08:43.138878+00:00`; wall time **524.848555972 seconds**.
- Released caller `f0adcef76eb824ee986c6ec05d707a6fc89de50e0f0fb25eae36ab8754a0ad0a`, plan `66df1e89808a904eaca07e959dde60ea46ec07347cd784fa1b2e14b939d4b26b`, corrected prompt `ef212605d1db6efa254fb19bf6313e682021e0c4f615c1fc12657ac5a5b9ecac` and input binding `fc80029a53e53c335014f86a4236d69c7c555c23555ae17a10d63a36c64a95d3` were used exactly once.
- Existing bounds remained 900 seconds, TERM then 10-second grace, and 64 MiB per output file. The reviewer requested **23 Read/Grep calls**; no execution or write tools were requested. One valid, non-error terminal result exists; there are no invalid JSON lines.
- Both launcher and independent audit verified **445 input files**, including modes and complete bytes. The audit additionally compared **63 current source copies plus 74 retained source/base copies** with immutable Git blobs, modes and revision paths. The actual a9 tree matches the bound tree.
- `COMPLETION-READBACK.json`: 219,199 bytes, SHA256 `0a13c704fc69ce62745ae0ea581550cc9d5b5bfa8b2498a325a1631c9cac7d02`.
- `stdout.jsonl`: 1,112,279 bytes, SHA256 `1fb8686e5eb15a001c90fcb1e1249462942ff9566f18613044bab66eb40e2a19`. The raw stream is retained; public reporting uses only the literal public final text.
- `stderr.log`: 93 bytes, SHA256 `baa73543804294abc0f6c8fc48aeda5ae18986656b5edbc3c7f41677d79ac60e`.
- `exit.json`: 1,285 bytes, SHA256 `7bd2a47f337f39bba3ea7d5023bdf6a2319bafc0432f877c58484a1b8eb331bc`.

The prior 900-second attempt remains preserved as exit 124 with no final verdict. This successful, separately released continuation does not relabel that attempt.

## Findings and evidence limits

The external reviewer finds no blocking defect in the six-consumer resource guard, mount-provenance integration, pin carry or generated inventory. Its explicit goalpost assessment finds no weakened assertion, comparator, tolerance, label or gate.

Its **F1 is nonblocking**: the appended native fixture command has a dedicated `current_dir`, while the backend commands do not set one. Outside the official `/test` setup, the fixture's relative temporary files can be created in the harness working directory and remain there on an assertion failure. The reviewer recommends a dedicated backend working directory. This run made no source correction.

F2 records SaBRe's pre-existing live metadata discovery limitation; F3 records intentionally retained inherited-stream stat/alias semantics; F4 records a pre-existing potential overlap between the ordinary inode pool and fixed stream identities. These are source findings and retained scope limits, not additional runtime diagnoses, new backend qualification, or completed follow-up repairs.

The original `run_kvm_preserves_closed_standard_input` method, including its unchanged cat assertions and appended empty-stderr assertion, has not run in the evidence supplied to this continuation. The official attempt refused before admission because the unpublished a9 source was unavailable to its default admission path; zero tests were admitted. That is neither a cat pass nor a cat assertion failure.

The final report also requests complete-stat-routes and generator/inventory at a9. The existing evidence must remain explicit: **both descriptor-reuse legs and all three complete-stat-routes legs already passed at aa7 with published binary a31abf55**. All 19 selected native methods, formatting, Clippy, the 634-identity inventory and build are also attributed to aa7. Exact source composition into a9 was separately reviewed; it is not a new execution at a9. The final checklist does not invalidate or erase the completed aa7 measurements, and no additional execution was started here.

The Reverie full-library measurement remains **426 passed / 1 failed** on the authenticated candidate-native ELF, with unchanged `vm.rs` and no base runtime comparison. The earlier selected 37 native passes and Clippy success remain valid within their own source bindings. None of these results is Hermit canonical guest parity.

No source, index, ref, pull request or frozen review input was changed during this task. No second continuation was started.
