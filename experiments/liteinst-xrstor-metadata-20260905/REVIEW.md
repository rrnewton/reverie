# Independent direct XSAVE/XRSTOR diagnostic review

## Findings and verdict

**APPROVE only the bounded causal-observation claim. No concrete error found that defeats that claim.**

In the retained, qualified AMD execution, restoring the process's own untouched standard XSAVE image with XRSTOR and then saving again reproduces the one-byte signature: offset 512 changes from `a7` to `27`, meaning XSTATE_BV `0x2a7` becomes `0x227`. The remaining 2439 bytes match. The capture-only zero case and both nonzero controls retain full-image equality within their respective processes. No signal/custom-return/SUD/Tool mechanism is needed in this reproducing program.

**The zero-restore whole-image comparison still fails, and the diagnostic DAG still fails.** This review does not approve normalization, a product-code change, weakening a comparator, ISA conformance, complete FP fidelity, repeated determinism, or resolution of any earlier product failure. The primary AMD manual rule remains missing. Other paths may have additional defects.

Reviewed 2026-09-05. Execution was **not independently replayed**. Review operations only read retained artifacts, verified hashes, disassembled the retained executable, and compared retained raw bytes. No compilation, experiment/guest execution, network, extra agents, live product-source reads, or product modifications occurred. Only this requested report was written; the product timer worker is unaffected.

## Exact target and provenance

All experiment-relative references below are under `/home/newton/work/dev-hermit/experiments/liteinst-xrstor-metadata-20260905`.

| Artifact | SHA256 |
| --- | --- |
| `RESULT.md` | `863e105c28c22e2f0dc1e8997a85fac021948545c9b3960401d33fe213440608` |
| `EVIDENCE.sha256` | `455af08da379bef1ebd4a15ab9968a08c1fb976edefe3cf7884ac420f0c2a6d3` |
| `identities.sha256` | `1bb601b70ba836ba59a60345ba1ffd71b852376af09b0f178fd0ea6b632bd999` |
| Final `probe.c` | `c22b9f4d548e5b9872939879d6b21567ac1bda9864038bd4ad5e7914cd62e9d9` |
| `probe.S` | `e33163de0ae01ca71c6ae433e49dcc4164dbf7eafb621962094fc1381b46bd6c` |
| `attempt-02/probe` | `1c3a6c762b741362cbc2f74f817f635561d5ee99b212dd742152d9e968497579` |
| `attempt-02/disassembly.txt` | `e4b56e595c567820fda733256993ac1c177d6eac31175a2d2e3ad850816e47fc` |
| `attempt-02/zero-restore/before.bin` | `54a6160f38fd89cd7e115fc243dab7560103849082e129edc4859cb0ae416c7d` |
| `attempt-02/zero-restore/after.bin` | `ca4b493f4465d978a6e1cb25a6f9bc04f4e5d410794f1d360ce6d7833096d418` |

The supplied RESULT hash matches. All entries in the final evidence manifest, identities manifest, and both per-attempt evidence manifests verify. Current experiment source/driver files match their retained attempt-02 copies. Fresh read-only `objdump -d -Mintel attempt-02/probe` produces exactly the retained disassembly hash. This binds inspection of emitted instructions to the retained executable; it is not an independent rebuild or execution attestation.

Applicable guidance: parent `dev-hermit/AGENTS.md`, the explicit bounded-review instructions, `code-review`, and `presenting-quantitative-data`. Neither the experiment directory nor its `experiments/` parent has an additional AGENTS file. The explicit no-extra-agents restriction governs this review.

## Operand validity, interval, and compiler boundary

