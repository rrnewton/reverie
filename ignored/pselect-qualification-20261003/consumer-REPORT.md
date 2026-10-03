# pselect6 consumer preflight

Background: Hermit's Detcore syscall layer mediates a guest's readiness queries and owns deterministic scheduling and virtual deadlines. Reverie's KVM executor supplies the immediate descriptor-readiness sample. This source-only check asks whether the unchanged `c-programs/poll-readiness` strict verification cell can use the proposed bounded pselect6 repair. No command below was executed, no native probe or build ran, no checkout/ref changed, and no released slot was accessed.

## Exact source binding

Consumer base: **9e7dd6e33cf6fc76f3b9a744f50441120b32f0d6**, read from the shared Hermit repository's `refs/remotes/origin/main`, not from its stale working-tree HEAD (`0c4c35919a8363c2d67a3f9b7c1604ada93c7cee`). This report does not claim a fresh GitHub network read. The base is one commit after the socket-cell activation at https://github.com/rrnewton/hermit/commit/5d8cf0cf1d9a087503bfe8b1df368df72b9c44d7 . Its `detcore/src/syscalls/io.rs`, entire C-program manifest, and `tests/c/poll_readiness.c` are byte-identical to that activation commit.

The consumer already pins Reverie **3a196cfb9dbb775900a1acb02890b8ca10aedd0c**, the producer repair's base: `hermit-cli/Cargo.toml:64`, and `Cargo.lock:1757-1759` for reverie-kvm. That pin has no new pselect tail. Future qualification must first compose the reviewed repaired Reverie commit into a fresh registered Hermit checkout and record that new clean Hermit SHA and pin; testing the unmodified pin cannot qualify the repair. SOURCE-BINDING.json records the exact Git blobs and SHA256 of every inspected Hermit source.

## Call path and bounded-domain compatibility

- https://github.com/rrnewton/hermit/blob/9e7dd6e33cf6fc76f3b9a744f50441120b32f0d6/detcore/src/syscalls/io.rs#L456 handles pselect6. Its lines 480-515 validate a nonnull outer mask wrapper using an injected malformed timeout. The repaired producer still returns EINVAL for the valid wrapper plus invalid timeout, and preserves wrapper EFAULT precedence. A null inner pointer ignores its size.
- At lines 518-527, an explicitly zero timeout is injected directly. **The target cell's zero-time calls do not enter InternalIOPolling.** This is the ordinary non-record/replay sequentialized path; non-sequential mode also injects directly at lines 472-474.
- The longer-wait path preserves the one-word bound at lines 530-538. It snapshots a real mask or treats a null inner mask as absent (549-563), clears the original wrapper pointer (566), and enters `handle_internal_pselect6` (572).
- Each internal probe has a scratch zero timespec (613, 632, 651-657), original fd-set snapshots restored before injection (658-660), and no wrapper unless a real inner mask existed (618-626, 633). Thus an original unmasked call with nfds <= 64 produces exactly the repaired zero-time/unmasked form. The scheduler request is InternalIOPolling (635-649); virtual deadline progression stays entirely in Detcore (580-583, 674-690). A real temporary mask remains deliberately unsupported by the producer.
- **ENOSYS is not retried or translated to success.** `pselect6_probe_result` only converts ERESTARTSYS to EINTR (111-118). In the loop, any result other than Ok(0) exits (662-671); errors do not copy scratch result sets. Therefore our zero-ready/pending-guest-signal guard propagates an explicit unsupported result, without an endless retry. For an original nonzero timeout, Detcore may update that original timeout before returning ENOSYS (669 and 705-726); the producer's no-effects guard is not a promise of no consumer-level remaining-time writeback. An original zero timeout uses direct injection and performs no such Detcore writeback.

The target fixture is https://github.com/rrnewton/hermit/blob/9e7dd6e33cf6fc76f3b9a744f50441120b32f0d6/tests/c/poll_readiness.c . It creates one pipe (109-113), writes five bytes, checks readable/writable states, drains it, and closes the writer. Every select uses an explicit zero timeval (72-73, 87-88); ppoll uses a zero timespec and NULL mask (98-101). It creates no guest thread, child, alarm, handler, or guest signal. Consequently there is no deliberate eligible guest signal to hit the new ENOSYS boundary. If one appears, the cell fails visibly: the fixture retries only EINTR, and any other select error reaches `fail` (74-76, 89-91).

The source calls libc `select`, not a hardcoded raw syscall. The prior failure and current platform lowering reach pselect6, but this audit did not execute or disassemble libc. Retain the real INFO log to establish the actual injected syscall and nfds on the consumer. The fixture has only its standard descriptors and one pipe, so the intended guest descriptor allocation is below 64; no hardcoded new fixture assertion is proposed. A run showing nfds > 64 is an incompatibility to report, not grounds to widen or bypass the producer boundary.

