The current source target is Hermit a9d3b1faa4502963e33bcb3e7e8a213a4e1a3bb0, tree340e146bb4e3943f33393cc71792bcc8a3b31079, based on b03fd6c16a438060f0013d58948643d8f3d81ab5. All measurements below remain attributed to immutable aa7ea4827b8328345e715d76518d7f2205e41110/tree40d69bea9282cbcbf161a2736bcb39108a1d53db, Reverie596b9ade and, for runtime, the published ELF a31abf5547e55c221b5a79fdff728ab2c4313882ace1a8473eac6890734a74fd. No a9 runtime outcome follows from source equality.

| Completed evidence | Actual outcome | Wall / aggregate CPU seconds |
| --- | --- | --- |
|19 selected native methods |19 pass;0 fail,0 ignored,0 missing expected identities | Per-command values in CHECKS-COMPLETED.json |
| Formatting | cargo fmt --all -- --check passed |3.152913 /2.698178 |
| Workspace/all-target Clippy, third-party-backends | Passed with -D warnings |38.314670 /69.550818 |
| Hermit nextest inventory | Exactly634 expected identities; no test execution inferred from listing |30.797556 /72.962212 |
| CLI build | Passed, followed by normal artifact publication |7.266809 /17.298229 |
| Descriptor-reuse, ptrace | Guest0, exact32-byte success stdout |1.871937 /0.754274 |
| Descriptor-reuse, KVM | Guest0, exact same32-byte stdout |1.824602 /0.748192 |
| Complete-stat-routes, native | Guest0, exact success stdout, empty stderr |0.535492 /0.029957 |
| Complete-stat-routes, ptrace | Guest0, exact success stdout, raw226819-byte INFO stderr |1.953513 /0.763502 |
| Complete-stat-routes, KVM | Guest0, exact success stdout, raw220464-byte INFO stderr |2.045842 /0.756270 |

The complete check sequence, including DAG --check and publication, used322.272465 aggregate CPU seconds and187.674703 summed wall seconds. It ran no guests. CHECKS-COMPLETED.json authenticates each original command, named test results, receipt/readback digest, unchanged source and inactive/empty final scope. Actual native bodies and inventory are not inferred from a successful wrapper exit. The first calibration refusal and below-tip commit refusal remain separately retained.

The runtime controls used the unchanged31,160-byte fixture fc1546ea0cbb2c1378f1d3ef1d26a5fd36203b64218fc6e6b0d39d5cff50d48c. They retained strict/minimal/tmpfs-/test arguments, the reviewed observer/safehermit route and complete INFO logs. There was one execution per listed leg, no retries, no truncation, no resource stop, no kill or observer error. Descriptor readback verifies22 input identities; complete-mode readback verifies17 native and22 Hermit inputs,25 unique paths. All final scopes were independently inactive/dead, MainPID0, empty ControlGroup. These are five ordinary runtime measurements across two fixture modes, not five newly selected cells, repeated determinism or canonical parity.

Exact retained report bindings:

- appendix/CHECKS-COMPLETED.json:62751fa45f88b1f1720c11a137255594775841219199fdecb473062b47148934. Actual command receipts/readbacks and logs are in appendix/checks; inventory-comparison retains the exact634 identities.
- appendix/descriptor-reuse/FINAL-READBACK.json:4f9f824edd13b226e8537faa2363a51d76a2a66fe85fe4ffc7bcb5a1de63c250. The two raw result hashes are f5f96cd623fca828bbd0157d816d69de631299ff7123af1891c7ef4d4d9abf5a and27df4c9825d219d4f7c5c8980847bf73503786a2ce9fad1de075e2b8cddeaf82. The original RESULT.md predates complete-mode execution and retains that earlier pending wording.
- appendix/complete-stat-routes/FINAL-READBACK.json:990eeeaf988478dfcb3fdcd689ac3c07f7f9bb6949896f728eac541011e442bb. RESULT.md:93e47997f9aedfca96347983e554262d6ed4772b04e592dcb741fe0c2f8b4545. Native/ptrace/KVM raw results and both streams are preserved beside it.

Actual a9 source composition is separately proven in composition-readback.json:21 authored paths and8 incoming-main paths, zero overlap; all1726 entries match the exact union; both authors and full commit messages are retained. The complete authored patch remains byte-identical96502f98. Incoming main changes only ledger/profile support, validation selftests and restored LiteInst arguments; the original resource/mount implementation, dependency pin and C fixture are unchanged. The full independent source verdict is appendix/composition/independent-REVIEW.md SHA74b3b498976b5ff47b0de58bc3a2dabbf56c91b7f221080436b09dab59b1b7dc; independent readback884fb965 and author full readback8b350feb remain available. Read the small scalar composition-readback.json first instead of loading its1,726-entry appendix.

The original full pinned-image Rust cat attempt on a9 refused before admission; no tests ran, and the original assertion remains pending. The retained record names validate-kvm-closed-stdin-a9d3b1fa-official.service, admission exit3/reason stale-base, and materialized_target=false; executed-test fields remain null, not an executed failure count. The retained log reports an unresolved fixed-floor lookup because GitHub compare returned HTTP404. Root independently found that the default local admission checkout lacks unpublished a9, while floor ancestry in the owned slot passes; a separate GitHub GET commit a9 returned HTTP422. This is a source-availability/admission refusal, not evidence that the cat assertion failed or passed. The exact record and333-byte log are copied under appendix/official-admission. The descriptor/proc-stat controls do not replace that original obligation.

The complete Reverie native library still has426 passes and1 failure out of427, zero ignored. Its original result, sole failed assertion, unchanged vm.rs and unknown causation remain in the prior bound package. Neither this appendix nor the source rebase changes that outcome. The prior external attempt timed out with no verdict; its public material is supplied for continued analysis, never as approval.
