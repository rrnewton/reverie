# Sparse KVM guest-memory snapshot handoff

## Repository state

- Registered slot: `/home/newton/work/dev-hermit/worktrees/slots/kvm-vfork-optimization`
- Local branch: `kvm-vfork-optimization-reverie`
- Base: `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`
- Local commit, not pushed: `3c2315e91baacce8f81137aea5d58efa898d68db`
- Tree: `7ead6cd2c213a0b851b6d699020e008f3d99ad4e`
- SHA-256 of `git diff --binary 8c8c0a57..HEAD`: `e58391aee589d562b8481d66260666051046a865a26cd14c0bc8185a0de55344`
- Effective base diff: only `reverie-kvm/src/memory.rs`, 284 insertions and 13 deletions.
- The old vfork-specific semantic changes in `executor.rs`, `runtime.rs`, `vm.rs`, and `tests/static_elf.rs` were reverse-applied without resetting the branch. Those files are byte-identical to the base.
- Remote branch and draft PR were not changed. https://github.com/rrnewton/reverie/pull/525 remains at the rejected vfork-specific head `bac766c3aa6472081bb63566612abe04fd4b9a84`.

## Implementation

`GuestMemory` now owns a memfd backing. `GuestMemory::new` creates and sizes the memfd and maps it with `MAP_SHARED | MAP_NORESERVE`. It tries `MFD_CLOEXEC | MFD_NOEXEC_SEAL`, with an `EINVAL` retry using `MFD_CLOEXEC` for older kernels.

`GuestMemory::snapshot` preserves the existing private process-snapshot semantics while avoiding an unconditional full 1 GiB copy:

1. Create a fresh memfd-backed destination mapping.
2. Copy the `UserAccess` state.
3. Enumerate every source data extent with `SEEK_DATA` and `SEEK_HOLE`.
4. Copy each complete extent with a looping `copy_file_range` call.
5. On an unsupported operation, error, malformed extent, zero progress, partial copy, or offset inconsistency, run the prior full-mapping copy. The fallback overwrites any partial sparse result.

Parent and child use distinct memfds, so later writes remain independent. No process lifecycle, vfork scheduling, virtual-time, or syscall-classification behavior changes. The sole production caller remains `KvmBackend::snapshot_process`, reached from `prepare_forked_process` for accepted process clone forms; thread clone continues to share its existing `GuestMemory` handle.

The earlier direct vfork sharing design must not be restored. Independent review found that it held a parent until child exit rather than successful exec, made child address-space metadata private while memory was shared, and mishandled nonleader exec. This commit deliberately preserves the base behavior and changes only how the private snapshot is copied.

## Correctness evidence

- `cargo fmt --all -- --check`: pass.
- `cargo check -p reverie-kvm --all-targets`: pass.
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: pass.
- `cargo test -p reverie-kvm -- --test-threads=1`: 261/261 pass, zero failures (206 lib, 3 counter, 2 erestartsys, 41 static_elf, 3 strace, 6 vmcall).
- `cargo build -p reverie-kvm --release`: pass.

New tests cover holes, multiple distant extents, writes crossing a page boundary, the final mapping bytes, source/child write independence in both directions, preserved user-access state, a forced partial sparse copy followed by `EOPNOTSUPP` and a correct full-copy fallback, and data after `MS_SYNC` plus `MADV_DONTNEED`.

The last test exercises the backing-file path but does not prove physical eviction because `MADV_DONTNEED` is only a hint.

## Performance mechanism and prototype

The original RUN1737 `c-programs/vforkexec` result was 2,638 ms wall and 1,388,374 microseconds CPU under KVM, versus 1,068 ms wall and 157,602 microseconds CPU under ptrace. Perf attributed 74.81% inclusive cycles to the full `GuestMemory::snapshot` copy and another 6.27% self to the subsequent full zero. A warm direct full snapshot was about 113.4 ms.

Before implementation, a 1 GiB `MAP_SHARED` memfd prototype with three touched pages copied all data extents in 0.098 ms using `SEEK_DATA`/`SEEK_HOLE` plus `copy_file_range`, versus 113.4 ms for the full copy. The source used 24 blocks, the destination 6,144 blocks, and the bytes matched exactly. A diagnostic release-mode snapshot of a 64 MiB mapping with three distant touched regions took 432.349 microseconds; this is diagnostic, not the canonical benchmark.

## Canonical vforkexec A/B

Evidence: `/home/newton/work/dev-hermit/ignored/validate/evidence/kvm-sparse-snapshot-ab/run-vforkexec-15pairs-20260906/results`

`sha256sum -c DIGESTS.sha256` passes. The admitted run used devbig014, HARD reserved cgroup CPU 6, no per-task pinning, a fixed fixture, two warmup pairs, and 15 measured pairs. All 34 invocations passed on the first attempt with zero retries, timeouts, errors, cgroup throttling, or residual processes. Baseline Reverie was `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`; candidate was this local commit. The candidate Hermit binary SHA-256 was `44afeb4feb61f94fb04e5e21e18529ad6ce9027bc6b8a2454e4439d2b58c1adb`.