- **Supported/enabled state:** `probe.c:23` checks XSAVE/OSXSAVE/AVX and AVX2/AVX512F, XCR0 exactly `0x2e7`, and CPUID leaf D's supported mask and current/maximum standard image sizes exactly `0x2e7` and 2440. AMD vendor/model is observed in retained output, not a separate vendor-rejection condition in the program. Approval is for that observed host only.
- **Complete component bounds:** `probe.c:46` checks user components, exact sizes, ordered nonoverlap, image bounds, and exact Hi16 offset. Recorded `(component, offset, size)` tuples are `(2,576,256)`, `(5,832,64)`, `(6,896,512)`, `(7,1408,1024)`, `(9,2432,8)`. They cover the standard extended area through byte 2439 without overlap. The header is outside these payload ranges.
- **Alignment and ownership:** `probe.c:104` allocates two separate live 2440-byte buffers with `posix_memalign(...,64,...)`, clears both before capture, and checks their alignment. The emitted caller contains these allocations, scalar/REP clearing, and alignment check. The only loads used to construct zero Hi16 state are before either capture, from the still-zero after-buffer. Neither header nor captured payload is synthesized or patched after XSAVE.
- **Same address and mask:** `probe.S:25` sets EDX:EAX to `0:0x2e7`; XSAVE64 writes `[rdi]`. Between that save and the optional restore, only `test ecx,ecx` and a branch occur. XRSTOR64 reads that same `[rdi]`; the following XSAVE64 writes `[rsi]` with the same mask. No intervening instruction changes those pointers or EDX:EAX. XRSTOR does not restore PC/general-register context here.
- **Exact emitted interval:** `attempt-02/disassembly.txt:714` shows mask setup at `0x401a56/0x401a5b`, first XSAVE64 at `0x401a5d`, scalar test/branch at `0x401a61/0x401a63`, XRSTOR64 at `0x401a65`, and second XSAVE64 at `0x401a69`. The restore and second save are adjacent. There is no call, syscall, signal operation, image store, or unrelated FP/vector instruction in that interval. The scalar test changes integer flags, not the measured XSAVE state; “no other state writes” must be understood with that distinction.
- **No identified compiler/ABI contamination:** the capture interval is an out-of-line assembly function, not inline assembly with omitted compiler clobbers. The C call follows SysV integer/pointer argument placement. The function preserves its stack and callee-saved general registers, saves/restores x87 control and MXCSR, and resets the x87 stack before returning. Its vector/x87 seeding is intentional and caller-saved. Cleanup FNINIT/FLDCW/LDMXCSR/VZEROUPPER occurs only after the second image is captured and does not edit the saved buffers. The C build uses `-mgeneral-regs-only -fno-tree-vectorize`; emitted FP/vector operations are confined to the assembly routine. No material C, ABI, bounds, or compiler-FP interaction was found that explains away the measured difference.
- **Raw input structure:** every retained before-image has XSTATE_BV `0x2a7`, XCOMP_BV zero, and all remaining header bytes zero. The active bitmap is a subset of `0x2e7`. The standard image comes directly from native XSAVE immediately before restoration, under unchanged controls and mask. These checks support well-formed same-process input; they do not supply the missing normative rule for the observed metadata transition.

The payload formation also matches the retained antecedent methods: diagnostic-15's memory loads into zmm16–31 for the zero profile and its prior VPTERNLOGD all-ones method for the nonzero profile. The cited frozen patch and `before/owned_cpuid.rs` hashes match the values in RESULT. Other inherited components are not asserted identical across processes.

## Actual counts and raw comparisons

Population: **four fresh processes in attempt-02, one per case, with two 2440-byte images per process**. There are no repeats. Each row is backed by its own `command`, `status`, `stdout`, `stderr`, `before.bin`, and `after.bin` in `attempt-02/<case>/`. All four report `qualified=1`, correct initial header/payload, `quiet_after=1`, zero voluntary/involuntary context-switch deltas, and empty stderr.

| Case / executable arguments | XSTATE_BV before → after | Independent full-image comparison | Recorded process status |
| --- | --- | --- | --- |
| `zero-capture`: `capture zero` | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |
| `zero-restore`: `restore zero` | `0x2a7 → 0x227` | only offset 512 differs, `a7 → 27` | 1 |
| `nonzero-capture`: `capture nonzero` | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |
| `nonzero-restore`: `restore nonzero` | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |

I independently checked all eight image sizes, compared all four complete pairs, decoded all eight headers, and counted all 1024 Hi16 payload bytes in each image. Zero profiles remain entirely zero there; nonzero profiles remain entirely `ff`. XCOMP_BV and the rest of each header stay zero. Independent `cmp -l` for zero restore produces only one-based byte 513 with octal values 247 and 47, matching offset 512 and hexadecimal `a7 → 27`. The three other comparisons return 0. This is not a header-only comparison being represented as full-image equality.

The C comparison at `probe.c:133` inspects every byte. Qualification and equality remain distinct: line 146 returns 78 for unqualified input, 1 for a qualified unequal image, and 0 only for qualified equality. `run.sh:33` retains all four results and aggregates any nonzero status into failure. Both attempts' aggregate status is 1; attempt-02's admission log reports zero passed DAG nodes and one failed node. Its successful causal observation does not relabel the equality test or DAG as passing.

## Guard refinement and goalpost-moving assessment

