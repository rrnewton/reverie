[hermit2, claude-cml-w-liteinst, opus, devbig014, role=impl]

# LiteInst backend-parity-c cells red on main: Phase 1 report, stopped (the fix belongs in reverie)

- **Task:** `liteinst_logs_its_own` (TaskGraph hermit2). The coordinator claude-coord-mega-lander holds the claim; I did not claim, close or re-tag it.
- **Slot:** /home/newton/work/dev-hermit/worktrees/slots/cml-liteinst
  - branch `claude/liteinst-runtime-self-load-20260927`
  - HEAD 19553a64308ca123bbde7cc3d7720e0aa72e795a
- **Date:** 2026-09-27
- **Evidence directory:** /var/tmp/cml/liteinst-evidence/

## 0. Bottom line

1. **The mechanism is confirmed, and it is wider than the task title.**
   - reverie's host-hybrid launch loads the LiteInst runtime *through the guest's own environment*, by setting `LD_PRELOAD=<runtime>` and `REVERIE_LITEINST_HOST_RUNTIME=1`.
   - Inside the guest process, glibc's dynamic loader (ld.so) then:
     - opens and maps the runtime, a 68.6 MB address span;
     - loads the runtime's dependency `libgcc_s.so.1`;
     - runs the runtime's constructor.
   - Each of those syscalls, and the loader's timing `rdtsc` reads, is a real event of the guest process. reverie-ptrace passes every one of them to Hermit's Detcore tool, which logs it and advances Hermit's logical clock.
   - None of this happens under the ptrace backend, because nothing is preloaded there. The ptrace backend is not "missing" events.
   - Host example, aio-refusal: 41 guest syscalls on ptrace against 374 on LiteInst, for a program that prints 9 bytes.
2. **This is not a regression. These cells have never passed a comparison against ptrace.**
   - The comparison was switched on by https://github.com/rrnewton/hermit/commit/e385d5f72a9b336bb121329790c6e9e3b61bdd34 (2026-09-16 19:41 -0700).
   - Before it, 21 of 21 LiteInst backend-parity-c cells passed a same-backend repeat check, and no ptrace comparison existed.
   - After it, 6010 of 6010 retained LiteInst parity logdiffs diverged, all at record 16 / syscall 3. They come from 31 validation runs, 2026-09-17T00:23 to 2026-09-27T00:37.
3. **Removing the runtime's records would not make any cell pass.** The differences that remain are ones the guest program itself can observe:
   - The injected environment strings move the initial stack. A ptrace run with two inert variables of the same size reproduces LiteInst's stack pointers exactly (control c1, section 4).
   - The runtime mapping moves libc 0x4200000 bytes (66 MiB) lower.
   - The runtime's first `malloc` initialises the *program's* glibc heap. The program's own `getrandom`/`brk`/`brk` calls therefore disappear, and its heap buffer moves.
   - On the KVM lane, the program's own records differ even with the load window ignored:
     - the `fstat(1)` stack pointer differs in 97 of 97 cells;
     - the `write(1)` buffer differs in 96 of 97 cells.
4. **Why I stopped: there is no principled Hermit-side fix.**
   - Detcore cannot see which LiteInst phase a thread is in; reverie exposes no such state.
   - The loader's work for the runtime is interleaved with the program's own library loading, and `libgcc_s` can also be the program's own dependency.
   - On this host (glibc 2.34), the first divergence comes *before* the runtime is loaded.
   - A Hermit-side filter would leave the guest-visible shifts in place, and the task forbids masking them.
   - The change belongs in reverie; section 6 lists it.
5. **No commits.** The slot is clean (`git status` rc=0, 0 lines). There are no fix tests, because there is no fix.

## 1. Reproduction at main 19553a64 (Phase 1, step 1)

### Binaries and runtime

- **Hermit build:** `cargo build --release --locked -p hermit --bin hermit -p hermit-manifest-plan --bin test-harness` with `CARGO_TARGET_DIR=/var/tmp/cml/target-liteinst`. rc=0, 89.6 s. Evidence: build1.log, build1.start and build1.end.
- **Hermit binary:** /var/tmp/cml/target-liteinst/release/hermit
  - sha256 21a55b143f1ac5eddfd10bf100c94c010e22efe31732113d49980d94ab51d244
  - 68,494,064 bytes, mtime 02:34 PDT
- **Runtime staging:** `scripts/stage-liteinst-runtime.sh release /var/tmp/cml/target-liteinst/release/libreverie_liteinst.so ...`. rc=0, 22.9 s. Evidence: stage1.log.
- **Runtime:** /var/tmp/cml/target-liteinst/release/libreverie_liteinst.so
  - sha256 81ce1730fade4f96fa7d3ee0adad7939782ac85c569d03b966fa452bf72a4d40
  - 35,241,088 bytes
  - revision marker `libreverie_liteinst.so.revision` contains `b0ede531e00dd1e068d0e0b1f220edddcf96d4b5`
