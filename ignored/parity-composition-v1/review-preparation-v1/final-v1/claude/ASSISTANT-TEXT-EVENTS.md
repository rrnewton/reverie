Event 37, block 0, UUID eee4051e-6cfc-4c0c-b170-d11204d8b348

I'll start with the mandatory index.

Event 450, block 0, UUID 30302203-961b-478b-92c3-15cce7d0d237

I have completed the mandatory reading. Report follows.

---

# Independent adversarial review — Reverie CPUID + capture-identity composition

**Range** `6e3915b70a71657a08f0028b3f70d6074116206a..3d4a401ed8959befad0b2f10db59093725f27432`, head tree `73024912837cbbeaa70b6d2173f5b596eebedcb3`
**Base** is the landed timestamp head: `SETUP.json` records `base: 68569ed7fd5c3667456d1d3e0d74ba66264242f4`, `base_tree: 8e9a1d6b22b421992508e26855a568314a12bddf` — byte-identical to the tree I approved in my whole-PR verdict for `https://github.com/rrnewton/reverie/pull/586`. That prior approval is **not** carried into this verdict.

## 0. What I actually read, and in what order

I read `final-v1/claude/READ-INDEX.txt` first, then every bounded mandatory chunk. **I did not follow the listed order exactly**, and I will not claim I did:

- Items 1–14 in listed order (source scope, source identity, my own grounding authentication, my own prior report, the 22-chunk full composition diff, the six new files, `executor.rs` context, `lib.rs`).
- **Deviation 1:** after item 14 I jumped to items **26–28** (the unchanged Detcore consumer: `detcore/src/lib.rs`, `syscalls/files.rs`, stat determinization call sites) to settle the capture-identity determinism question before reading the rest of the backend.
- Then items 15–21 (runtime, terminal admission tests, `vm.rs`, guest, subscription), then back to **22–25**, then 29–45 (the harness callers), then 46–55 (the committed-target binding, transport probe, raw outputs, readback, final results), then item **37** (`observer/observer.py`, 6 chunks) last, and finally item **50** (`complete-terminal-admission-tests`, 2 chunks).
- I additionally read `final-v1/common/RAW-RESULT-READBACK.json` as a follow-up to item 47.

**Coverage is complete**: every chunk listed in the index has been read in full. Total 27 documents plus the harness set; `SOURCE-AUTHENTICATION.json` records 2,621 files + 7 symlinks + 3 gitlinks = 2,631 entries, which reconciles with `QUALIFICATION.md`'s "2,630 tracked entries and one additional ignored `Cargo.lock`" — the apparent one-entry gap is tracked-vs-ignored, not a discrepancy.

I used only `Read` and `Glob`. No build, no guest, no network, no write, no delegation. I did not open another review lane's report or any mutable qualification output.

---

## 1. FINDINGS

### Blocking defects: **none.**

Six non-blocking findings, in descending severity.

---

### C1 — MEDIUM (scope boundary, not a defect in this layer)
**A bare `reverie-kvm` embedder's `fstat(1).st_dev/st_ino` for the capture sink becomes host-assigned where it was previously a constant.**

`executor.rs::synthetic_captured_output_stat` previously filled `st_dev`/`st_ino` from an internal synthetic device plus a monotone counter. It now takes a `CaptureObjectIdentity` sourced from a live pipefs inode:

```rust
fn synthetic_captured_output_stat(identity: CaptureObjectIdentity) -> libc::stat {
    stat.st_dev = identity.device;
    stat.st_ino = identity.inode;
    stat.st_mode = libc::S_IFIFO | 0o600;
```

Two host-assigned values now reach the guest that did not before. That is a real change and it must be named rather than waved through.

**Why it does not block.** `reverie-kvm` has never been the layer that virtualizes object identity. `guest_object_stat` falls through to a real `fstat(host_fd)` for every ordinary object, and the retained control `anonymous_proc_fd_links_match_direct_owned_object_stat` asserts that a guest's own `pipe2()` reports the **native** dev/ino. The capture sink was the single object in this layer carrying a fabricated identity; the change removes an inconsistency rather than introducing nondeterminism into a previously determinized surface. Everything this layer *does* determinize for the sink is unchanged: `sanitize_stat_timestamps`, `DETERMINISTIC_METADATA_SECONDS`, `st_uid`/`st_gid` = 0, `st_size` = 0, `st_blocks` = 0, `st_nlink` = 1, `st_blksize` = PAGE_SIZE.

