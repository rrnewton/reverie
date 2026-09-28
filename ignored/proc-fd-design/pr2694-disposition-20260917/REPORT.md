# Preserve and repair existing Hermit PR2694

Verdict: **keep https://github.com/rrnewton/hermit/pull/2694 open; a superseded closure is not justified.** The current main call paths still lack its inherited-stdio status containment and append conversion, and none of its 33 added Rust test identities or nine new C fixtures is present at the corresponding main paths. This is a preservation disposition, not approval of the old proposal. Its standing current-head correctness refusals remain in force.

## Bound source and public state

All comparisons use immutable Git objects from the read-only recovery slot. No source, index, ref, TaskGraph claim, or public state was changed; no build, test, or guest was run.

| Input | Exact identity |
| --- | --- |
| Public head | `46670790208e71be0b1aa0e249297c0f4b38663a`, tree `2c5ffe82932954367046ae55a43c0a94e1afa6a1` |
| Authored merge-base | `dade02611a0988aaa2f234411c16462fdbfb5519`, tree `7d2393d2149143d7056d2acf5ea1bb07076b73bd` |
| Observed current main | `1ef956b6b16a19080c94838ead0c88926f7231ff`, tree `487b6e232d3969de6c3453359d832ceed868e3fe` |
| Preserved private initializer follow-up | `046139e3affc6548054befe809f76fdafcb6846a`, parent the public head, tree `0a2a9c0008bab3c41284bcd84de95019e59dd0b3` |
| Current main Reverie pin | `b3049e54c644e28e2894402a36bb65664ab4508b` |

The 07:36:56 UTC GitHub response reports OPEN, non-draft, not merged; 22 changed files, +3,891/-164. Its base SHA field is the historical `24e15d63b2e356b878318faa968691ef3d5eb707`, not a measurement of current main. Mergeability was null/unknown. A final whole-proxy `ls-remote` at 07:45:06 UTC confirmed public branch `fix/kvm-verify-stderr-oappend-leak` still at the head above and main still at `1ef956b6`. Full raw metadata, all 31 comments, and the empty formal-review array are retained.

Canonical comments retain two current-head refusals. In particular, read the full reviews at https://github.com/rrnewton/hermit/pull/2694#issuecomment-5656780761 and https://github.com/rrnewton/hermit/pull/2694#issuecomment-5690782488. The later bookkeeping correction at https://github.com/rrnewton/hermit/pull/2694#issuecomment-5690790907 does not withdraw either refusal. Labels are not approvals. The existing determinization-strategy critical classification is preserved; this report invents no new attestation or landing permission.

## Current authority and closure policy

The final command was `env TG_DB_PATH=/home/newton/.tg/hermit2.db with-proxy /home/newton/work/dev-hermit/ci-hub/ci-hub active-work --json`. It completed at 07:45:07 UTC with exit 1 and a complete structured `state: drift` result. Its ORC snapshot was captured at 07:44:40.041734 UTC, about 27 seconds old. The live domain task `kvm-lane-to-full-determinism-and-parity` is present in both `claimed` and `actually_active`, owned by `Ensure deterministic KVM parity | dev-hermit`, status `IN_PROGRESS`; its notes contain the full PR URL. Unrelated drift does not erase that reported claim. The closed historical task named in the public PR body is not current ownership authority. This snapshot proves the stated observation, not indefinite future authority; refresh before any later public operation.

Read the complete current `docs/PR_SWEEP_VERDICTS.md`: 2,778 lines / 150,721 bytes, SHA256 `decb66dbe87aaaeee7c7d98152b774789045c0077eb625f4407ca9f3881f8c26`. Its merge-base/content/residual rules apply: preserve unique error handling and tests, inspect dependency implementations, and do not overwrite newer main. Its historical ownership prose at lines 173–199 and corresponding checklist language conflicts with the current parent AGENTS rule that TaskGraph plus a fresh snapshot is the sole authority. The parent rule governs; PR prose and closed tasks are history only. Root has been told of this conflict. No closure-specific condition changes the concrete result here: the core authored work is absent, not merely a duplicate with an unlanded test.

## Complete 22-path preservation map

`public-complete.patch` is the complete merge-base-to-head diff: 208,319 bytes / 4,652 lines, SHA256 `54e26f4f5c1255df691cd78343bb6a82865d584afc1fe62b020ad24734c45e92`. Every changed path was read, including all generated semantic changes. `content-map.json` binds base/head/main blobs and bytes; `authored-test-identities.json` records every one of the 33 added Rust identities. Distinctive mechanism searches and current callers supplement same-path absence; this is not an ancestry-only conclusion.

