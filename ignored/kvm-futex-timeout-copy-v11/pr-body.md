[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain Language Summary and Project Impact

KVM previously left a guest futex timeout pointer mapped for the entire host wait and let the host kernel read that guest memory after Reverie had inspected the request. This change imports and validates the timeout before either futex word, then gives the kernel an owned copy. It improves Linux error-order fidelity and removes the timeout-memory time-of-check/time-of-use gap needed for robust KVM execution.

The entry fence already closed with retained operands before this change. The measurable ownership change is narrower: a sleeping futex retains one guest word rather than that word plus the timeout mapping. Publication still remains blocked by the retained word.

## Determinism

The timeout value becomes fixed when Reverie copies it, so a racing guest write cannot make validation observe one value and the host kernel consume another. The host stack value remains live for the synchronous syscall, while retained operands keep each translated futex word mapped for the call.

Tool-owned futexes continue to route through Detcore scheduling instead of this direct host adapter. Host-owned threads still use host real time and remain outside Hermit's deterministic scheduling envelope; this pull request does not claim to change scheduler ordering, wakeups, virtual time, record/replay, or full KVM parity.

## Linux Semantics

The adapter follows Linux `sys_futex` ordering for the scoped cases: it imports and validates each timeout-bearing operation before `do_futex` rejects command or clock flags and before key lookup reaches either word. It preserves argument four as an integer for requeue and wake-op commands, accepts the three Linux realtime-clock combinations, and checks guest alignment before mapping access only for the reviewed non-PI key-first operations.

PI lookup order, guest-versus-host PI TIDs, and private/shared futex key identity remain explicit residuals. The `FUTEX_WAIT_REQUEUE_PI` test no longer claims an order its inputs cannot distinguish.

## Relationship to gVisor

This patch changes Reverie's direct KVM static-ELF syscall adapter and does not alter or borrow a gVisor execution path. The behavior and tests are checked against Linux futex ABI ordering; Hermit's gVisor-derived components are unchanged.

## Validation

- `cargo fmt --all -- --check`: passed
- `cargo test --locked -p reverie-kvm --lib futex -- --nocapture`: 7 passed, 0 failed
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf static_elf_host_futex_wait_observes_finite_timeout -- --exact --nocapture`: 1 passed, 0 failed through a real KVM vCPU
- `cargo clippy --locked -p reverie-kvm --lib --tests -- -D warnings`: passed
- `cargo test --locked -p reverie-kvm --lib`: final retry 790 passed, 0 failed, 0 ignored
- `git diff --check`: passed

The new allow-side test covers all thirteen admitted futex commands and all three admitted `FUTEX_CLOCK_REALTIME` combinations. On the immediately preceding v10 test revision, a negative-control mutation that passed null instead of the owned timespec made the focused unit test fail with status 101 and the real-KVM case hit its 30-second hard timeout. That receipt is not represented as an exact-head mutation: the final test body was subsequently renamed and its diagnostics changed. The reviewed head preserves the same production pointer and exact `ETIMEDOUT` oracles, and its unmutated source observes `ETIMEDOUT` in both layers.

The first final-source full-library attempt retained one unrelated `EAGAIN` failure in `descriptor_retirement_accept_cleanup_releases_both_guards`. Its exact isolated retry passed, then the complete 790-test retry passed. An earlier pre-review run also retained an unrelated positioned-vectored pipe result of 4 instead of `EPIPE`; its isolated retry and the then-current full suite passed. Neither failure was hidden, relabelled, or removed.

There are no GitHub CI signals. A test-only follow-up should isolate the finite-timeout unit case in a self-executed process: its rescue loop can otherwise outlive its local receive bound if a future regression sleeps on an unexpected translated host address. Existing qualification commands had outer hard timeouts, and this does not change the production result reviewed here. Downstream Hermit pinning and exact-main real CLI checks through `bin/safehermit` remain separate follow-up; this pull request alone is not a delivered backend milestone.

## Human Review Required

Trigger 2: this changes Reverie's syscall-interception translation behavior for futex timeout and command validation. Independent exact-head Codex and Claude-family reviews are required before landing.
