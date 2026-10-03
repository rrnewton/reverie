Background: Reverie's KVM backend implements Linux system calls for a guest virtual machine. Its new epoll_pwait2 support waits only with an explicit zero timeout and no temporary signal mask, using proven eventfd registrations. This report qualifies the repair of a host-kernel error-order mismatch in https://github.com/rrnewton/reverie/pull/907. Exact native-versus-KVM results remain required on the same host; Linux6.1/6.6 and6.18 order conflicting invalid arguments differently.

Result and exact source
-----------------------

The repair passed focused unit/integration checks, the complete KVM library, the required-device refusal control, all-target Clippy with warnings denied, workspace documentation with warnings denied, and final formatting. Four deliberately broken variants each failed their selected regression. This is focused local qualification, **not canonical full validation, an independent review approval, or landing evidence by itself**. Parent-owned fresh reviews remain separate.

- Exact tested head: https://github.com/rrnewton/reverie/commit/f78354c066fc77e160216e959d1fb979c0348696
- Base: https://github.com/rrnewton/reverie/commit/a28f7cd0b8b75187756b2a7681ca8536e47b2fe4
- Tree: `57f9d3fb658204102b410b373143a2db44c3b59d`; KVM subtree `64cfd5ef35cd8d3bd957cd90c66c620116b05d28`.
- The repair checkpoint a44ae04cf06d2ee8e64ca35ef129c35a9f181480 was mechanically replayed with the original feature commit. Both range-diff rows are `=`. KVM and shared Cargo inputs are byte-identical across that replay; only the two already-authorized ptrace paths differ. See REPLAY.json and RANGE-DIFF.txt. Every product qualification phase below is at f783.
- Cargo.lock is unchanged, SHA256 `f19530a3aa1d79d5771cdf17d36d127922495973ab7cbfc56a726326e8cecee6`; its complete bytes are included. SOURCE-FINAL.json binds all five final PR source files, shared inputs and complete diff; each phase receipt also binds source, lock, command, test executable hashes and process generations.
- The parent pushed f783 and verified remote head/main. This worker did not push, merge, alter policy or run canonical validation.

What changed
------------

For an absent guest descriptor, the handler now invokes host syscall441 with guaranteed-invalid host fd -1. It preserves signed low32 maxevents, including invalid/oversized values. Before projecting the output pointer to NULL or usize::MAX, it checks the entire original positive-count guest numeric range, retaining the established four-level guest address policy. The largest product is INT_MAX*12 =25,769,803,764 bytes, so it cannot wrap x86-64 usize. No epoll object, event list, signal mask or output buffer is reached on this invalid-fd path. A host errno is preserved, including unsupported/denied errors; an impossible nonnegative host result becomes ENOSYS rather than fabricated readiness success.

The successful valid-fd path, count/range/provenance admission, retained writable-alias copyout, one-shot behavior, terminal failure handling and notification order are unchanged. SUCCESS-PATH-BINDING.json records the sole handler substitution. The helper has no global kernel-version cache and no uname-based requirement.

The integration fixture's mode14 now receives one exact host-native expected errno as explicit input, identical for native/direct/Host/Tool execution. The guest never calibrates its own expected value. All16 modes, the full64-byte report,8192-byte arena,4096-byte read-only timeout and all native/KVM byte comparisons remain intact. Unknown native setup outcomes fail qualification; there is no skip, accepted guest-errno set, exception to equality, sorting, lower count or changed deadline.

Kernel facts and environmental scope
-----------------------------------

Primary Git blobs were independently fetched and verified; files and digests are included under primary/.

- Linux6.1: https://github.com/torvalds/linux/blob/830b3c68c1fb1e9176028d02ef86f3cf76aa2476/fs/eventpoll.c#L2221 checks count, numeric range, then fd.
- Linux6.6: https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/fs/eventpoll.c#L2283 has the same order.
- Linux6.18: https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/eventpoll.c#L2451 checks fd first, then count/range/type.

The current host native setup observed EBADF/EBADF (`[-9,-9]`). No real6.1/6.6 kernel was executed for this repair. The forced legacy/current contexts below are adapter model/seam coverage, not older-host qualification. No project-wide minimum Linux6.18 is claimed. Different kernel ABIs are different environmental inputs; exact agreement is required between native and KVM under the same input. The handler propagates unsupported host syscall results rather than claiming coverage on a host that lacks441. Ordinary host signal delivery or policy auditing is not described as absent merely because this path does not touch epoll state.

Actual qualification at f783
---------------------------

The population was declared in DECLARED-INVENTORY.json before execution, not inferred from a successful output count.

| Phase | Actual result | Guarded wall | Libtest wall |
|---|---:|---:|---:|
| focused-units |14 passed,0 failed |25.41s |0.40s |
| focused-integration |6 passed,0 failed |34.45s |5.57s |
| final-library |1114 passed,0 failed,0 ignored |89.83s |63.82s |
| required-kvm |1 passed,0 failed |30.01s |see raw receipt/log |
| final-clippy-serial |exit0, `--all-targets -- -D warnings` |17.71s |not applicable |
| final-docs |exit0, workspace `RUSTDOCFLAGS=-Dwarnings` |15.77s |not applicable |
| final-format |exit0, `cargo fmt --all -- --check` |4.25s |not applicable |

The full library includes the14 focused tests; these are repeated executions, not additional unique tests. Six source-bound integration tests execute17 native guest fixtures and54 KVM guests (18 each direct, Host-owned Tool, Tool-owned Tool), retaining62 exact syscall callbacks across the two Tool paths. Those counts follow the unchanged asserted mode/route population and successful exact named completion; they are not a new consumer observation/logging guarantee. Mode14 adds no guest syscall. Independent ABI setup contributes two host-only calls in the unit native-order test and two in the integration ABI test; the full library repeats the unit setup. These setup calls are separate from the17 native guest fixture executions.