| Authored path | Behavior or check that must survive; current-main disposition |
| --- | --- |
| `ci/dag/validate.json` | Generated populations and canonical/hosted commands. Exactly 20 changed leaves; old totals must be regenerated against the eventual composed source, not copied over newer main. |
| `ci/manifest-plan/src/validation_dag_static.rs` | Matching population assertions and KVM 24-to-33 command/count changes; retain all current selectors and bounds when composing. |
| `detcore-dbt/src/lib.rs` | Propagate initializer refusal through real eager/lazy ABI entrypoints before state publication or guest-memory probes. Four added controls include KCMP EPERM/EBADF and unchanged guest/result sentinels. Current main is byte-identical to merge-base. |
| `detcore/src/fd.rs` | Separate logical status updates from physical scheduler nonblocking. Current main is byte-identical to merge-base. |
| `detcore/src/lib.rs` | Carry the typed inherited-alias error and refuse startup before scheduler RPC. Current main has other changes, but no replacement for this path. |
| `detcore/src/syscalls/files.rs` | Contain inherited status changes, refuse unsupported changes, read logical append after admission, convert write families to kernel RWF_APPEND, and preserve errors/releases/sendfile behavior. Current main is byte-identical to merge-base. |
| `detcore/src/syscalls/files/append_tests.rs` | New module absent. Five controls cover admission ordering, post-admission status, scratch/descriptor failure cleanup, partial/tool failures, and 84 native neighboring cases across five write families. |
| `detcore/src/tool_global.rs` | New typed initializer-error test refuses before any thread-start RPC, including repeated hook polling; absent on current main. |
| `detcore/src/tool_local.rs` | Real inherited descriptor metadata, KCMP OFD equivalence rather than inode equality, typed refusal/empty-state propagation, and three initial controls; absent on main. Preserve the private follow-up below rather than restoring its known live-discovery/raw-bit defects. |
| `hermit-cli/src/event_stream.rs` | Registers the actual record/replay append test module; current main is byte-identical to merge-base. |
| `hermit-cli/src/event_stream/append_record_replay_test.rs` | New module absent. Exercises actual EventReader/WriteV2 record and replay: six records, converted syscall ABI, recorded offsets, shared OFD position 10, exact `nrefix\nWVvPQq` bytes, zero write, exit 37, and unchanged supervisor flags. |
| `hermit-cli/tests/cli.rs` | Eighteen new tests: nine ptrace/KVM pairs for non-stdio status masking, contained nonblock/append, nonblocking stdin, supervisor stdout protection, inherited initial/direct-set flags, alias sharing, write-family append, and unsupported status refusal. All added identities absent on main. Preserve existing CLI tests as well. |
| `scripts/validate.rs` | Exact selected KVM population ratchet 24-to-33 for those nine added methods; derive the future union from actual identities. |
| `tests/c/nonblocking_stdin_recv.c` | New fixture absent: nonblocking stdin consumes the primed byte without aborting the container. |
| `tests/c/nonstdio_status_flags.c` | New fixture absent: pipe/dup flags, independent writer, EAGAIN, blocking restoration and data preserve ordinary descriptor behavior. |
| `tests/c/stdio_append_record_replay.c` | New fixture absent: six append write forms including a zero write, with exact result/exit expectations. |
| `tests/c/stdio_append_write_paths.c` | New fixture absent: sendfile, pwrite, pwritev, pwritev2, flag/offset boundaries and pipe errors. |
| `tests/c/stdio_initial_nonblocking.c` | New fixture absent: inherited nonblocking seen both by initial F_GETFL and direct F_SETFL without a prior getter. |
| `tests/c/stdio_nonblock_then_append.c` | New fixture absent: containment remains effective after nonblocking is set; supervisor O_APPEND must not escape. |
| `tests/c/stdio_status_alias.c` | New fixture absent: stdout status updates are visible through stderr only when they share an OFD. |
| `tests/c/stdio_status_flag_containment.c` | New fixture absent: logical append plus real writes, and exact refusal to clear preexisting physical append. |
| `tests/c/stdio_unsupported_status_flags.c` | New fixture absent: O_ASYNC/O_DIRECT/O_NOATIME changes fail with exact EOPNOTSUPP and unchanged state. |

The four exact-base main blobs are `c96b43fd56dc91777e1567bc3a49b98cf63cf8ab` (files.rs), `89eca566c1614d5d8cc66fbfdfccdca108a5c331` (fd.rs), `f716831c71797e6ecc91534859d408c04902f390` (detcore-dbt/lib.rs), and `86b755faa3ab190359d961450201cfa39dd8ef67` (event_stream.rs). At current `files.rs:2724–2746`, F_SETFL still forwards the physical flags and updates the model afterward; there is no contained append conversion. Existing general OFD sharing and scheduler nonblocking behavior are prerequisites already present at the base, not proof this proposal landed elsewhere.

The 33 added Rust methods break down as four DBT + ten Detcore + one Hermit unit + eighteen CLI. The private follow-up adds four more Detcore controls. These are authored identity counts, not newly executed test totals. The old graph's 470→474 regular, 532→533 Hermit, 667→677 Detcore, 78→87 CLI and 24→33 KVM assertions are historical unions. Current main has different totals (including regular 507, Hermit 630 and Detcore 679); do not regress them by replaying stale generated values.

## Preserved repair and actual remaining mechanisms