- **reverie revision:** b0ede531 is the reverie gitlink at 19553a64 (`git ls-tree`) and the revision Cargo built (build1.log: `reverie.git?rev=b0ede531...`).
  - All reverie line numbers below come from the Cargo checkout of that exact revision: /home/newton/.cargo/git/checkouts/reverie-2fc770f7a9c80803/b0ede53, `git rev-parse HEAD` = b0ede531e00dd1e068d0e0b1f220edddcf96d4b5.
- **Harness binary:** `test-harness` needed `-p hermit-manifest-plan --bins` as well. rc=0, 27.7 s. Evidence: build2.log.

### Command

This is the harness's own single-cell path. It is the official runner, so it is not wrapped in safehermit.

```
HERMIT_BIN=/var/tmp/cml/target-liteinst/release/hermit E2E_RESULT_ROOT=/var/tmp/cml/liteinst-evidence/e2e \
E2E_RUN_ID=phase1-aio-host E2E_KEEP_VERIFY_LOGS=1 timeout 900 \
/var/tmp/cml/target-liteinst/release/test-harness run --lane portable --category backend-parity-c \
  --test backend-parity-c/aio-refusal --mode verify --backend liteinst --parity-reference ptrace \
  --results /var/tmp/cml/liteinst-evidence/e2e/phase1-aio-host.results.jsonl \
  --junit /var/tmp/cml/liteinst-evidence/e2e/phase1-aio-host.junit.xml
```

### Result

- **Outcome:** rc=1, wall 1.6 s. 1 of 1 cells FAILED; 2 of 2 attempts failed, taking 731 ms and 669 ms.
- **Failure class:** `product_failure`, "liteinst diverged from ptrace: shared Detcore INFO records".
- **Results metadata:**
  - `hermit_sha` 19553a64308c…, `source_tree_dirty` false, `relaxations` []
  - `binary_sha256` 21a55b14…
  - machine devbig014, kernel 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf
- **Runtime actually loaded:** the verify log shows `openat(... "/var/tmp/cml/target-liteinst/release/libreverie_liteinst.so")`.
- **Repeat checks on each side (e2e/runs/phase1-aio-host/backend-parity-c-aio-refusal-verify-liteinst/backend-parity.json):**
  - LiteInst run against itself: matched bitwise, 1006 = 1006 messages.
  - ptrace reference against itself: matched bitwise, 121 = 121 messages.
  - Standard output: identical on both backends (9 bytes, sha256 926d56f1…).
  - Only the cross-backend comparison diverges: 120 selected records on ptrace against 1005 on LiteInst.

### The first divergence on this host is record 14 / syscall 2, not 16 / 3, and the reason supports the finding

- The divergent record is `arch_prctl(12289 = ARCH_CET_STATUS, <stack buffer>)`.
  - Attempt 1: ptrace 0x7fffffffe9a0, LiteInst 0x7fffffffe930.
  - Attempt 2: ptrace 0x7fffffffe970, LiteInst 0x7fffffffe900.
  - The shift is −0x70 in both attempts.
- The host's glibc 2.34 loader (`ldd --version`) issues this call before it processes `LD_PRELOAD`. The KVM lane's Nix glibc 2.42 does not issue it. The KVM lane therefore first diverges where the runtime load begins; this host first diverges on a stack pointer that the injected environment has already moved (section 4).
- Apart from that, the shape matches the KVM lane:
  - **Before the preload-file check, LiteInst logs:**
    1. rdtsc, rdtsc, brk, arch_prctl, mmap;
    2. rdtsc;
    3. openat, read, fstat, mmap×5 and close of the runtime;
    4. rdtsc;
    5. `access("/etc/ld.so.preload")`.
  - **ptrace logs:** rdtsc, rdtsc, brk, arch_prctl, mmap, then `access`.
- **Plan check:** `test-harness plan --lane portable --category backend-parity-c --backend liteinst --ci-only` gave rc=0 and 97 cells (plan-liteinst.txt, 97 lines).

## 2. Regression or always red (Phase 1, step 2)

This is not a regression. The LiteInst cells have been red on every run since a ptrace comparison was first applied to them.

| Validation run (read-only, under /home/newton/work/dev-hermit/ignored/validate/artifacts/) | Hermit commit | Contains e385? | LiteInst backend-parity-c cells | Compared against ptrace? | Result |
|---|---|---|---|---|---|
| validate-ops-tick-fe4108174265-171e125114b1 | fe41081742652fce09fc22f20469df10fa09ddaa (2026-09-16 16:18 -0700) | no (`merge-base --is-ancestor` rc=1) | 21 | no: none of the 21 cell dirs has a backend-parity-logdiff.json | 21 of 21 `verified=true, bitwise_parity=true`, a repeat check against itself; no attempt-2 dirs |
| validate-ops-tick-158a89f6217b-088d91951315 | 158a89f6217b25db9540237f9c1e256cdbaf785c (2026-09-16 23:24 -0700) | yes (rc=0) | 97 | yes | 194 of 194 logdiffs diverged, all at record 16 / syscall 3 |
| All 31 retained runs that have LiteInst logdiffs (29 runs × 194 files + 2 runs × 192) | 2026-09-17T00:23 → 2026-09-27T00:37 | yes | 96–97 | yes | 6010 of 6010 diverged; first record 16 in 6010, first syscall 3 in 6010 |

