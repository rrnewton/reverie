# Captured-output pselect6 repair

Background: Reverie can store guest stdout and stderr in memory while retaining ordinary host file descriptors behind their synthetic pipe identities. The new zero-time pselect6 implementation was polling those host carriers, making guest readiness depend on the supervisor's output destination.

Completed 2026-10-03 08:20:31 AM ET by native worker native-kvm-pselect-capture-repair-01a0d5c2, within registered slot kvm-pselect-capture-repair-20261003 generation 1. Source and build work are complete; source writes and reads are released. The slot retains its evidence and private target for the owning coordinator's subsequent review/landing lifecycle.

Final commit: https://github.com/rrnewton/reverie/commit/d4971bca43fd65749b9d2a9c4fd5c7dee00ad505

Private branch: `codex/kvm-pselect-capture-repair-20261003`. Base: https://github.com/rrnewton/reverie/commit/2475b71bce56235663eb7d556c46aa4aa0550ee2 . Tree: `a8e896fdcb2cf6dabb7dafa6c02f5a875497febf`. The push used an explicit 40-hex refspec with an absence lease; GitHub readback exactly matches the branch head, and main stayed `2475b71bce56235663eb7d556c46aa4aa0550ee2` before and after. The branch of https://github.com/rrnewton/reverie/pull/903 and main were not pushed.

## Repair and regression

The dispatcher now carries capture_output into this pselect6 handler. The existing input validation and descriptor-existence scan run first. A selected captured-output alias then yields ENOSYS before host poll or copyout, including captured O_PATH aliases. A later missing selected fd still yields EBADF. No temporary masks, blocking calls, scheduler ordering, signal delivery, or existing select/poll/ppoll behavior changed; PRESERVED-FUNCTIONS.json proves byte identity for those three handlers and the pending-signal helper. The older select/poll capture gap is tracked separately at https://github.com/rrnewton/reverie/issues/904 .

The new unit tests plant a private regular File and an empty live UnixStream as the same captured alias, without changing any host standard fd. Same-run native/uncaptured controls prove the carrier readiness, and repaired capture calls require ENOSYS with exact whole-memory, signal, capture-byte/identity, alias and descriptor preservation. The tables also require input/fd error precedence, nfds0/out-of-range/unselected behavior, ordinary guest-fd1 replacement, and tagged/untagged O_PATH behavior.

The added actual-KVM fixture selects implicit stdout/stderr and guest-only dup aliases: four calls per successful execution route. It requires ENOSYS, unchanged bitmaps/zero timeout, an exact completion marker, and exactly four pselect6 callbacks in each Strace run. All direct, Strace Host, and Strace Tool routes run before the Rust assertions compare their outcomes. This is explicitly an unsupported-boundary test, not a claim of native parity for capture vectors. The existing 18-mode native/direct/Host/Tool suite is unchanged (18 native + 54 VM executions; 36 Tool executions each assert one callback). The new case adds 3 VM executions and 8 asserted Tool callbacks.

## Actual red evidence and limits

On unchanged production at 954769e428d7a403d8d33fd3494d6e96443d51a9 plus the new tests, all three unit tests failed, rc101. The Stdout carrier pair actually returned `[1, 0]` against `[-38, -38]`. Its native and uncaptured controls and state snapshots executed before that assertion. The Stderr pair and the post-loop captured-memory comparisons were not reached in this red run. The precedence table passed its first eight rows, then read-only output returned EFAULT (-14) instead of ENOSYS (-38); its last two write-only/except-only rows were not reached. The boundary table passed its first four rows, then captured O_PATH returned3 instead of-38; its two subsequent O_PATH rows were not reached. All these loops execute completely in the repaired passing tests.

The VM red test actually ran all three routes and each returned guest exit81 after the first stdout pselect returned1/errno0; each Strace route observed one callback. The remaining three guest pselect cases per route were not reached. The new VM test therefore exited101. These are measured failures, not predictions or relabelled passes.

## Qualification