Median baseline to candidate:

- Canonical cell wall: 1,194 ms to 505 ms.
- Canonical cell CPU: 671.753 ms to 162.923 ms.
- Execution wall: 1,058 ms to 371 ms.
- Execution CPU: 671.753 ms to 162.923 ms.
- Outer admitted wall: 1,873.126 ms to 1,169.645 ms.
- Outer cgroup CPU: 1,596.165 ms to 984.883 ms.

Paired candidate/base geometric means:

- Cell wall: `0.4355594778511444`.
- Cell CPU: `0.25237535407743483`.
- Execution wall: `0.3669490385220973`.
- Execution CPU: `0.25237535407743483`.
- Outer wall: `0.6228378594688696`.
- Outer cgroup CPU: `0.6154233100845115`.

One retained candidate sample had 569.457 ms cell CPU; it was not filtered. Independent recomputation from the raw rows matched every reported median and paired geometric mean.

Evidence hashes:

- `summary.json`: `b748f342671539197eb272961732058105c586f49bd8ca8060293394cfcf34fe`
- `metadata.json`: `fb7e34f8d7e84dbccc2cd58ee6fc00f9f04e94b5d26d023910c54e933fd3151a`
- Raw rows: `fb9be1064abe63dbffb2ab7e7bab71d6d0eefc46bf32bbee2a0926388ad01098`
- `README.md`: `ba5214ca0602207852cf424a87fa2fb5ff4885a149a181463e577516b2c77d32`
- `DIGESTS.sha256`: `94f0f42d58a84cc10fd186c5b09a941cbc551b194ee58af88f8198904bd70998`
- `COMPLETE`: `1cbb1785bbbe49a31f7122702e41ac1f1c76b834902367b67735f8d5f8862e20`

The parent is running or appending canonical `c-programs/racewrite-nostdlib` and no-fork `c-programs/hello-nostdlib` results. Do not claim those are complete from this handoff.

## Population boundary

RUN1737 contained 222 unique selected KVM/verify cells; the sorted identity SHA-256 was `0167cc218ebf0ae3fcbcfec11afae9d94a3d849d85f959f8de3bf8f509f114d5`. The earlier vfork-only census found only `c-programs/vforkexec` used vfork semantics: 177 cells were checked from retained logs and 45 through source inspection (43 repository-local and two external programs, `/usr/bin/du` and `/usr/bin/find`).

This sparse-copy change affects every process snapshot, not only vfork. Directly measured selected examples include `c-programs/vforkexec`, `c-programs/racewrite-nostdlib`, `c-programs/ptrace-attach-eperm`, and `c-programs/nanosleep-par`; source inspection also found selected `c-programs/dbt-execveat-unsupported`. That list is not a complete cell census after the pivot. The source-level production caller enumeration is complete.

## Review boundaries

1. Each live guest address-space snapshot now owns one host memfd, closed with its mapping.
2. Initial allocation requires Linux `memfd_create`. Sparse-copy syscall failures have a full-copy fallback, but memfd creation itself has no anonymous-mapping fallback. This matches the KVM/Linux platform boundary and should be stated in review.
3. `SEEK_DATA` may conservatively classify holes as data, which can only reduce the speedup. It must not omit bytes that read nonzero.
4. Immediate dirty `MAP_SHARED` writes are covered. The `MS_SYNC`/`MADV_DONTNEED` case covers backing-file reads but does not prove eviction.
5. KVM guest writes bypass `host_access`, but process snapshot runs while the source vCPU is stopped. This is the existing synchronization premise.
6. A partially completed sparse copy followed by fallback is explicitly tested.
7. Canonical evidence currently covers one host and one affected cell in this handoff. The parent is adding the fork and no-fork checks.
8. Existing vfork semantics remain known-incomplete; this change neither claims nor attempts Linux vfork fidelity.
9. Re-measure with the separate ordinary-exec `MADV_REMOVE` work after sequencing. Current exec zeroing can densify an exec'd child's memfd before a later snapshot.

## Pull-request handoff

- Local replacement body: `/tmp/reverie-vfork-pr-body.md`. It still says canonical A/B pending; append the exact evidence above and the parent's racewrite/no-fork results before publishing.
- The first body line is the required disclosure: `[hermit2, kvm-ratchet, unresolved, devbig014, role=impl]`.
- Retitle the draft from the obsolete “Share KVM guest memory across vfork until exec” to concise wording such as “Copy sparse KVM guest-memory snapshots.”
- Do not treat https://github.com/rrnewton/reverie/pull/525 as containing this implementation until the branch is deliberately updated and read back by content.
- No push, landing, approval, or self-attestation was performed.