## Exact manifest selection and future commands

https://github.com/rrnewton/hermit/blob/9e7dd6e33cf6fc76f3b9a744f50441120b32f0d6/tests/e2e/manifests/c-programs.yaml#L14796 defines `c-programs/poll-readiness`, portable lane, `verify`, KVM currently disabled at line 14820. The replay row at 14828 remains separate and must stay off from this evidence. The unchanged fixture requires **poll_readiness ok=8** and exit 0 (159-169), so repeatably failing the same checks cannot falsely pass strict verification.

Execute only after root's ordinary source/pin/review and disk/CPU admission, from the new clean registered consumer checkout. The following are inner command payloads for that existing guard/proxy route, not authorization to run now. Keep at most 8 selected CPUs, `nice -n 10`, start /data >= 280e9 bytes and stop the owned process group below 265e9; no full validation is proposed.

```bash
env CARGO_TARGET_DIR=target/pselect-consumer CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 nice -n 10 cargo build --locked -j 8 -p hermit --bin hermit -p hermit-manifest-plan --bin test-harness --bin hermit-manifest-plan > ignored/pselect-consumer/build.log 2>&1; rc=$?
```

The directory `ignored/pselect-consumer` must be created before redirecting, and `rc` captured and recorded before reading the log. Then the disabled-cell diagnostic (official E2E harness, so no extra safehermit wrapper) is:

```bash
env -u E2E_IMPORT_RESULTS -u E2E_PARITY_SELECT -u HERMIT_TEST_CPU_TIMEOUT_MULTIPLIER -u HERMIT_TEST_WALL_TIMEOUT_MULTIPLIER E2E_RESULT_ROOT="$PWD/ignored/pselect-consumer/e2e" E2E_RUN_ID=pselect-disabled-001 E2E_RUN_INDEX=0 E2E_KEEP_VERIFY_LOGS=1 HERMIT_BIN="$PWD/target/pselect-consumer/debug/hermit" nice -n 10 target/pselect-consumer/debug/test-harness run --repo-root "$PWD" --lane portable --test c-programs/poll-readiness --mode verify --backend kvm --probe-disabled --no-retry --jobs 1 --results "$PWD/ignored/pselect-consumer/disabled-results.jsonl" > ignored/pselect-consumer/disabled-run.log 2>&1; rc=$?
```

`--probe-disabled` selects exactly the disabled row and cannot be combined with `--ci-only`; see test-harness.rs:848-863. Do not use `--source-sha` for a real tracked checkout, `--allow-empty`, imported results, a timeout multiplier, or an alternate comparator. The default unchanged execution bounds are **22 CPU seconds / 57 wall seconds**, from `ci/manifest-plan/src/timeouts.rs:12-14`.

After a passing diagnostic and the authorized manifest flip/generation bookkeeping, qualify the clean enabled commit with the same command, changing run ID and result/log filenames and replacing `--probe-disabled` with `--ci-only`. Expected classification is then required; the earlier disabled diagnostic must not be relabelled as required. Keep `--no-retry --jobs 1`.

The harness itself compiles the unchanged fixture with `cc -std=c11 -O2 -g -Wall -Wextra -Werror` (`runner.rs:3042-3066`), selects the explicit HERMIT_BIN (`484-492`), and constructs the strict KVM verification invocation (`3509-3563`) with INFO logs, `--backend kvm`, `--base-env=minimal`, strict verification, and typed JSON evidence. Do not manually replace that invocation with a bare Hermit call.

Pass requires one actual spawned KVM attempt, no import, rc0, row PASS with no failure, exact stdout `poll_readiness ok=8\n`, typed verified=true and bitwise_parity=true/matched with strict INFO and I/O-buffer comparisons, nonzero comparison population, and unchanged bounds/relaxations. Preserve the typed report and golden INFO log; the current verifier may remove the second identical log after matching. A single-backend strict match is not ptrace-versus-KVM parity. Preserve ERROR/infrastructure/refusal or FAIL results as such; do not retry-to-green.

## Conclusion and remaining limits

No consumer source incompatibility was found for the unchanged signal-free zero-time readiness cell. The consumer already presents the supported syscall form, and its error handling fails explicitly for the excluded pending-signal condition. This does not establish full pselect signal/restart semantics, masked waits, raw blocking waits, large fd tables, record/replay, or the cell's actual runtime outcome. The real guarded consumer run and its retained syscall trace remain required.