Every command used the retained run_guard.py serially: at most8 available CPUs, nice10, Cargo jobs8, debug0, incremental0, private target/script cache, required KVM, fresh /data admission280,000,000,000 bytes, one-second stop sampling at265,000,000,000 bytes, bounded phase wall time, and at most128MiB combined output. The wrapper records actual command rc independently, snapshots source before/after, records/reaps generation-bound owned descendants, and refuses successful classification if any remain. All13 records have no stop reason, identical before/after source bindings, and no remaining descendants. Minimum sampled free space was 1,552,731,922,432 bytes. No automatic rerun-to-green occurred.

| Phase | Actual rc | Wall seconds | Terminal test count |
| --- | ---: | ---: | --- |
| baseline-build | 0 | 39.294 | no test execution |
| baseline-unit | 101 | 0.420 | 0 passed, 3 failed |
| baseline-kvm-capture | 101 | 0.821 | 0 passed, 1 failed |
| repaired-build | 0 | 27.662 | no test execution |
| repaired-pselect-unit | 0 | 0.416 | 9 passed, 0 failed |
| repaired-kvm-pselect | 0 | 3.881 | 4 passed, 0 failed |
| required-kvm-control | 0 | 0.560 | 1 passed, 0 failed |
| full-kvm-library | 0 | 59.557 | 1100 passed, 0 failed |
| clippy-kvm-alltargets | 0 | 20.894 | no test execution |
| workspace-docs | 0 | 42.751 | no test execution |
| final-build | 0 | 27.970 | no test execution |
| final-pselect-unit | 0 | 0.482 | 9 passed, 0 failed |
| final-kvm-pselect | 0 | 4.653 | 4 passed, 0 failed |

The required-device control includes the new exact test name and demands an actual /dev/kvm refusal with child exit101. Its one outer test passed; this does not reclassify the injected child failures as ordinary product success. Clippy ran `cargo clippy --locked -p reverie-kvm --all-targets -j8 -- -D warnings`. Documentation ran `RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps -j8`; Cargo emitted DynamoRIO build-cache informational warnings, while the rustdoc warnings-denied command exited0.

## Replay and evidence authority

The fully qualified repair tree was committed as9cde37e61019d61bcc0592ee42ad471bf1650802 atop954769e4. Freshly fetched main added only2475b71bce56235663eb7d556c46aa4aa0550ee2, changing reverie-ptrace/src/task.rs and its injection test. There was no KVM source/dependency/config overlap. Replaying both PR commits from old base3a196cfb9dbb775900a1acb02890b8ca10aedd0c yielded `d4971bca43fd65749b9d2a9c4fd5c7dee00ad505`; RANGE-DIFF.txt contains exactly two `=` rows. The entire reverie-kvm tree and workspace configuration are byte-identical across replay, so no second full-library run was requested or claimed.

Final-head no-run build, nine focused units, and four integration tests are bound to `d4971bca43fd65749b9d2a9c4fd5c7dee00ad505`. The 1100-test library, required-device control, package Clippy, and workspace documentation receipts remain explicitly bound to the pre-rebase repair source. Workspace docs were not rerun on the combined ptrace tree. This packet supplies focused qualification, not canonical validation, independent approval, or landing authorization.

Cargo.lock SHA256: `f19530a3aa1d79d5771cdf17d36d127922495973ab7cbfc56a726326e8cecee6`. FINAL-SOURCE.json contains all changed source/fixture, unchanged native fixture, final test binary and guard hashes. Each phase RESULT.json includes actual argv, source/binary bindings, output hash/bytes, wall/free-space samples, and descendant-retirement evidence. Full raw logs, original red patch, repair/full-PR diffs, rebase output and remote readbacks are preserved here. The exact who-am-i tag is retained in WHO-TAG.txt and is the first commit-body line; its resolver reported the agent name unresolved, which is preserved rather than silently replaced.

No assertion/tolerance/timeout was weakened, no existing parity case skipped, no failure renamed as a pass, and no check deleted. No source changes outside the new registered slot, Hermit runs, protocol edits, reviewer launches, public comments, PR updates, merges, or cleanup were performed.
