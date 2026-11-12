The first current full-library attempt failed: 450 passed, 3 failed, 0 ignored and 0 filtered from all 453 registered methods. Libtest completed the whole population; this was not a timeout or observer refusal. All 427 original identities were retained: 424 passed and 3 failed. All 26 added repair controls passed. The four unchanged futex enrollment/store/wake methods passed within this full run as well as their preceding focused runs. Neither result explains or erases the historical full427 result of 426 passes and one read-only delayed-waiter enrollment failure.

The three failures are original methods:

- `vm::tests::partial_start_failure_keeps_later_child_reachable_for_cleanup`: its later worker received `CancelAfterFailure` while the unchanged assertion at vm.rs:6647 expected `Cancel`; the outer `later_cancelled` assertion at 6665 then failed. The first started worker subsequently reported a disconnected rescue/release channel during test unwind.
- `vm::tests::production_wrapper_cleans_fork_after_boundary_restore_failure`: the original direct `Error::InvalidGuestAddress` match at vm.rs:6461 failed.
- `vm::tests::production_wrapper_cleans_tool_thread_after_boundary_restore_failure`: the same direct error match failed. Both methods panic before their remaining explicit pending-start/owned-child cleanup assertions. This output alone therefore does not establish whether cleanup behavior succeeded; production code and the original contracts need separate diagnosis. The exact returned error variant is not printed by those assertions.

The actual test ELF is the already emitted source-v23 library at local HEAD 9db60ab95587d4cb5e0438dfeca409471eb9baf5, SHA256 daf7a52b631f19790dcac4e1d1fbbbd3326683ca3305a424db6befedb25cee8b, 116212232 bytes. Root independently proved this complete tree is identically landed in https://github.com/rrnewton/reverie/pull/577 at c8f4ca9d2e95460e027678ff23f6dec2529d255d. No source, assertion, deadline, branch, ref, Cargo lock or binary was changed for this attempt; there was no compile or relisting.

The actual unfiltered libtest arguments were exactly the original `--test-threads=1 --nocapture -Z unstable-options --format=json`. The original 30 aggregate CPU-second / 60 wall-second / 16 GiB / zero-swap limits remain, with the original 1 MiB fatal stderr and caller read bounds. Raw stdout was 210301 bytes and stderr 10696 bytes, bounded and untruncated. The original full-suite JSON parser retained every individual outcome before the nonzero-exit check. The current hardware-admission copy differs only in accepting these exact full-suite arguments; it still requires a real character-device open, KVM API 12, unchanged device and ELF identities, and execution inside the authenticated service. `REVERIE_REQUIRE_KVM=1` was passed, no KVM-unavailable diagnostic appeared, and admission recorded the same full argv and payload exit 101.

The observed service was safehermit-20260917T180412Z-1929616.service, actual PID 1930173, with complete CPU accounting of 8.569336 seconds and observed wall time 16.095472791 seconds. Libtest reported 15.186689503 seconds. Payload and admission returned 101. Observer error and resource stop were null; the service was inactive with MainPID 0 and an empty cgroup. The caller recorded its terminal readback before rejecting the failed test command. Two fresh independent systemctl readbacks also found inactive/empty state. Comparison eligibility is false because the command failed; it is not relabelled as success.

Complete source, all 130 explicit inputs, emitted ELF, inventory, clean tracked/index bytes and exact HEAD matched before and after execution. The source-after record retains explicit input rehashes. The original full427 caller/result, four focused first outcomes and all prior native/VM/static evidence stay unchanged. No retry was made. These are Reverie component results, not Hermit strict INFO, repeat determinism or cross-backend parity measurements.

Evidence:

- plan.json: 33d6c0d66bbe64f3395c5b47b5036e71ffc17a4bf14d88c98b93cf3a7441e749
- launch.py: 0cc7673efac1102aa9ecc112fea145122b2c4c232744f7e25ada8228ecc3c6bd
- admit.py: f34e3086e2f05e5f234bde6475e71c0fae8da0f0f11f1bf9dc2db8fa9b8aa3e5
- run-1/launch.json: c0032261309b66d73f874b97c9c16e21af98c6f9923540acfa1bf0219d2da5a2
- RESULT.json: 6820d7b61eeff749f1d7ed8cbb6dc799ce2ca6085a6e9cede0917b458b750a9a
- TERMINAL-READBACK.json: aee2525b118647cce6a031be0889b39b6186ebf48d5ca4a752f13369367906ed
- run-1/native-outcomes.json: 95bb773735d7301bb25bb7a349edcebf08188433792b50d43b0fac3e3610fb62
- raw observer result.json: 784d882b8bb4d6db0708c2e27f866d3a65b5129e57a721f8e0be0fcbfa1d0dba
- raw stdout: 1fbde353709acb3792055a094556fc3cebb4da533fe6933bff9181e71f6b13f4
- raw stderr: 522527c3857e0bf324f197d921d320861745741d27f45255cdff8ceaeeccc0e3
- run-1/source-after.json: a1290c05c5f6f3d7ba36b14af014dd5f20f3b1042f84d8d62720dbe9f38f3ed1

The raw result paths are recorded in RESULT.json under the dedicated measurement-prejoin-native-20260917/reverie-futex-full-library-v1/native-full directory. The next work is source diagnosis of these three newly measured failures against the original contracts. The old enrollment failure still lacks syscall timing evidence; no environmental or common scheduler cause is inferred from either full run.
