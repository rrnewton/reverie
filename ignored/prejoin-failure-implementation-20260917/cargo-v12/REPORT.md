# Source v18 compile failure

The released cargo-v12 attempt stopped at compilation; no inventory, native test, lint, VM or guest ran. The new native control uses `Errno::ENOTSUP` at `reverie-kvm/src/runtime/failure_tests.rs:241`, but this project exposes `Errno::ENOTSUPP`. Structured Cargo stdout contains one E0599 and its failure-note, with no other compiler diagnostics. The full raw stdout and stderr are retained and bounded and untruncated.

The actual service `safehermit-20260917T160647Z-3562839.service` exited 101 after 1.936583 CPU seconds and 2.606381369 wall seconds. Accounting is complete, the cgroup is empty, and the independent terminal readback reports inactive/dead with MainPID 0 and no ControlGroup. No observer error or bound was hit. Full live source/input comparison passed after terminal failure.

All later cargo-v12 stages and lint-v9 remain unexecuted; their dispatch/output absence was checked. No candidate artifact is claimed from this failed compile. The actual v16 native and qualification ELF copies remain bound retained inputs. Source v18 and this first failure are preserved before an identifier-only successor correction. No test case, assertion, selector or bound changes are required.