- **Introducing change:** https://github.com/rrnewton/hermit/commit/e385d5f72a9b336bb121329790c6e9e3b61bdd34, "Enable ptrace parity in the existing portable selectors", a direct commit with no pull-request link in its body. It turned on the ptrace-reference comparison in the existing portable backend-parity-c selectors. Evidence: e385d5f72a-stat.txt and e385d5f72a-diff.txt.
  - It did not break working behaviour. It started measuring something that had never been true.
- **What the project already said:**
  - The body of https://github.com/rrnewton/hermit/commit/fe41081742652fce09fc22f20469df10fa09ddaa (pull request https://github.com/rrnewton/hermit/pull/2948) says the selections "do not establish in-process execution or cross-backend parity".
  - hermit@19553a64 docs/LITEINST_QUALIFICATION_20260917.md:25-29: "They are same-backend strict repeat comparisons, not in-process LiteInst, ptrace-free execution, or comparisons against a ptrace golden run. The category name `backend-parity-c` is not evidence of cross-backend parity."
  - The same file, lines 46-47: "the stored parity table still has zero measured comparisons".
- **Cell count growth:** the count grew from 21 to 97 through https://github.com/rrnewton/hermit/commit/290280a9e20486be8edafe33b56a0abb41acd103 ("Select 96 measured LiteInst verification cells") and https://github.com/rrnewton/hermit/commit/7ca5b4e82a7a1388306f815066ceb129ee52e444 ("Preserve parity reporting for added LiteInst cells"). Both descend from e385 in main's history (`merge-base --is-ancestor` rc=0). The 21 original cells diverge in the same way as the others.
- **Evidence files:**
  - liteinst-logdiffs.txt: 6010 paths.
  - liteinst-parity-history.tsv: one row per run, with verdict and first-divergence histograms.
  - aio-liteinst-celldirs.txt: 40 aio-refusal LiteInst cell dirs, the earliest from 2026-08-14T22:53. No LiteInst logdiff exists before e385; earlier cell dirs contain only verify-1.json.
  - fe41-to-158a-log.txt: the 59 commits between the before and after runs.
- **GitHub API:** the lookup of pull requests by commit returned 403 (the proxy destination filter blocks api.github.com for this agent identity). I cite commit URLs instead.

## 3. Where the records enter, and why ptrace never sees them (Phase 1, step 3)

In the citations below, `hermit@19553a64:` means the slot and `reverie@b0ede531:` means the Cargo checkout named above.

### 3.1 The launch injects the runtime through the guest environment

- **Dispatch:** `hermit@19553a64:hermit-cli/src/lib.rs:2599-2604`. For `Backend::Liteinst`, Hermit resolves the runtime with `liteinst_runtime_library_path()` (lib.rs:955; lib.rs:847 validates it and requires the `.revision` marker). It then calls `reverie_liteinst::LiteinstBackend::run_host_with_preload::<Detcore>(command, config, preload)`.
- **Entry point:** `reverie@b0ede531:reverie-liteinst/src/backend.rs:217-225`. `run_host_with_preload` calls `configure_host_command`.
- **Environment:** `reverie@b0ede531:reverie-liteinst/src/backend.rs:545-567`, `configure_host_command`.
  - It sets `LD_PRELOAD=<canonical runtime path>[:<existing value>]` and `REVERIE_LITEINST_HOST_RUNTIME=1` on the guest command.
  - The "existing value" falls back to *Hermit's own* `LD_PRELOAD`, through `std::env::var_os("LD_PRELOAD")` at line 556, when the guest command has none (see open risks).
- **Execution:** the guest then runs under reverie-ptrace with the Detcore tool.

### 3.2 glibc's loader loads the runtime inside the guest process

- **Order:** glibc's `dl_main` (elf/rtld.c) processes `LD_PRELOAD` before it checks for `/etc/ld.so.preload` (Nix renames that file to `/etc/ld-nix.so.preload`). `handle_preload_list` is bracketed by `rtld_timer_start` and `rtld_timer_accum`, and each of those executes one `rdtsc`.
  - That is exactly the record order in both lanes: rdtsc, the runtime's openat/read/fstat/mmap×5/close, rdtsc, then access(preload file).
  - On the KVM lane, ptrace logs the access at record 16 and LiteInst logs an rdtsc there.
- **Dependency:** loading the runtime also loads its dependency `libgcc_s.so.1` (host c2 syscall indexes 30-38).
- **Read-only relocation:** it adds 2 extra `mprotect` calls, one for the runtime and one for libgcc_s (c2 indexes 45-46).

