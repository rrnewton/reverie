# Source-v2 native validation result

The accepted source-v2 candidate compiled, and all 27 selected native tests passed: 8 new controls and 19 unchanged nearby controls. There were 0 failed, 0 ignored, 0 measured, and 408 filtered-out tests. Inventory found 435 tests; only the 27 explicitly selected tests were executed. No KVM guest, Hermit invocation, scheduler integration or fault-mutation run occurred.

Source binding SHA256 `fba65b3fa1797f33a7598687a245603727b0e03ee0fcd03f9c77d38bf77dbbc4`; base `114b309413612fafc2657c74e83811c71aac7b19`; full source patch SHA256 `a3dc4d84bfc049daa644a1f54e958b2879b67654b8c31d6e78d527e7f76cea15`. Source remains uncommitted and unchanged during execution and independent review.

Cargo.lock SHA256 `1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856` includes only the separately reviewed existing-manifest revision alignment recorded in cargo-v3. The cargo-v2 stale-lock refusal remains a pre-compilation refusal, not a test result or a retry-only passing test.

Plan `plan.json` SHA256 `c297510a70887146cacf9fd68fe1d0c97f551918554e891421eb201b84976fd2`; caller `launch.py` SHA256 `61a895b980f5f90b5254f1773fa817b97d228e9b059f7ec6dc5eb605b6f3b82f`. The actual launch record is `run-1/launch.json`, SHA256 `cf87c102369a4a1d5fdb8cff46726aa98d2bbb1b6b58c292c1fead8c2d2215ff`.

The unchanged observer SHA256 is `f10ab861f262dbbd18295d92e59e05174299b72b397f58de844ee1725266eae6`. Compilation used `--locked --offline`, 600 CPU/900 wall seconds. Inventory used 5/15; native tests used 30/60. All stages had 16 GiB memory and zero swap, with two build jobs. No bound or selected-test requirement changed.

| Stage | Exit | Aggregate CPU seconds | Wall seconds |
| --- | ---: | ---: | ---: |
| Compile | 0 | 85.110235 | 45.583959234 |
| Inventory | 0 | 0.205115 | 0.906773083 |
| Native tests | 0 | 0.225295 | 1.090184755 |

Libtest reported 0.11 seconds for the selected tests themselves. The native service wall time includes observer setup and accounting. Each stage has complete accounting, no observer error or stopped bound, and an empty inactive service confirmed by a separate systemctl read after observer completion. No retained output was truncated. All source, lockfile, helper and emitted executable checks passed before and after each stage.

The actual emitted KVM library test executable is `target/prejoin-native-v4/debug/deps/reverie_kvm-dc68039652d66c1e`, 110491440 bytes, SHA256 `16c7ef52ccbeecf98da2e29d1d865977960cba599441b574fce9beac056a8569`. Cargo reported one matching library-test artifact and no compiler diagnostic messages. That exact file was listed and executed.

The native output contains the expected three caught panic diagnostics from the unchanged existing child-cleanup control: forced pending-child panic, completed-child panic, and completion-lock poison. All are retained in full stderr; they were neither filtered away nor counted as new implementation failures. The existing test asserts that every child is joined and every diagnostic survives.

Observed new behavior includes actual pending Tool RPC cancellation before an OS join, typed primary plus separate consuming-hook error, status 37 and worker-before-leader ordering on the normal path, independent failure waiters, synchronous publication before local wake, exact owner recovery after a refused OS spawn, and failed-only cancellation of all child gates before the first join. Every handle used for these controls is an OS thread JoinHandle. No control here runs a KVM guest or proves real Detcore selected-transaction or registered-child cleanup.

Remaining requirements are unchanged: implement and review the Hermit terminal transition and completion caller; exercise registered and unregistered child accounting, setup-error consuming cleanup, selected scheduler transaction closure, clear-TID dependency, final global cleanup and clock behavior; obtain the required independent review of the final coordinated source before landing. Source expectations about watchdog mutation refusal have not been measured by a separate mutation run. No source or landing approval is claimed from this selected native result.

Concise machine-readable evidence is `RESULT.json`, SHA256 `5febb2c758e5e90e5eb484ca6fcca36b4b6b20d388d77a3ecd9d69147e721ad2`. Full observer output records live under the reserved measurement-prejoin-native-20260917/cargo-v4/{compile,list,native} subtree. Their result hashes are compile `167bb07960f217c7a64509bc11114dedf26110e501361ca08c569dbeaf0ed2ab`, inventory `215fa8456ee36199c97edb2c9e1a39fcc551e618f55f5379e6d6ac11aeba1aad`, and native `464deacf724899dceab40cf6102c0267ee71ff3cadfddc40d1c212e4ffc06167`.
