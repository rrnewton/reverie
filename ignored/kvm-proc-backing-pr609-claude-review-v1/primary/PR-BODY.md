[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain Language Summary and Project Impact

KVM synthesizes several stable `/proc` files instead of exposing host process state. Those snapshots previously sat behind a writable anonymous backing description, even though guests saw them as read-only files. This change builds each supported snapshot in a sealable memfd, sets mode 0444, applies the permanent write, grow, shrink, and seal seals, and only then publishes a fresh read-only or `O_PATH` description to the guest. The writable construction description never enters the guest file table.

The change covers the 17 fixed paths produced by `synthetic_proc_content` and the already-supported readable dynamic fdinfo descriptions. It also aligns the scoped direct-open, proc-fd, metadata, `O_TMPFILE`, create-directory, and low-descriptor-word behavior with Linux. It is a prerequisite for later authenticated proc-carrier identity work; it does not implement `syncfs`, SCM_RIGHTS identity transport, or full backend parity.

## Determinism

Snapshot bytes can no longer be changed through a writable alias, grown, shrunk, or made sealable again after publication. Synthetic proc metadata remains run-stable, and failed fdinfo opens do not allocate a carrier or sequence state. Fork, exec, dup, close, and dead-leader/live-worker paths preserve the established per-description lifecycle state.

The exact required seal set is `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`; the kernel-added `F_SEAL_EXEC` variant is accepted when host policy supplies it. `/proc/self/loginuid`, deterministic random devices, and synthetic sysfs remain on their existing virtual-file path.

## Linux Semantics

The open path now validates complete non-`O_PATH` `__O_TMPFILE` forms before pathname access, including the private bit, required `O_DIRECTORY`, forbidden `O_CREAT`, and write-capable access modes. The loader probes and caches whether its host follows Linux's pre-6.4 `LegacyLookup` or current `EarlyEinval` create-plus-directory ordering, fails closed on any other result, and preserves that selection across fork and exec.

For fixed synthetic proc files, `O_PATH` is normalized to Linux's admitted `O_PATH`, `O_DIRECTORY`, `O_NOFOLLOW`, and `O_CLOEXEC` subset; read-only opens preserve supported status bits. Dynamic fdinfo now separates live pathname existence from content support, so metadata and directory/create/TMPFILE errors do not collapse into an early `ENOSYS`.

Known residuals are explicit. Linux permits bare fdinfo `O_PATH` opens, including `O_PATH|O_NOFOLLOW` and ignored legacy bits, while Reverie retains its pre-existing `ENOSYS`; this change does not claim fdinfo `O_PATH` lifecycle, link, or status parity. Fdinfo `O_TRUNC`, `O_DIRECT`, and bare `O_CREAT|O_EXCL`; direct `/proc/mounts` and proc-fd link identity; SCM_RIGHTS synthetic identity; mmap type and `MAP_SHARED_VALIDATE`; later `mprotect(PROT_WRITE)` provenance; and noncanonical aliases remain outside this patch. No Linux 5.8–6.3 runtime result is claimed.

## Validation

All build and test commands used a 180-second outer timeout and disabled incremental compilation. The frozen source and binary patch received two independent full adversarial approvals, a separate lock/lifecycle audit, and a focused native fdinfo `O_PATH` scope audit before commit.

- `cargo fmt --all -- --check`: passed in 2.092 s
- focused synthetic-proc library tests: 31 passed, 0 failed in 23.167 s
- exact seal control: 1 passed, 0 failed in 0.107 s
- dead-leader/live-worker generation control: 1 passed, 0 failed in 0.110 s
- required real-KVM static-ELF test with `REVERIE_REQUIRE_KVM=1`: 1 passed, 0 failed in 29.680 s; KVM absence cannot skip
- full `reverie-kvm` library: 812 passed, 0 failed in 8.650 s
- strict all-target Clippy: passed in 9.603 s
- native matrices: 4,896 direct-open, 60 proc-fd, 180 tmpfile-surface, 36 global-order, 99 complete-tmpfile, 18 create-directory, 48 ordinary-`O_PATH`, and 25 live-fdinfo cases
- six causal source mutations each failed its intended exact assertion and were reverted with source-hash verification
- `git diff --check`: passed

The exact binary patch SHA-256 before commit was `356462ca55635b56f572697db8d5e935a6915debb147350b7f587f4d148be0fb`. The host was x86-64 Linux 7.1.3. Legacy behavior is backed by upstream Linux commit https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58 and injected exact-result tests, not a fabricated old-kernel run.

## Relationship to gVisor

This changes Reverie's direct KVM static-ELF syscall adapter. It neither changes nor borrows a gVisor execution path. The scoped behavior is checked against native Linux and the direct KVM execution path; Hermit's gVisor-derived components are unchanged.

## Human Review Required

Trigger 2: this changes Reverie's KVM syscall-interception behavior and guest-visible file-description semantics.

Trigger 3: sealed synthetic proc carriers are a determinization mechanism for guest-visible kernel metadata. Independent exact-head Codex- and Claude-family adversarial approvals are required before landing. Those reviews must treat the fdinfo `O_PATH` mismatch and all other residuals above as explicit nonclaims.
