# Exact-head remote CI classification

PR: https://github.com/rrnewton/reverie/pull/605

Candidate workflow: https://github.com/rrnewton/reverie/actions/runs/35529013886 at exact head `865023d1a9d2b82320a38e17e5a83bca1c4af3ee`.

Exact-base control: https://github.com/rrnewton/reverie/actions/runs/35529400983 at `main`, which remote readback bound to the exact PR base `78203cd45751cba5f86f1e7ad5c545aceb29c017` before dispatch.

Both jobs in both workflows failed in the unchanged `reverie-dbt` library at the same three tests:

- `launcher::tests::diagnostic_file_preserves_process_group_cleanup_and_exit_status`: actual status 2, expected 7;
- `launcher::tests::diagnostic_file_separates_guest_stderr_from_diagnostics`: actual status 2, expected 7;
- `launcher::tests::unset_diagnostic_file_preserves_the_shared_stderr_stream`: output status was not successful.

Each of the four job executions reported `100 passed; 3 failed; 13 ignored`. Candidate-to-base `git diff --exit-code` is empty for both `reverie-dbt/src/launcher.rs` and `reverie-dbt/tests/fixtures/fake_drrun.sh`. No candidate-only remote failure exists before the common DBT stop. The workflow red state is therefore a reproduced exact-base failure, not evidence caused by the three-file KVM SIGCHLD patch. It remains a real repository failure and is not relabelled green.

Retained full job logs:

- candidate hardware job 106126169325: SHA-256 `73fd5dbe53e0052dbc81978eda9b7d53947b686a603cbdc9b7635f2e863af1c6`;
- candidate hosted job 106126169411: SHA-256 `9410ff17ce49d59dfae3a6748a94b1c57bdef25300e8934b2abebad59494cfea`;
- base hardware job 106127209927: SHA-256 `040de88feccdc0e042ea64824d1fe2896c47f3faf2dc0fae5a632adaf7ea06e5`;
- base hosted job 106127209822: SHA-256 `48a2e8fb0ffb7a2262b2075f390e7ea2e4bd0182008c4dd15ba829a1be916ce5`.

This control does not replace exact-head KVM qualification. The candidate's local exact-head evidence remains: 776/776 serial `reverie-kvm` library tests, 11 focused child-exit unit tests, one real static-ELF child-exit contract, all-target Clippy with warnings denied, formatting, and diff check.