The determinism boundary for the product is Detcore. `handle_statx` (`detcore/src/syscalls/files.rs:2688-2708`) injects the real call and routes the result through `determinize_stat`; `DetStat` carries `dev` and `inode` as explicit fields. That path is unchanged by this composition.

**Evidence limit attached to C1:** I did **not** read `determinize_stat`'s body — it is not in the bound set. I can state that the determinization path exists and carries both fields; I cannot assert from source that it rewrites `inode` for a FIFO. That residual belongs to the separate Hermit-consumer evaluation, which the instruction correctly told me to keep distinct, and I am keeping it distinct. **Real host inode/device identity is not by itself deterministic virtualization** — agreed, and this finding is exactly that point recorded rather than assumed away.

---

### C2 — LOW (stale name)
`captured_output_fstat_is_synthetic_and_stable` now asserts a *live* identity. The name says "synthetic". No behavioural consequence; a rename is free. Flagged because a misleading test name is how a future reader mistakes the new external oracle for the old internal one.

---

### C3 — LOW (cost, not correctness)
**With both CPUID and RDTSC interception armed, every RDTSC `#GP` pays two extra `KVM_GET_MSRS` before falling through.**

`vm.rs::tool_instruction_exception` tries CPUID first, and `cpuid_instruction_exception` calls `self.cpuid_interception.verify(&self.vcpu)?` after the `exception.vector != 13` filter but *before* the instruction decode. An RDTSC-heavy guest with CPUID also subscribed therefore performs one extra two-MSR vCPU round-trip per timestamp trap. Mode 3 of the dispatch fixture exercises the combination and passes, so this is a cost, not a defect.

I am explicitly **not** recommending the obvious reordering (decode first, verify only on a successful CPUID decode). That would weaken the current fail-closed property that MSR drift is detected on *every* `#GP`, which is worth more than the round-trip.

---

### C4 — LOW (carried forward, not introduced here)
`qualification-v2/phase.py::inspect` still writes `compiler-diagnostics.json` *after* the `build-finished` `require`. A compile that never emits `build-finished` produces no diagnostics file — the same ordering I diagnosed at V6. Not introduced by this diff, and the composition's compile succeeded, so it did not bite.

---

### C5 — LOW (carried forward, now duplicated)
My V7 findings V1/V2/V4 are unchanged in this head. Note that the CPUID gate copies the timestamp gate's stack-segment acceptance verbatim — `USER_DATA_SELECTOR & !3` — so the inherited looseness now exists in **two** places. It remains harmless because CS is compared strictly against `USER_CODE_SELECTOR`, but the "normalise these together" note now covers three sites rather than two.

---

### C6 — INFORMATIONAL
`Tool::handle_cpuid_event`'s default implementation is `Ok(raw_cpuid::cpuid!(eax, ecx))` — the **host** CPUID. That file is unchanged, but this composition makes the default reachable on this backend for the first time. A Tool that subscribes `cpuid()` without overriding the handler will now receive host CPUID values under KVM where it previously received the installed deterministic table. Detcore overrides it. Worth naming because the reachability is new even though the code is not.

---

## 2. Goalpost-moving audit — all four targets assessed explicitly

### Target: weakening an assertion so a test passes — **NOT FOUND**

I examined every changed assertion against the base. Exactly one assertion's *expected value* changed: the capture identity pair. The instruction asked me to decide whether the replacement satisfies the requirement or merely weakens it. **It satisfies, and it is strictly stronger**, on six independent grounds:

1. **The oracle moved from internal to external.** The old assertion compared the reported inode against a value the executor itself computed from its own counter — a self-consistency check that could not fail for any reason other than a counter bug. The new one compares against `fstat` of a real kernel object the test opens independently (`capture_native_stat(keeper)`).
2. **Three inequalities were added that the old form could not express:** `assert_ne!(captured.st_ino, native_pipe.st_ino)` (distinct from a guest's own pipe), `assert_ne!(captured_inodes[0], captured_inodes[1])` (the two streams are distinct objects), and `assert_ne!(stats[0].st_ino, stats[1].st_ino)`.
3. **A positive cross-object control was added:** `assert_eq!(captured.st_dev, native_pipe.st_dev)` — the sink and a guest `pipe2()` share pipefs, which is what Linux does and which the old synthetic device could never have satisfied.
4. **Every deterministic field the old assertion checked is still checked**: `S_IFIFO | 0o600`, `st_size == 0`, `st_blocks == 0`, `st_mtime == DETERMINISTIC_METADATA_SECONDS`.
5. **Route agreement widened, not narrowed**: one helper now requires `fstat`, `newfstatat` across four path spellings plus `AT_EMPTY_PATH`, `statx`, and `stat` to agree.
6. **The old model was removed, not left as a fallback.** `synthetic_guest_fd_object_inode` is now `#[cfg(test)]`, which is a *compile-time proof* that no production caller remains. Had any production path still used it, the tree would not build.

Elsewhere: `terminal_runtime_tests.rs` changed by exactly **2 bytes** (6,658 → 6,660), which is precisely `Timestamp` → `Instruction` (9 → 11 chars). I read the complete 189-line file: the five-row table `(exit, T, T) (exit_group, T, T) (write, F, T) (execve, F, F) (fork, F, F)` and all ten assertions are identical, and `terminal_cancellation_and_into_guest_forwarding_do_not_inject_or_start_children` and `terminal_exit_retires_only_current_identity_and_preserves_existing_status` are untouched. `tests/support/timestamp_terminal.rs` is byte-identical to the landed V7 file (sha `447bf99d…`), mode 10 and `events.len() == 7` intact. The CPUID terminal fixture is a **new parallel file**, not an edit of the timestamp one.

### Target: widening a tolerance / adding an exemption / skipping a case / relaxing a comparator — **NOT FOUND**

There is no tolerance to widen: every new oracle is exact equality — full 64-bit register comparisons including `r8`, `rsp` and RFLAGS; exact call vectors such as `[(1,7,2),(2,7,2),(2,7,2)]`; exact `(dev, ino)` pairs; exact `(vector, RIP)` pairs.

No exemption: `kvm_available` is unchanged, and `REVERIE_REQUIRE_KVM=1` is both planted in `prepare.py`'s plan environment **and** re-verified by `phase.py::payload`, which requires every declared env key to match at payload time and rejects undeclared `REVERIE_*` overrides.

No case skipped. I did not take the finalizer's word for this — I read all 83 raw stdout/stderr streams myself and found no skip message. Independently, `finalize_results_v2.py` regex-scans every stream for `\bskip(?:ped|ping)?\b|\bunexecuted\b|KVM unavailable|cannot open /dev/kvm` and gates `all_qualified` on the result. Every one of the 83 shows `passed: 1, failed: 0, ignored: 0, measured: 0`.

No comparator relaxed anywhere in the diff.

### Target: renaming or relabelling so a failure reads as a pass — **NOT FOUND**

`ProcessExecutionContext::Timestamp` → `Instruction` is a **private enum variant** with no bearing on any test name or verdict. Tellingly, the test function keeps its original name (`timestamp_exit_admission_preserves_returning_ordinary_injections`), so the rename cannot disguise a scope change — control-82 is verifiably the same named test as before, and it ran `ok` at `filtered_out: 522`.

The first metadata attempt is **preserved as unqualified**, exactly as required: `accepted: false`, `raw_status: 137`, `terminal_authenticated: false`, `accounting_complete: false`, `old_receipt_repaired: false`, `total_including_first_refusal_cpu_unknown: true`. Its CPU is **not** folded into the 437.665 s aggregate. Nothing was relabelled.

C2 (`…_is_synthetic_and_stable`) is a stale name whose assertions became *stronger* — the opposite direction from this target.

### Target: deleting a check rather than satisfying it — **NOT FOUND**

This is the strongest quantitative evidence, and I derived it independently rather than reading it off a summary. Using `filtered_out + 1` from the raw libtest suite lines:

| population | V7 baseline | this head | delta |
|---|---|---|---|
| lib declarations | 512 | **523** | +11 |
| static-ELF declarations | 321 | **331** | +10 |
| vmcall | 6 | 6 | 0 |
| read-clock | 8 | 8 | 0 |

The +11 and +10 match **exactly** the new declarations I counted in source: lib = `cpuid_instruction.rs` 4 + `cpuid_runtime_tests.rs` 2 + `capture_identity_tests.rs` 5 = 11; static = `tests/support/capture_identity.rs` 1 + `cpuid_dispatch.rs` 6 + `cpuid_terminal.rs` 3 = 10. **No declaration was removed, renamed away, or quietly dropped.**

Supporting counters, all in the tightening direction: the rustfmt path list grew 13 → 17; the selection grew 55 → 83; `admit_target.py` gained a stricter private-lease precondition; `synthetic_guest_fd_object_inode` was demoted to `#[cfg(test)]` rather than deleted.

On the instruction's specific concern: **the EMFILE, closed-standard-fd and error-cleanup controls are real, not mocked.** They use real `setrlimit(RLIMIT_NOFILE)`, real descriptor exhaustion, real `close(0/1/2)`, real `pipe2`/`F_DUPFD_CLOEXEC` failures counted separately, and real `catch_unwind`. The `CaptureSetupMustNotInitialize` Tool *panics* if `init_global_state` is ever reached, which is a genuine failure mode, not a stubbed refusal.

---

## 3. Review area 1 — CPUID (assessed against each sub-target)

**Capability detection.** `configure` requires `PLATFORM_INFO` (0xce) in `get_msr_feature_index_list()`, then a system-level `get_msrs` with `exact_count("feature read", 1, …)`, then `data & (1 << 31)`. This matches the kernel's `kvm_get_feature_msr` path and `MSR_PLATFORM_INFO_CPUID_FAULT`. Measured on the vendor kernel: `host feature: count=1, platform_info=0x80000000`.

**Exact get/set counts.** Every transfer is wrapped in `exact_count`, and writes are **one MSR per call**. This is not stylistic — `KVM_SET_MSRS` is documented to stop at the first failing MSR and return a partial count, so a batched write would silently apply a prefix. The probe measures `read 1/2; write 0/1` distinctly.

**Arming order is mandatory, not cosmetic.** Upstream `kvm_x86.c` refuses `MSR_MISC_FEATURES_ENABLES` CPUID_FAULT unless `supports_cpuid_fault(vcpu)`, so `PLATFORM_INFO | SUPPORT` must land first. `restore` reverses the order (MISC then PLATFORM), which is the correct inverse. Measured transitions `[0, 1, 0]`.

**Per-real-vCPU ownership.** `Interception` is a field of `KvmBackend`; `from_thread_state` calls `new_with_memory_and_cpuid_policy`, creating a fresh VM + vCPU per CLONE_THREAD worker. Per-vCPU and per-VM therefore coincide in this backend, which is why the probe's three-separate-backend measurement is the applicable model and `same_vm_multivcpu_test: false` is not the gap it first appears to be. The probe shows an independent backend reading ENABLE=0 (`index 320, data 0`) while the first stays armed (`data 1`). A pre-armed vCPU with no owning consumer is refused: `"CPUID interception was armed without an owning Tool consumer"`.

**Stale / readback / partial failure.** `verify()` re-reads both MSRs on every fault and errors on drift. `configure(true)` on an already-enabled owner routes to `verify`. Arming does a full readback compare and, on mismatch, restores and returns the original error with cleanup errors attached via `with_cleanup`. A failed `restore` **retains** `original`, so a later arm is refused with `"unresolved MSR restoration"` rather than double-arming.

**Restoration and opt-out.** `configure(false)` → `restore`, idempotent when `original` is `None`. Every non-consumer loop calls `set_cpuid_interception(false)`, verified by real MSR readback in `assert_disarmed`.

**Decoding / prefix / LOCK / length / fetch-fault priority.** The decoder accepts 0x66/0x67/0xF2/0xF3, all four segment overrides, FS/GS, and REX 0x40–0x4F, caps at 15 bytes, and requires exactly `0F A2`. It explicitly rejects `0F 31` (RDTSC), `0F 01 F9` (RDTSCP), a bare `0F`, and `ED`. Real controls: LOCK-prefixed CPUID → #UD with 0 callbacks; 16-byte overlong → #GP with 0 callbacks; a 2-byte instruction at BOUNDARY−2 with an NX next page dispatches with length 2; the 3-byte case raises #PF with `cr2 == BOUNDARY` and no dispatch.

**This decoder is the only discriminator, and that matters.** The probe measured `unrelated CLI still faults: vector=13, error=0` — a plain `0xFA` produces an identical vector and error code. Vector plus error code alone cannot identify a CPUID fault; the instruction bytes can. The design is correct for that reason, not incidentally.

**CPL / long-mode envelope.** CR0.PG, EFER.LMA|LME, `!CR4.LA57`, error code 0, `CS == USER_CODE_SELECTOR`. Matches `kvm_emulate_cpuid` → `kvm_require_cpl(vcpu, 0)` → `kvm_queue_exception_e(vcpu, GP_VECTOR, 0)`.

**Input truncation and result semantics.** `rax as u32` / `rcx as u32` with nonzero upper halves exercised in the guest fixture and asserted Tool-side. `result_registers` zero-extends all four outputs, advances RIP by the decoded length via `checked_add`, and clears RFLAGS.RF (bit 16) — correct for retirement of a faulted instruction — preserving everything else. The guest fixture compares all four registers plus `r8`, `rsp` and flags at 64-bit width.

**TF.** Real control: `#DB` (vector 1) at `LOAD_ADDRESS+12` with exactly 1 callback.

**Coexistence with RDTSC/RDTSCP.** The two decoders are mutually exclusive *by construction* — each returns `None` on the other's opcode, both directions explicitly tested — so dispatch order is immaterial to correctness (it costs C3). One mixed control asserts the exact vector `[(1,7,2),(1,MAX,0),(1,MAX,1)]` with strictly increasing clock samples.

**Fresh child vs exec / reused vCPU.** A new backend starts unarmed and re-arms from subscriptions; exec reuses the same backend and stays armed. The control asserts exactly `[(1,7,2),(2,7,2),(2,7,2)]`.

**Default / Host / Tool loops and `has_cpuid_interception`.** Disarmed in `run_with_tool`, `run_static_elf_process` and `run`. The non-ELF Tool asserts `!has_cpuid_interception()` and panics if the callback ever fires. Host-owned worker: `[1, 1]`.

**Native masking is not equated with callback support.** Unsubscribed CPUID returns the installed deterministic table with **0 callbacks** — measured on hardware as `rdx=0x20100800` (leaf `0x8000_0001` EDX bit 27 clear) reaching HLT with no dispatch. The distinction is maintained throughout.

---

## 4. Review area 2 — shared Instruction callback state

The rename is a pure private refactor: all four predicates (`tail_injection_allowed`, `ordinary_injection_allowed`, `injected_signal_allowed`, `has_resumable_signal_boundary`), the preflight extension, the process-action error arm and the `Hlt` construction use the new variant with identical logic.

**The landed timestamp behavior is preserved, verified three ways:** the admission table file differs by 2 bytes; `timestamp_terminal.rs` is byte-identical; and controls 47–49, 82 and 83 all re-executed `ok` against this head, including mode 10 (`worker_timestamp_exit_group_terminates_leader_and_runs_hooks_once`, `filtered_out: 330`).

**Ordinary returning `write` remains allowed while tail `write` is refused** — confirmed in the complete 189-line file at rows `(libc::SYS_write, false, true)`. This is also where I restate my own Turn-H correction: my V6 N2 literal ask for `ordinary(write) == false` was **wrong**, and it was correctly not fulfilled.

**A pure MSR/decoder control is not an end-to-end Tool control** — agreed, and the packet keeps the populations apart, which I verified rather than assumed: controls 01–02 are pure decoder, 03–04 are real-MSR but not Tool, 05–06 are real KVM runtime, 07–15 are real Tool dispatch, and the terminal fixture is the end-to-end lifecycle. I assessed the real fixture evidence separately from the unit evidence throughout.

The dispatch now routes a tagged `ToolInstructionBoundary` to a matching `ToolInstructionResult`, with a typed error if the two kinds disagree. Terminal arm, hidden-scratch accounting on all seven paths, and the `process_completed` check are unchanged.

---

## 5. Review area 3 — captured output identity

**Setup precedes image and Tool consumption.** `prepare_captured_output` runs before `static_elf.take()` and before `init_global_state`. The EMFILE control proves the image pid survives, `tool_failure` is none, and a Tool whose `init_global_state` panics is never reached.

**Real failures, separately counted.** First `pipe2` failure, second `pipe2` failure, and `F_DUPFD_CLOEXEC` relocation failure each have their own counter and their own control.

**Private CLOEXEC endpoint ownership.** `pipe2(O_CLOEXEC)`; the write end is dropped immediately; the keeper is relocated above fd 2 if needed; `F_GETFD == FD_CLOEXEC` is asserted; the keeper is never installed in any guest file table. `fstat` requires `S_IFMT == S_IFIFO`.

**Root lifetime / drop / unwind.** The owner is declared before the executor, so drop order outlives it. `Arc::downgrade` plus a drop probe confirm liveness under both the normal and the `catch_unwind` path; the fds report `EBADF` only after the last `Arc` is gone.

**Stream identity vs guest fd alias and OFD identity.** `dup`, `dup2`, `dup3`, `fcntl(F_DUPFD*)` and `open("/proc/self/fd/N")` all propagate `OutputAlias`; `dup2` of an ordinary file over an alias retires it; a private fork keeps its alias; draining output does not retire identity. Shared threads and private forks share the `Arc`; `replace_after_exec` deliberately does not touch `output`.

**Authoritative table synchronization.** `FileTableState::install` / `update_from_elf` bracket every syscall, covering `fd_object_inodes`, `cloexec_fds`, `closed_standard_fds`, `proc_files` and `fdinfo_files`.

**stat / statx / readlink / proc symlink routes.** One helper requires agreement across `fstat`, `newfstatat` (four spellings + `AT_EMPTY_PATH`), `statx` and `stat`. The `/proc/self/fd/N` symlink *object* remains synthetic `S_IFLNK` on the guest-fd device, while `readlink` names the real pipe inode — the correct split.

**I am not generalizing this to pipe readiness or physical I/O.** The write end is closed at creation; nothing is ever written to or read from the pipe; the capture remains in memory. This is an identity reservation, full stop. The unchanged byte-capture, vectored-IO, `lseek`, write-site and statx-mask controls were all retained and re-executed.

---

## 6. Review area 5 — qualification identity and limits

**83 unique declarations** = 43 lib + 34 static + 2 vmcall + 4 read-clock, across **92 planned phases** = 83 + metadata + compile + format + 4 list phases + core-check + clippy. Every phase records `raw: 0`, `accepted: true`, `event: "ok"`; `records_authenticated: 5651`; CPU 437.665 s reconciles with the per-phase sums.

**No pending result is treated as a pass.** `finalize_results_v2.py` asserts `not refused` before each phase, requires `len(phases) == len(order)`, requires every phase accepted, requires zero skips, and requires `scm['status'] == ''`. `phase.py` requires the started set, the ended set, the ended *order* and the selected list to match exactly, and requires exactly one suite with `passed == len(selected)` and `failed == ignored == measured == 0`.

**Four fresh retained harnesses**, distinct inodes, mode 0555, `fresh=false`, advisory `flock` lane lease. `execute_continuation.py` additionally requires `sys.flags.optimize == 0` and `PYTHONOPTIMIZE == '0'`, so asserts cannot be stripped. The observer (`observer.py`, read in full) authenticates the safehermit unit identity, ExecStart argv, cgroup membership and final accounting, and gates `comparison_eligible` on exit 0 + no stop reason + untruncated + `unit_result == success`.

**The initial raw-137 RefUnit accounting refusal is preserved as unqualified** and is not counted, not repaired, and not merged into the CPU total.

**The five transport-probe phases are prerequisites, not composed declarations** — they are not among the 83, and I did not add them. **I am not adding overlapping predecessor totals**, and I am not inferring full backend parity, a Hermit timer fix, clock-origin repair, or new cancellation intervals from any of this. Inventories (868 declared), declarations (83 selected), and physical fixture modes are three different populations and I have kept them apart.

---

## 7. Conclusions

### 7.1 Deterministic behavior

`reverie-kvm` adds no clock effect for CPUID — `cpuid_instruction.rs` contains no time source, and `read_clock` is asserted unchanged across the CPUID callback (RPC, returning injection, and refused injection), while increasing strictly across guest comparison branches. Detcore's `handle_cpuid_event` performs `time.add_cpuid()` and returns the `InterceptedCpuid` table. So a Hermit guest's CPUID becomes table-determined and time-charged where, under KVM before this change, it was served by the installed table with no Tool dispatch and no charge.

That is the mechanism behind the divergence I decomposed at V6 (2,255 ns = 223 × 10 + 1 × 25, the extra instruction being the CPUID KVM never dispatched). **I am not claiming the divergence is resolved** — that needs a new cross-backend measurement which this packet does not contain and which I am instructed not to infer.

No coarsening, freezing, rounding or resetting of virtual time appears anywhere in the diff. The one determinism-relevant change is C1, scoped above.

### 7.2 Linux / x86 / descriptor semantics

Sound. The CPL3 CPUID fault matches `kvm_emulate_cpuid` + `kvm_require_cpl` including the zero error code, confirmed against upstream source *and* measured on the vendor kernel. The MSR arming order is forced by the kernel's own guard and the restore order is its correct inverse. One-MSR-per-write is forced by documented `KVM_SET_MSRS` partial-transfer semantics. Prefix, LOCK, length, fetch-fault priority and TF all match the architecture and are pinned by hardware controls. **No disputed prefix/TF/RF native semantics is inferred from the MSR transport probe** — those come from the instruction-level controls, and I kept the sources separate.

Descriptor semantics: the sink now shares pipefs with a guest's own anonymous pipe, presents one live distinct inode per stream, keeps `/proc/self/fd/N` as a synthetic symlink whose target names the pipe, and agrees across four syscalls and six path spellings.

Two pre-existing divergences persist and are now applied identically to the CPUID path: single-step surfaces as a typed `GuestException{vector:1}` rather than a guest SIGTRAP, and 1 GiB code pages plus the upper canonical half are refused.

### 7.3 Evidence limits

1. `determinize_stat`'s body is unread (C1's residual).
2. `lifecycle_signal_mask_preflight`'s body is unread; this composition only renames its existing arm.
3. The tool-process async epilogue is unread; the terminal arm's early return is verified only through observable hook effects.
4. **83 of 868 declarations were selected (~9.6%)** — 523 lib + 331 static + 6 vmcall + 8 read-clock. This is a targeted selection, not a full suite.
5. The transport probe is a transport/ownership measurement on kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, explicitly `cpuid_tool_callback_or_accounting_test: false` and `same_vm_multivcpu_test: false`.
6. The first metadata attempt's CPU is unknown and its raw-137 refusal stands unrepaired.
7. No Hermit integration, cross-backend parity, clock-origin repair or cancellation-interval claim follows from this evidence.
8. No PR body is bound, so PR-body obligations are not checkable here.