Three added unit tests:

- `epoll_pwait2_invalid_fd_projection_preserves_both_kernel_orders`:12 operand rows ×2 upper-word patterns ×2 kernel-order models =48 strict contexts. It checks exact raw fd/count/pointer/timeout/mask operands and the exact result.
- `epoll_pwait2_invalid_fd_preserves_denials_and_refuses_impossible_success`:four exact denial/error outcomes and three impossible nonnegative outcomes, each with exactly one raw-call attempt.
- `epoll_pwait2_absent_guest_fd_cannot_poll_an_ambient_host_epoll`:creates only an owned host epoll alias at a number absent from the guest table, without changing host standard descriptors. Both high-word variants return EBADF with unchanged complete guest-page bytes, and the real guest epoll still delivers its event.

Causal controls and exact restoration
------------------------------------

MUTANT-PLAN.json and run_mutants.py specify the unchanged selected tests and bounded commands. Each row ran exactly one test, with0 passed/1 failed/1113 filtered and literal exit101; no compile failure was mistaken for a regression failure.

| Mutant | Expected failure witnessed | Guarded wall |
|---|---|---:|
| fixed-ebadf |legacy-context exact result fails |24.38s |
| fixed-einval |current-context exact result fails |24.15s |
| invalid-pointer-as-null |full-original-range raw pointer assertion fails |25.61s |
| false-success |impossible host success is wrongly exposed |25.07s |

Every mutant retained its diff, source/ELF hashes, output and restoration receipt. Mandatory restoration uses saved exact bytes and cannot be interrupted by a cancellation handler raising an exception. POST-MUTANT-RESTORATION.json proves every declared source/lock hash and tracked status matched f783 before the full library rebuilt and ran. No mutant binary is represented as final product evidence.

Preparation failures and canceled overlap retained honestly
-----------------------------------------------------------

- `format-artifact`: exit1 because the private source copy lacked referenced sibling module process_signal_publication.rs. No tracked source was changed by this preparation failure.
- `format-complete-artifact`: exit1 because the initial partial dependency copy still lacked child_wait.rs. Immutable complete module context was then copied for formatting; `format-module-complete-artifact` exited0. Both errors/logs and the immutable formatting dependency manifest are retained. These were source-copy preparation failures, not product tests or an assertion relaxation.
- `checkpoint-commit`: Git itself exited0 and created a44ae04c, but the guard correctly returned94 because an owned descendant remained live after the command leader exited. The recorded exact generations were terminated/reaped, and the final census is empty. This guard failure remains a failure; it was not repeated or relabelled green. No unrecorded identity/cause is assigned to that descendant.
- `final-clippy`: an orchestration mistake launched this command before required-kvm had reported terminal completion. Required-kvm started10:43:16 AM ET and wrote its terminal receipt10:43:46.620544 AM ET. Clippy started10:43:32 AM ET; its log shows both Cargo lock waits and `Checking reverie-kvm`, so it did reach checking. The generation-bound stop record was written10:43:53.881258 AM ET, and Clippy's receipt10:43:54.375649 AM ET records actual command−15, guard94, cancellation and empty final descendants. Guard start labels have one-second precision; the stop-record timestamp is its file write time immediately after signaling, not an invented exact signal timestamp. See OVERLAP-ASSESSMENT.json and CLIPPY-OVERLAP-STOP.json.
- After both guards were terminal, corrected `final-clippy-serial` passed. The passed required-KVM test was not repeated to erase the overlap, and no source/assertion changed. **Not every historical phase was serial.** All phases after this corrected handoff, including docs and final formatting, were serial.

All guard/resource/descendant receipts remain included. Each guard applied fresh admission at280e9 free bytes, one-second stopping below265e9, nice10, at most8 available CPUs, Cargo jobs8, debug0, incremental0, private target/cache, bounded wall and128MiB output. This does not erase the transient overlap between two owned command trees just described.

Durability, disclosure and limits
--------------------------------

This packet is a delta to the unchanged historical5914 evidence. The complete old127-file archive, SHA256 `c91864c49e8cac1a208fa659e63718e78e55e6c32e55b5273a4a41ff36a1194c`, and all public parts remain linked at https://github.com/rrnewton/reverie/pull/907#issuecomment-5969808050 . Neither its reports nor its receipts were rewritten. The source design is also preserved at https://github.com/rrnewton/reverie/pull/907#issuecomment-5970025138 . This report is the single canonical new qualification report.

Implementation model: inherited gpt-6-astra. The actual who-am-i resolver remained degraded and emitted `[hermit2, unknown-agentname, gpt-6-astra, devbig014, role=impl]`; the exact unresolved tag was preserved, not replaced by an invented identity. Times here use the owner's Eastern-time policy. This worker's report is not an independent approval.

Determinism: no consumer runtime or record/replay repair is claimed. https://github.com/rrnewton/hermit/issues/3574 remains separate, as does the absent epoll output extent logging in https://github.com/rrnewton/hermit/issues/3633 . The original seven-check consumer fixture and broader producer byte-exact arenas remain distinct evidence.

Linux/POSIX: scope stays explicit zero-time, NULL-mask, proven native eventfd interests with modeled descriptor/table history. Unknown/imported/escaped/unsafe histories, blocking waits and temporary masks remain refused. The repair does not expand those claims or modify the old select/poll/epoll handlers.

Source is frozen at f783, all qualification command trees are retired, and tracked status is clean. `ignored/` is untracked evidence, not a claim that literal git status is empty. All source writes are released to root; no further execution is needed for this qualification packet.