### 3.3 The runtime's constructor runs inside the guest process

1. `reverie@b0ede531:reverie-liteinst/src/lib.rs:329-332` is the `.init_array` entry, built with feature `preload-constructor`. It calls `reverie_liteinst_initialize` (lib.rs:207-215).
2. `reverie@b0ede531:reverie-liteinst/src/runtime.rs:864-866`: `initialize_from_environment` calls `std::env::var_os(HOST_RUNTIME_ENV)`.
   - This allocates an `OsString`. By code reading, it is the first glibc `malloc` in the process, so it initialises the heap the program will later use: `getrandom` for the tcache key, then `brk(NULL)` and `brk(0x426000)`.
   - The begin marker is not logged in the Detcore stream, so this attribution is from reading the code, not from a log line.
3. `reverie@b0ede531:reverie-liteinst/src/runtime.rs:954-973`: `initialize_host_runtime_with` runs three steps:
   - it executes the begin-marker trap (line 967);
   - it runs `prepare()`, which in c2 opens `/proc/self/maps` 24 times and creates 8 memfds named `liteinst2-trampoline`;
   - it executes the ready-marker trap (line 970).
4. reverie-ptrace records the phase privately:
   - The phases are `PreExec`, `Waiting`, `Bootstrap` and `Ready` (`reverie@b0ede531:reverie-ptrace/src/task.rs:812-817`).
   - `after_exec` moves to `Waiting` (task.rs:849-853).
   - The begin marker moves to `Bootstrap` (task.rs:3295-3304, in `classify_liteinst_trap` at 3289).
   - The ready marker moves to `Ready` (task.rs:3412-3419, in `handle_sigtrap` at 3364).

### 3.4 Every one of those events reaches Detcore

- **Syscalls:** every seccomp stop goes through `handle_seccomp` (task.rs:4967).
  - The only bypass is for LiteInst mapping syscalls that the tool has not subscribed to (task.rs:4978-4980).
  - Every other syscall reaches `self.process_state.handle_syscall_event(self, syscall)` (task.rs:5014-5018), *whatever the LiteInst phase*.
  - Detcore logs `inbound syscall` at `hermit@19553a64:detcore/src/lib.rs:1857-1862`. The only gate is `guest_past_first_execve()` (detcore/src/tool_local.rs:2924), which is already true once the guest has exec'd.
- **rdtsc:**
  - A trapped `rdtsc` arrives as a SIGSEGV and goes to `handle_rdtscs` (task.rs:3542-3547, then task.rs:2894-2913).
  - `handle_rdtscs` calls Detcore's `handle_rdtsc_event` (`hermit@19553a64:detcore/src/lib.rs:1350-1364`), which adds to the logical clock and logs `inbound rdtsc`.
- **Comparator:**
  - The cross-backend record envelope keeps every record whose target is `detcore` (`hermit@19553a64:hermit-cli/src/bin/hermit/record_envelope.rs:127-140`).
  - Only host addresses are canonicalised (record_start.rs:262-270). Guest pointers such as the stack buffers above are compared exactly, as they should be.
- **Why ptrace never sees these events:** the ptrace backend injects no preload, so none of these syscalls or rdtsc reads happen at all.

### 3.5 Where the 333 extra syscalls come from (host aio-refusal, control c2 against c0)

Source: /var/tmp/cml/liteinst-evidence/control/c0-ptrace-baseline.stderr and c2-liteinst.stderr. Indexes count syscalls from 0 in c2.

| Part | Count | c2 indexes |
|---|---|---|
| Loader opens and maps the runtime (openat, read, fstat, mmap×5, close) | 9 | 3-11 |
| Loader timer rdtsc around LD_PRELOAD processing | 2 rdtsc | around 3-11 |
| Loader loads `libgcc_s.so.1` | 9 | 30-38 |
| Extra read-only relocation mprotects (libgcc_s, runtime) | 2 | 45-46 |
| Runtime constructor window, between the loader's final `munmap` (index 50) and the program's `io_setup` (index 367) | 316 | 51-366 |
| Program's own heap-setup syscalls that vanish on LiteInst (`getrandom`, `brk(NULL)`, `brk(0x426000)`) | −3 | not applicable |
| **Net** | **+333 = 374 − 41** | not applicable |

- **Constructor window by syscall:** read 179, close 32, mmap 26, openat 24 (all `/proc/self/maps`), statx 16, fcntl 14, memfd_create 8 (all `liteinst2-trampoline`), ftruncate 8, brk 4, rt_sigaction 2 (signal 5, SIGTRAP), munmap 2, getrandom 1.
- **rdtsc:** 10 on ptrace, 12 on LiteInst.
- **After the window**, the program's syscalls are io_setup, io_submit, io_getevents, io_destroy, fstat, write, exit_group.

