[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

Remote CI is red, and this comment does not relabel it green.

Candidate workflow https://github.com/rrnewton/reverie/actions/runs/35529013886 ran at exact PR head `865023d1a9d2b82320a38e17e5a83bca1c4af3ee`. Both hosted and self-hosted jobs stopped in the unchanged `reverie-dbt` library with the same three diagnostic-FD failures: two statuses were 2 instead of 7, and the unset-diagnostic-file case returned a non-success status. Each job reported 100 passed, 3 failed, and 13 ignored.

Exact-base control https://github.com/rrnewton/reverie/actions/runs/35529400983 ran at remote `main`, verified immediately before dispatch as the exact PR base `78203cd45751cba5f86f1e7ad5c545aceb29c017`. Its hosted and self-hosted jobs reproduced the identical three test names, values, and 100/3/13 count. `reverie-dbt/src/launcher.rs` and `reverie-dbt/tests/fixtures/fake_drrun.sh` are byte-unchanged from base to head.

Classification: real pre-existing repository failure, not a candidate-only failure and not KVM SIGCHLD evidence. Full job logs and hashes are retained. The exact-head KVM qualification remains the scoped landing evidence: 776/776 serial `reverie-kvm` library tests, 11 focused child-exit unit tests, one real static-ELF child-exit contract, all-target Clippy with warnings denied, formatting, and diff check.

Full local `validate.sh` is not claimed and the `locally-validated` label is intentionally absent because rebuilding the prior 19.4 GiB all-feature cache would cross the mandatory 400 GiB free-space floor at current headroom.