**Private initializer repair is available, unpublished.** `046139e` changes only tool_local.rs: both stdio initialization routes use the same alias discovery; F_GETFL words use `from_bits_retain`, keeping the real Linux 0x8000 bit; live file types, physical nonblocking, flock state, failure/clone behavior and independent same-inode objects remain distinguished. Its complete 23,350-byte diff is retained as `private-followup.patch`, SHA256 `d89ebd8cd4a486541bbf1e3e9c5029c4eac8e90293862e0087096d28d75b5275`. Current main still uses the old per-fd live setup at tool_local.rs:2079–2088 and truncates flags at :708. Notes 25424/25432 preserve the bounded review and eleven focused native passes, with compilation about 40.61 seconds and Clippy about 24.97 seconds. Those are historical evidence reported by the verified task notes; this task did not rerun or rehash that old campaign. Notes 26485/26495 explicitly retain the remaining backend obligations.

**SaBRe must turn initializer refusal into actual startup termination.** At the currently pinned b304 Reverie adapter, `experimental/reverie-sabre/src/reverie_adapter.rs:594–629`, `handle_thread_start` still returns unit after logging Err/Pending. `dispatch_syscall` at :552–590 can bypass unsubscribed calls before invoking the Tool, and `shared_result` at :819–830 maps Fatal to guest EIO. Thus merely introducing Hermit's typed startup error does not prevent the real guest from continuing. The relevant 8c8-to-b304 changes add register handling, not a startup failure barrier. Repair the real adapter/lifecycle path and retain actual entrypoint controls; a mocked returned error is insufficient.

**DBT needs real fork/exec logical-OFD lifetime, in addition to the four entrypoint refusal controls.** Current `detcore-dbt/src/lib.rs:474–505` sends prepare-exec state without descriptor/OFD logical flags. The copied runtime state around :1319–1363 is process-local; process-clone completion at :1586–1606 invalidates flock data, not a cross-process logical-status authority. FileMetadata's in-process Arc clone/fd filtering does not make a heap Arc shared after a real fork or survive image replacement. Current pinned Reverie's runtime bridge remains unchanged for this mechanism; its intervening client changes are signal/getpgrp handling. Existing task notes already preserve the proposed shared authority/identity/cleanup obligations. Use those rather than claiming the private initializer repair solves real fork/exec. This is a remaining obligation of the proposed contained-status model, not evidence that base ordinary Linux flags suffer the same defect.

**A KVM prerequisite has landed separately, but qualification has not been transferred.** Current pinned b304 has `reverie-kvm/src/executor.rs:364–366` dispatching pwritev2 to vectored I/O, validation around :4049–4082, and the owned-descriptor/capture/position implementation around :4180–4222, including real RWF_APPEND and offset -1 handling. The old public PR's 8c8 pin lacked that route. The already-landed vector work therefore supplies a prerequisite for a future current-main composition; it does not repair the frozen public pin or prove its proposed conversion on KVM. The public append-family CLI test selected only sendfile on KVM because of the old limitation. A composed repair must exercise the now-admitted append forms, while preserving explicit captured/synthetic refusal boundaries and the existing sendfile check. No append qualification was executed here.

## Smallest useful next increment and review limits

Continue in the existing https://github.com/rrnewton/hermit/pull/2694 lineage. Preserve all public authored paths and the private initializer repair; the latter is the smallest already-prepared source slice, but it is not independently sufficient to land the contained-status feature. Compose against the then-current main, retain the newer object/resource and proc/mount fixes from https://github.com/rrnewton/hermit/pull/3047 when present, and fix the real SaBRe startup barrier and DBT fork/exec authority before claiming those backends are supported. Do not overwrite newer main with the old tool_local/fd/files/CLI files.

Retain the actual handler/native-neighbor, record/replay, live-alias/raw-status, ABI pre-probe refusal and eighteen CLI controls. Complete the genuine backend tests for SaBRe failed initialization, DBT shared status after real fork and exec, and current admitted KVM append operations. Regenerate exact graph/count assertions after composition. Resolve the standing exact-head refusals through the existing review process; this report grants neither blanket correctness approval nor permission to publish, close, merge or claim. It adds no whole-DAG receipt prerequisite to the established checks.

Goalpost assessment: no assertion, comparator, timeout, label or gate was changed in this task. Treat the existing narrow O_NONBLOCK exception as its stated scope, not proof of total supervisor flag isolation; retain the O_APPEND oracle. Preserve exact errno/error cleanup and the originally failing backend behavior. Do not relabel old KVM sendfile-only coverage as the newly available full append coverage, or count private/native/source equality as public/current guest success. Current refusal findings stay historical facts even where a separate newer dependency supplies one prerequisite. The nine fixtures and 33 added checks cannot be discarded merely to retire a stale branch.

The full required review rule was applied to the complete authored content and the proposed preservation disposition:

> GOALPOST-MOVING REVIEW RULE
> Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
> Treat each of these as an explicit review target:
> - weakening an assertion so a test passes
> - widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
> - renaming or relabelling so a failure reads as a pass
> - deleting a check rather than satisfying it

The public diff, source copies, exact content/test maps, complete raw network responses, relevant task notes and dependency bodies are bound alongside this report. No current product runtime result or base runtime comparison is inferred from this source analysis.