## 4. What the guest program can observe: differences that survive removing the records

### Control experiment

- **Setup:** host, the harness's own aio-refusal fixture, the harness's flags (`--log info run --base-env=minimal --strict`, same binds and environment variables), run through /home/newton/work/dev-hermit/bin/safehermit with a 120 s deadline.
- **Evidence:** /var/tmp/cml/liteinst-evidence/control/ (`*.stdout`, `*.stderr`, `*.sh-report`, runs.txt).

| Run | Backend and extra environment | `arch_prctl(0x3001)` buffer | `io_setup` context pointer | `fstat(1)` buffer | `write(1)` buffer | syscalls / rdtsc | virtual time | rc, wall |
|---|---|---|---|---|---|---|---|---|
| c0 | ptrace | 0x7fffffffe9a0 | 0x7fffffffe988 | 0x7fffffffe210 | 0x4052a0 | 41 / 10 | 4,042,435 ns | 0, 3.74 s |
| c1 | ptrace + `LD_PRELOAX=<same runtime path>` + `REVERIE_LITEINST_HOST_RUNTIME=1` (same bytes; the loader ignores `LD_PRELOAX`) | **0x7fffffffe930** | **0x7fffffffe918** | **0x7fffffffe1a0** | 0x4052a0 | 41 / 10 | 4,046,492 ns | 0, 1.01 s |
| c2 | liteinst | 0x7fffffffe930 | 0x7fffffffe918 | 0x7fffffffe1a0 | **0x40cae0** | 374 / 12 | 29,892,094 ns | 0, 0.83 s |

- **Stack:** c1 loads nothing, yet reproduces every LiteInst stack pointer. So the stack shift (−0x70 at the first record, −0x70 at `fstat(1)`) is caused by the injected environment strings alone. It cannot be removed by any change to the records.
- **Heap:** the program's heap buffer moves only in c2 (0x4052a0 → 0x40cae0), because the runtime used the heap first. The program's own `getrandom`, `brk(NULL)` and `brk(0x426000)` are missing on LiteInst for the same reason.
- **libc position:** libc sits 0x4200000 bytes (66 MiB) lower in c2.
  - The tcache-key pointer passed to `getrandom` is 0x7ffff7e034f8 on ptrace and 0x7ffff3c034f8 on LiteInst.
  - libc's read-only relocation `mprotect` is at 0x7ffff7df8000 on ptrace and 0x7ffff3bf8000 on LiteInst.
  - The runtime's 68,596,738-byte span is mapped first.
- **Time:** the rdtsc values the loader reads differ, because the logical clock has counted the extra syscalls. At the 10th rdtsc, ptrace is at logical syscall 29 and LiteInst at 49. Total virtual time is 4.04 ms against 29.89 ms.
- **Extra state:** 8 memfd trampolines, a SIGTRAP handler (`rt_sigaction(5, …)`) and `libgcc_s` in the link map exist only on LiteInst.

### KVM-lane census

- **Source:** read-only, from /home/newton/work/dev-hermit/ignored/validate/artifacts/validate-kvm-hermit-6f5294822bf9-01a0d5c2-1790493347418765214-4c60dc00/e2e/runs/validate-kvm-hermit-6f5294822bf9-01a0d5c2-1790493347418765214-4c60dc00/. The table is in /var/tmp/cml/liteinst-evidence/kvm-lane-program-pointer-divergence.txt. I recomputed the summary figures from that table today.
- **Stack:** the program's own `fstat(1)` stack pointer differs in **97 of 97** cells (−0xc0 in 68, −0xd0 in 29).
- **Heap:** the program's own `write(1)` buffer differs in **96 of 97** cells (+0x4c70 in 92, and one cell each at +0x44f0, +0x4ea0, −0xb3a0 and +0x320). The only exception is `inline-syscall-sites`, whose buffer is static.
- **Syscall counts per cell:**
  - ptrace: min 32, median 44, max 5462;
  - LiteInst: min 376, median 388, max 5805;
  - extra: min 343, median 344, max 385.
- **Next record after the load window (aio-refusal, verify-logs):**
  - If the load window were skipped, the next record, `access(0x7ffff7ff3580 -> "/etc/ld-nix.so.preload")`, would match.
  - The record after that already differs: the libc `openat` path buffer is 0x7fffffffda10 on ptrace and 0x7fffffffd930 on LiteInst (−0xe0).
- **Unmatched directory:** that run has 98 first-attempt LiteInst backend-parity-c directories for the 97 planned cells. The extra one, `cpuid-probe`, has no ptrace comparison. Its same-backend verify passed (`verified=true`, `bitwise_parity=true`), and it is not in the portable-lane plan. I did not find which selector adds it.

## 5. Why there is no principled Hermit-side fix (the boundary argument)

The task requires any removal to happen at the producer, to cover only syscalls the guest program did not issue, and never to hide a guest syscall inside the same window. Hermit cannot meet that bar:

1. **Detcore cannot see the phase.**
   - reverie's `Guest` trait exposes only `is_command_bootstrap()` (`reverie@b0ede531:reverie/src/guest.rs:103`). That flag covers the pre-exec launcher, not LiteInst.
   - The LiteInst phase lives in reverie-ptrace's private `LiteinstRuntimeState` (task.rs:812-853).
   - Any Hermit-side boundary would have to be guessed from record content.
2. **The loader's work is interleaved and ambiguous.**
   - The runtime load (indexes 3-11) and the `libgcc_s` load (30-38) are issued by the same loader code that loads the program's own libraries, in the same `Waiting` phase.
   - `libgcc_s` can be the program's own dependency, as it is for C++ programs and anything that uses unwinding. A filter by path would hide that program's genuine loads.
   - No marker separates "loading for the runtime" from "loading for the program".
3. **The markers are not a trustworthy boundary even inside reverie.**
   - backend.rs:213-215: "Trap markers … are not a security boundary against arbitrary code already executing in the guest".
   - Also, the 3 heap-setup syscalls happen *before* the begin marker. A begin-to-ready window would not contain them, and it would still leave the loader records and the program's missing heap setup.
4. **On this host the first divergence precedes the runtime load entirely.** It is `arch_prctl(ARCH_CET_STATUS, stack)`, moved by the injected environment (control c1). No record filter reaches it.
5. **Removing records would still leave guest-observable divergences:** stack, heap, libc position, rdtsc values, link map, memfds and signal disposition (section 4). The task says these must not be hidden, and the KVM census shows them in the program's own records in 96-97 of 97 cells.
6. **Giving the ptrace reference the same environment would lower the bar.** That is what c1 does, and it makes the stack pointers match, but it changes what the reference measures. I ran c1 only as a diagnostic control. I did not change the comparator, the cell lists, the manifests or any gate.

## 6. The change needed in reverie

These are the changes reverie would need, with evidence for each. I edited nothing in reverie.

- **(a) Stop mutating the guest environment.**
  - Where: `reverie-liteinst/src/backend.rs:545-567` (`configure_host_command`).
  - Change: remove the `LD_PRELOAD` / `REVERIE_LITEINST_HOST_RUNTIME` injection, including the fallback to Hermit's own `LD_PRELOAD` at line 556. The configuration should travel through the explicit entry point that already exists: `reverie_liteinst_initialize_host(*const HostRuntimeConfig)` at lib.rs:217-274. Its documentation says it "does not inspect or mutate guest environment selectors", and no launch path uses it today.
  - Effect: removes the stack shift proven by c1 (all 97 KVM cells' `fstat(1)` pointers and the host's first divergence).
- **(b) Load the runtime from the tracer, not from the guest's loader.**
  - Where: replace the `.init_array` constructor (lib.rs:329-332, feature `preload-constructor`) with a tracer-driven load followed by an injected call to `reverie_liteinst_initialize_host`.
  - The building blocks exist but are not wired to any launch path: `reverie-ptrace/src/target_loader.rs` (1020 lines, exported at reverie-ptrace/src/lib.rs:53) and `target_loader/ordinary.rs` (519 lines, `resolve_host_initializer`). Nothing outside the module and its tests (reverie-liteinst/tests/target_loader_conformance.rs) calls them.
  - Caveat: `target_loader.rs` resolves the *guest's* `dlopen`. Loading through the guest's `dlopen` would fix (a), but it would still allocate from the guest's glibc heap, map into the guest's normal mmap range and pull in `libgcc_s`. On its own it therefore does not remove the heap and libc shifts; that needs (d).
- **(c) Run the tracer's injected load and initialisation without passing it to the tool.**
  - Execute the tracer-injected load and initialisation syscalls, and any runtime `rdtsc`, *without* calling `handle_syscall_event` / `handle_rdtsc_event`.
  - Bound this by the tracer's own injected frame, task ID and tracer-owned state, never by guest-executable markers.
  - Precedent: reverie-ptrace already runs injected syscalls that the tool has not subscribed to through `untraced_syscall` (task.rs:3088-3102, function at 7077), with no tool dispatch.
  - Because the tracer itself issues these calls, no guest-issued syscall can fall inside the boundary. This is the precision property the task asks for, and it is only attainable inside reverie.
  - A smaller variant that only makes today's begin-to-ready window untraced would remove about 313 records (the 316-syscall constructor window minus the 3 heap-setup calls that precede the begin marker). It would not reach parity, for the reasons in section 5.
- **(d) Make the runtime self-contained.** It needs:
  - its own memory arena in a private mapping, placed outside the kernel's default top-down mmap range, instead of glibc `malloc`;
  - no `DT_NEEDED` on `libgcc_s` (build with `panic=abort`);
  - no use of the guest's static TLS block.
  - Effect: removes the libc −0x4200000 shift, the heap pre-initialisation (96 of 97 `write(1)` buffers) and the missing program `getrandom`/`brk` calls.