### 7.4 Review-protocol triggers, derived from this diff

Prior classifications are not binding; these are my own for this source.

- **New syscall support — NOT triggered.** No syscall was added. CPUID and RDTSC are instructions, and `handle_cpuid_event` is a pre-existing `Tool` hook that gains backend support. *No syscall was newly added merely because an existing callback gained backend support.*
- **Core API / abstraction change — TRIGGERED.** The changed-path set is entirely `reverie-kvm/**`; `reverie/src/{tool,guest,subscription}.rs` are unchanged, so no *public* API changed. But this extends the backend's instruction-interception model to a second CPL3 fault class with a new MSR transport, generalizes `ProcessExecutionContext::Timestamp` into `Instruction` with a tagged boundary/result pair, and gives `Guest::has_cpuid_interception` its first non-default implementation.
- **New determinization strategy — NOT triggered.** Table-driven deterministic CPUID is an established Detcore strategy; this gives it backend reach.
- **Core DetCore scheduling change — NOT triggered.** No `detcore/**` file is in the diff; the Detcore sources in the index are unchanged consumer context.
- A KVM change also requires the PR's *Relationship to gVisor* section, which is not checkable from this packet.

---

## 8. Verdict

# APPROVE

for the **whole** base-to-head CPUID + capture composition `6e3915b7..3d4a401e`, tree `73024912837cbbeaa70b6d2173f5b596eebedcb3`.

No blocking defect. C1 is a genuine scope item that must be carried into the separate Hermit-consumer evaluation rather than closed here; C2–C6 are fix-forward.

The four goalpost-moving targets were each searched for explicitly against the base and **none was found**. The single changed expected value — capture identity — moves the oracle from an internal self-consistency check to an external kernel object and adds three inequalities and a cross-object equality; the declaration count rose by exactly the 21 tests I counted in source with nothing removed; and the old synthetic helper's demotion to `#[cfg(test)]` is a compile-time proof that the replaced model has no production caller left.

Reading completion is not approval, and my approval of the predecessor PR is not approval of this source. This verdict rests on the source I read in this range.