Attempt-01 compiled successfully but all four processes exited 78 before buffer capture. Their retained outputs stop at CPUID component 5, whose actual offset is 832, not the hard-coded 1088. No raw-image result from attempt-01 is counted as a qualified observation.

The only C change replaces guessed fixed component offsets with CPUID-derived, ordered, bounded layout validation. It also replaces the guessed component-6 offset 1152; the observed offset is 896. Exact component sizes, total size, enabled/supported mask, user-state requirement, and Hi16 location remain checked. In fact, the required extended sizes sum to 1864 bytes, exactly `2440 - 576`; ordered nonoverlap and those bounds leave no room for gaps and force the observed contiguous layout. The assembly is identical across attempts. Driver changes parameterize the attempt directory without changing comparison/status semantics.

- **Assertions weakened: no outcome/FP assertion weakened.** Every image byte is still compared, and the affected case still fails.
- **Tolerance widened, exemption added, case skipped, comparator relaxed: no outcome tolerance, exemption, skip, or comparator change.** The admission predicate did change; this is explicitly audited rather than hidden. Replacing an erroneous layout guess with checked CPUID bounds is a justified guard correction, not permission to ignore a state mismatch.
- **Failure renamed or relabelled as a pass: no.** Both four-case attempts and their nonzero aggregate statuses are retained. The result distinguishes a reproduced signature from a passing preservation test.
- **Check deleted instead of satisfied: no relevant safety or preservation check simply removed.** Layout guesses are replaced with structural size/bounds checks; exact mask/Hi16/size and full-image comparison remain. There is no product comparator or test patch in this diagnostic artifact set. No claim that live product files are unchanged is independently made from a forbidden live-source read.

## Older signal case: partial match only

Against retained `ignored/liteinst-owned-cpuid-20260905/native-06/signal.fp-before.bin`, I independently compared all 64 header bytes and all 1024 Hi16 bytes to the direct zero-restore before-image: both ranges match exactly. The complete initial images differ at **75 other byte positions**. Recomputing the old-to-new byte-difference list reproduces the hash of `native-06-starting-image.differences` (`bf4ad4be7d5d37cb8f1005163d4649060a40c43edfe4631c19b425afa91bab0d`). This confirms the comparison's direction and rules out an accidental whole-state equivalence claim.

The old signal before/after files verify against their retained manifest and independently show the same sole offset-512 `a7 → 27` change. Their hashes are `918402825d815254d56cd9a1e86c338248950c038c3f2bce7339347e2368d2b8` and `0654422207b228fdb28fd0b0ffb15b45e52bac9cfffa2402b053fb7f0f48d581`. The direct diagnostic reproduces the affected signature without requiring the signal mechanism; it does not prove that the older interval's entire state or all causes are identical.

## Environment and remaining limits

The retained attempt-02 command stream shows GCC 11.5.0 compiling once with `-std=c11 -O2 -Wall -Wextra -Werror -mgeneral-regs-only -fno-tree-vectorize`. The ELF is a normal x86-64 executable whose only NEEDED library is libc; loader/libc/compiler/cache hashes were checked by the original runner. No experiment execution was performed by this reviewer.

Recorded host: AuthenticAMD EPYC 9D85, CPUID.1 EAX `00b10f10`, family 26/model 17/stepping 0, Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`. The driver rejects exported `LD_*`/`GLIBC_TUNABLES` and any existing or symlinked `/etc/ld.so.preload`, and disables core dumps. The harness introduces no thread, handler, runtime hook, or signal delivery; it blocks maskable signals, refuses pending signals and armed interval timers before capture, and checks quiet state afterward. Reported context-switch deltas are observations, not a universal isolation proof or an extra equality exemption. Masking does not exclude hardware interrupts, all unmaskable events, or every conceivable external disturbance; this remains an ordinary native-process observation, not bare-metal isolation.

The recorded admission command uses `dagrun run --dag <experiment>/run.json --admission --max-cpus 2 --max-steps 1 --max-mem 2G --run-timeout 180`. Admission was granted with observed cgroup-v2 containment, two-CPU bandwidth quota, 2 GiB memory, no swap, 150-second step wall and 300-second CPU limits; outer safety ceiling is 240 seconds. Attempt-02 is recorded as a failed diagnostic node in 0.5 seconds, not a product validation receipt.

**Final verdict: approve the hash-bound causal observation only.** No extra experiment is required to accept this limited record. The missing primary AMD rule, interpretation of metadata, product normalization policy, earlier failing FP tests, repeated determinism, possible other bugs, and complete FP fidelity remain unresolved or outside scope. Source/evidence changes require a fresh review.