- **(e) Owner decision: what may remain visible.** Even after (a)-(d), the guest can observe:
  - the runtime mapping and the `liteinst2-trampoline` memfds in `/proc/self/maps`;
  - the SIGTRAP disposition;
  - the seccomp filter (`/proc/self/status`).

  Under a bitwise ptrace-parity gate, any cell that reads these will differ. Whether that is accepted, or whether such cells are exempt, is an owner decision about product semantics. I am not proposing it as a way to turn the gate green.

## 7. Record and replay

- LiteInst supports only `run`: `hermit@19553a64:hermit-cli/src/bin/hermit/main.rs:359-363` refuses record and replay with the LiteInst backend.
- `hermit-cli/src/liteinst_record.rs` is a tracing-record layer that says of itself "This preparation is not wired into backend verification" (line 18).
- No recording or replay consumes these events, and no recording format is involved. I proposed no format change.

## 8. Commands and exit statuses

- **Full command list:** /var/tmp/cml/liteinst-evidence/my-commands.txt holds 160 entries, extracted from my transcript with output truncated to 600 characters.
- **Index:** /var/tmp/cml/liteinst-evidence/my-commands-index.txt has one line per command, with the rc where the command printed one. Read-only `grep`/`sed`/`ls` commands print none.
- **Style:** every rc below was captured as `cmd > file 2>&1 ; rc=$?`, never after a pipe.

| # | Command (abridged) | rc | Wall | Evidence |
|---|---|---|---|---|
| 1 | `git -C <slot> rev-parse HEAD`, `branch --show-current`, `status --porcelain=v1 --untracked-files=all` (before the first build) | 0; 0 lines | – | slot-status-initial.txt |
| 2 | `cargo build --release --locked -p hermit --bin hermit -p hermit-manifest-plan --bin test-harness` (first try) | **101** | – | missing `agent-utils/rs/dagrun/Cargo.toml`: the submodule is uninitialised in the slot |
| 3 | `git --git-dir=/home/newton/work/dev-hermit/hermit/.git/modules/agent-utils archive --format=tar 26dd3eaa34ea… > …/agent-utils-26dd3eaa.tar` (26dd3eaa is the agent-utils gitlink at 19553a64); `tar -x -C <slot>/agent-utils`; `git status` | 0; 0; 0 (0 lines) | – | agent-utils-26dd3eaa.tar (25,036,800 bytes) |
| 4 | the same cargo build (second try) | 0 | 89.6 s | build1.log, build1.rc |
| 5 | `scripts/stage-liteinst-runtime.sh release …/libreverie_liteinst.so …` | 0 | 22.9 s | stage1.log |
| 6 | `test-harness plan --lane portable --category backend-parity-c --backend liteinst --ci-only` (first try) | **2** | – | the `hermit-manifest-plan` helper binary was not built |
| 7 | `cargo build --release --locked -p hermit-manifest-plan --bins` | 0 | 27.7 s | build2.log |
| 8 | the same plan command | 0 (97 cells) | – | plan-liteinst.txt |
| 9 | `test-harness run … --test backend-parity-c/aio-refusal --mode verify --backend liteinst --parity-reference ptrace` | **1** (cell FAILED) | 1.6 s | phase1-aio-host.out, e2e/ |
| 10 | `safehermit … hermit … --backend ptrace` (c0) | 0 | 3.74 s | control/c0-* |
| 11 | `safehermit … --backend ptrace --env LD_PRELOAX=$RT --env REVERIE_LITEINST_HOST_RUNTIME=1` (c1) | 0 | 1.01 s | control/c1-* |
| 12 | `safehermit … --backend liteinst` (c2) | 0 | 0.83 s | control/c2-* |
| 13 | `find ignored/validate/artifacts -maxdepth 6 -name backend-parity-logdiff.json -path '*liteinst*'` (read-only) | 0 (6010 paths) | – | liteinst-logdiffs.txt |
| 14 | `find … -type d -name backend-parity-c-aio-refusal-verify-liteinst` (read-only) | 0 (40 dirs) | – | aio-liteinst-celldirs.txt |
| 15 | `git show --stat` / `git show e385d5f72a` (shared tree, object reads only) | 0 | – | e385d5f72a-stat.txt, e385d5f72a-diff.txt |
| 16 | `git merge-base --is-ancestor e385… fe41…` / `… 158a…` | 1 / 0 | – | section 2 |
| 17 | `git merge-base --is-ancestor e385… 290280a9…`, `… 7ca5b4e8…`, `… 19553a64…` | 0 / 0 / 0 | – | section 2 |
| 18 | `git log fe41…158a` | 0 (59 commits) | – | fe41-to-158a-log.txt |
| 19 | `with-proxy gh api repos/rrnewton/hermit/commits/<sha>/pulls` for 3 commits | 1 (HTTP 403, destination filter) | – | pr-*.txt |
| 20 | `git -C <slot> status --porcelain=v1 --untracked-files=all` (final, twice) | 0; 0 lines | – | slot-status-final.txt, slot-status-final2.txt |

