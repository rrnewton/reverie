[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain-language summary

KVM synthesizes stable `/proc` files rather than exposing host process state. This change replaces their writable anonymous backing descriptions with sealed memfds and publishes only fresh read-only or `O_PATH` descriptions. It covers all 17 fixed paths returned by `synthetic_proc_content` and already-supported readable dynamic fdinfo descriptions.

This is a prerequisite for authenticated proc-carrier identity and `syncfs`; it does not implement either feature, SCM_RIGHTS identity transport, record/replay, or complete KVM parity.

## Safety and determinism

Each covered snapshot is fully populated, chmodded to 0444, and permanently sealed with `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL` before publication. The kernel-added `F_SEAL_EXEC` shape is accepted; any other extra or missing seal fails closed. The writable construction descriptor never enters the guest table.

Fixed snapshots get a newly reopened `O_RDONLY` or `O_PATH` description. Readable fdinfo snapshots get a newly reopened `O_RDONLY` description, preserve the modeled seq-file pwrite ordering, and keep their existing per-description lifecycle state. A non-`O_NOFOLLOW` allocation now explicitly removes stale virtual nofollow metadata for a reused descriptor number.

## Linux semantics and v9 corrections

The open path validates complete non-`O_PATH` `__O_TMPFILE` forms before pathname access and caches whether the host follows the pre-6.4 `LegacyLookup` or current `EarlyEinval` create-plus-directory policy.

V9 corrects and tests the review findings against pinned Linux v6.3 source:

- Existing `/proc/mounts` with `O_CREAT|O_DIRECTORY|O_NOFOLLOW` returns `ENOTDIR` under `LegacyLookup`, or `EEXIST` with `O_EXCL`; ordinary `/proc/mounts|O_NOFOLLOW` remains `ELOOP`.
- A missing, closed, or malformed nonempty fdinfo final component remains `ENOENT` for every `O_EXCL`/`O_NOFOLLOW` combination. Procfs supplies that terminal error before namei's create fallback.
- Exact `/proc/.../fdinfo/` with legacy `O_CREAT|O_DIRECTORY` returns `EISDIR`, including with `O_EXCL` or `O_NOFOLLOW`, after confirming the selected task is live.
- Live readable fdinfo pwrite is checked against native Linux: nonnegative offsets return `ESPIPE` before count or pointer validation; negative offsets return `EINVAL`; invalid descriptors retain the corresponding `EBADF`/`EINVAL` ordering.
- Native DAC-sensitive `EACCES`/`EPERM` comparisons are run only under a non-root supervisor. Guest immutability assertions remain unconditional, and the required-KVM guest proves its modeled uid/euid are zero before requiring `fchmod(...)=EPERM`.

## Validation

Every Cargo command used `CARGO_INCREMENTAL=0` and a 180-second outer timeout. Durations are whole-command wall times including compilation and linking.

- `cargo fmt --all -- --check`: passed in 2.143 s
- focused synthetic-proc library tests: 33 passed, 0 failed in 23.205 s
- exact seal, fdinfo-carrier/pwrite, lifecycle-generation, and stale-nofollow controls: each passed
- required real-KVM static-ELF test with `REVERIE_REQUIRE_KVM=1`: 1 passed, 0 failed in 31.112 s; KVM absence cannot skip
- full `reverie-kvm` library: 815 passed, 0 failed in 13.905 s
- strict all-target Clippy: passed in 9.842 s
- expanded native matrices: passed in 0.232 s, including the seven-case fdinfo pwrite oracle
- four v9 causal source mutations each failed the intended literal assertion and restored all source and patch hashes
- `git diff --check`: passed

The first full-library attempt had one unrelated concurrent pipe-test failure: `positioned_vectored_io_handles_pipes_partial_writes_and_sigpipe` observed a four-byte write instead of `EPIPE`. Its exact rerun passed, an immediate full retry passed 815/815, and the final post-mutation full suite passed 815/815. No source change was made for that flake.

The full base-to-v9 binary patch SHA-256 is `ca2a95b12e6cdadfb3ac983c67aa21dd6fee6e9b7ac66271a7e7228732e5eb1e`. Validation ran on x86-64 Linux 7.1.3 as supervisor EUID 212630.

## Explicit residuals and evidence limits

- No Linux 5.8–6.3 runtime is available locally. Legacy behavior is supported by pinned upstream source, https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58, and injected exact tests, not claimed as locally executed.
- `/proc/self/loginuid`, deterministic random devices, and synthetic sysfs CPU-frequency files remain on the older generic virtual-file carrier. A representative control samples `/proc/self/loginuid`, `/dev/urandom`, and `/sys/devices/system/cpu/cpufreq/boost`, showing those three carriers are unsealed and can still be reopened through guest proc-fd as writable; this patch does not claim untested path coverage or fix the carrier class.
- Linux permits `O_PATH`, including `O_PATH|O_NOFOLLOW`, on live fdinfo entries. Reverie still returns `ENOSYS`; fdinfo `O_PATH` lifecycle/link/status parity is out of scope. Bare fdinfo `O_CREAT`, `O_CREAT|O_EXCL`, `O_TRUNC`, and `O_DIRECT` ordering also remains a disclosed pre-existing mismatch.
- Exact `/proc/self/fdinfo/` ordinary-open/directory/TMPFILE behavior remains the pre-existing `ENOENT` residual; only its legacy create-directory ordering changes here.
- Fdinfo statx still reports the pre-existing truncated basic-stat mask, omitting the timestamp bits. The new test makes that mismatch explicit rather than treating it as parity.
- Direct `/proc/mounts` and proc-fd `O_PATH|O_NOFOLLOW` identity, SCM_RIGHTS identity, noncanonical aliases, mmap type/`MAP_SHARED_VALIDATE`, later `mprotect(PROT_WRITE)` provenance, and credential-exact root mutation parity remain outside this patch.
- A future kernel-added implicit memfd seal other than `F_SEAL_EXEC` causes a deliberate `EOPNOTSUPP` fail-closed result.

## Review history

The original head received a Codex approval at https://github.com/rrnewton/reverie/pull/609#issuecomment-5757098630 and a Claude-family refusal at https://github.com/rrnewton/reverie/pull/609#issuecomment-5757264646. The Linux-source correction to that refusal is recorded at https://github.com/rrnewton/reverie/pull/609#issuecomment-5757393444. This v9 head supersedes both exact-head verdicts and requires fresh Codex- and Claude-family adversarial approvals.

## Human review required

Trigger 2: this changes Reverie's KVM syscall-interception behavior and guest-visible file-description semantics.

Trigger 3: sealed synthetic proc carriers are a determinization mechanism for guest-visible kernel metadata. Independent exact-head Codex- and Claude-family adversarial approvals are required before landing.