- **Other steps:** all other steps were reads of source, artifacts and logs: `sed -n`, `grep`, and Python tallies of JSON and logs. None of them wrote outside /var/tmp/cml/liteinst-evidence/ or /tmp/claude-coord-notes/cml-w-liteinst-*.
- **Restrictions honoured:**
  - I ran no `make validate`, validate-run, start_unit, ci-hub validate or wrkslots command.
  - I did not write to the parent checkout or to the parent's `hermit/` or `reverie/` submodules.
  - I pushed nothing.

## 9. Test results

- **Fix tests:** there are none, because there is no fix (Phase 2 was not entered). I did not run the required fail-before/pass-after tests or the re-run of 8 cells for the same reason.
- **Reproduction:** 1 of 1 cells run, 1 of 1 FAILED, with 2 of 2 attempts diverging at record 14 / syscall 2 on this host. Wall 1.6 s; attempts took 731 ms and 669 ms.
- **Controls:** 3 of 3 exited 0, with identical 9-byte stdout. Wall times 3.74 s, 1.01 s and 0.83 s.
- **Historical census:** 6010 of 6010 LiteInst parity logdiffs diverged (31 runs), and 21 of 21 pre-comparison cells passed their repeat check against themselves.

## 10. Commits and slot state

- **Commits:** none. HEAD is still 19553a64308ca123bbde7cc3d7720e0aa72e795a on `claude/liteinst-runtime-self-load-20260927`.
- **Status:** `git -C /home/newton/work/dev-hermit/worktrees/slots/cml-liteinst status --porcelain=v1 --untracked-files=all` returned rc=0 with 0 lines (slot-status-final2.txt, re-checked at report time).
- **Submodules:** `git submodule status` shows `agent-utils`, `reverie` and `third-party/rr` as uninitialised (`-` prefix). The extracted agent-utils files (step 3) sit in the uninitialised submodule directory, so `git status` does not show them (see open risks).

## 11. Later divergences exposed

These are the next findings, for whoever takes the reverie redesign. None are masked.

1. **Environment-driven stack shift.** It is proven by control c1 and seen in the program's own `fstat(1)` pointer in 97 of 97 KVM cells. On this host it is the *first* divergence, at `arch_prctl(ARCH_CET_STATUS)`.
2. **Program heap pre-initialised by the runtime.** The program's `getrandom`/`brk`/`brk` are missing, and its `write(1)` buffer moved in 96 of 97 KVM cells (0x4052a0 → 0x40cae0 on this host).
3. **libc 0x4200000 bytes lower**, because the runtime's 68,596,738-byte span is mapped first.
4. **Different rdtsc values and total virtual time.** The 10th rdtsc is read at logical syscall 29 on ptrace and 49 on LiteInst; total virtual time is 4.04 ms against 29.89 ms.
5. **Extra guest-visible state:** `libgcc_s.so.1` in the link map, 8 `liteinst2-trampoline` memfds, a SIGTRAP handler, 24 reads of `/proc/self/maps` and the seccomp filter.

## 12. Open risks and owner decisions

1. **Owner decision.** The portable backend-parity-c node has required bitwise ptrace parity of the LiteInst host hybrid since e385, and 6010 of 6010 comparisons have failed. The qualification document says these selections are not cross-backend parity evidence. The owner should decide whether the host hybrid belongs under this gate before the reverie redesign lands. I changed nothing in the gate.
2. **Hermit's own `LD_PRELOAD` can leak into the guest.** reverie backend.rs:554-557 falls back to Hermit's process environment when the guest command has no `LD_PRELOAD`, so a preload set on the Hermit process would be forwarded into every LiteInst guest. This is from reading the code only; I did not run it.
3. **cpuid-probe has no ptrace comparison.** It is the 98th LiteInst backend-parity-c directory in the KVM run and not in the portable-lane plan. I did not find which selector adds it.
4. **Extracted agent-utils files in the slot.** /home/newton/work/dev-hermit/worktrees/slots/cml-liteinst/agent-utils/ holds files extracted from agent-utils 26dd3eaa, so the build could find `dagrun`. `git status` does not show them because the submodule is uninitialised. Whoever retires the slot should expect this directory to be non-empty.
5. **Lane difference.** My live reproduction ran on the host (glibc 2.34), not the KVM lane's Nix glibc 2.42 container. That explains the host's first divergence (14/2 against 16/3), and the KVM-lane conclusions rest on the retained read-only artifacts (6010 logdiffs and the 97-cell census).
6. **Attribution of the heap-setup calls.** Attributing the 3 heap-setup syscalls to `std::env::var_os` (runtime.rs:865), before the begin marker, is from reading the code; the markers are not logged. The count of about 313 records for a begin-to-ready window depends on it.
