# Direct XSAVE / XRSTOR metadata diagnostic

## Result

**Yes: directly restoring this process's own untouched, valid native XSAVE
image reproduces exactly the byte-512 bit-7 change.** No signal, custom return,
SUD, Detcore, or runtime hook is needed for this observation.

One fresh ordinary process ran each case in attempt-02. Each captured the full
2440-byte standard XSAVE image twice. All four qualified with starting
XSTATE_BV `0x2a7`, XCOMP_BV `0`, XCR0/save/restore mask `0x2e7`, and the
required Hi16 payload. After XCOMP_BV was also zero in every case.

| Profile | Interval | XSTATE_BV before → after | Full-image comparison | Process status |
|---|---|---|---|---:|
| Memory-loaded zero Hi16 | XSAVE → XSAVE | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |
| Memory-loaded zero Hi16 | XSAVE → XRSTOR same image → XSAVE | `0x2a7 → 0x227` | only offset 512: `a7 → 27` | 1 |
| Seeded all-ones Hi16 | XSAVE → XSAVE | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |
| Seeded all-ones Hi16 | XSAVE → XRSTOR same image → XSAVE | `0x2a7 → 0x2a7` | all 2440 bytes equal | 0 |

The zero profile's 1024 Hi16 bytes remained zero; the nonzero profile's remained
`ff`. The whole-image diagnostic remains **failed** for zero restore, and the
admitted DAG exits 1. No metadata is excluded or normalized. Independent `cmp`
agrees: `513 247 47` means one-based byte 513, octal values corresponding to
zero-based offset 512 and hexadecimal `a7 → 27`.

The direct zero-restore starting header (all 64 bytes) and Hi16 payload match
native-06's ordinary-signal input exactly. The entire initial images do not:
75 other bytes differ. `profile-comparison.txt` and
`native-06-starting-image.differences` preserve this distinction.

## Method and guards

The initial bounded search of the cited native-02/03/06 and diagnostic-15
reports, retained controls, and related LiteInst notes found capture-only and
ordinary-signal controls, but no explicit standalone direct-restore control.
Existing runtime XSAVE/XRSTOR trampolines were not counted as that control.
This is not an exhaustive filesystem absence claim.

The zero profile copies diagnostic-15's VMOVDQU64 loads into zmm16–31 from the
still-zero after buffer, before either capture. The nonzero profile copies its
retained VPTERNLOGD all-ones method. Both use the existing FNINIT/FLD1/FLDPI and
YMM0/YMM1 seeding sequence. Source references:

- `ignored/liteinst-owned-cpuid-20260905/diagnostic-15/controls-delta-from-diagnostic-12.patch`,
  SHA-256 `64be0f341a1ad0811420a00a10ae7f53c4fb95d04893397cbac789ffc9bc42c4`.
- Its `before/owned_cpuid.rs`, SHA-256
  `b8f75d7f75829b92f634df5ae1165fcf0c24a34be7612e3b84e4e3db0c8d8205`.

CPUID guards require XSAVE/OSXSAVE/AVX, AVX2, AVX512F, the exact supported and
enabled mask, 2440-byte size, user-component sizes/nonoverlapping bounds, and
Hi16 offset 1408/size 1024. Allocations are 64-byte aligned. Buffers are cleared
before capture only; no captured header, payload or mask is edited. XRSTOR64
receives the same address just written by XSAVE64. It restores no PC or general
register context. No alternate stack or signal frame exists.

Emitted code in `attempt-02/disassembly.txt` confirms XSAVE64 at `0x401a5d`,
the scalar mode branch, XRSTOR64 `[rdi]` at `0x401a65`, and immediately adjacent
XSAVE64 `[rsi]` at `0x401a69`. There are no calls or syscalls in that interval.
The C harness has no threads or asynchronous sources, blocks maskable signals,
refuses pending signals and armed interval timers, and installs no handlers.
All four measured intervals report zero voluntary/involuntary context switches
and a successful post-capture quiet-state check. Hardware interrupts were not
disabled; this is an ordinary-process observation, not isolated bare metal.
Loader-environment/preload guards passed, and core dumps were disabled.

## Attempts, admission and identities

Attempt-01 compiled but all four processes exited 78 **before capture**:
the initial diagnostic guard incorrectly assumed opmask offset 1088. Actual
CPUID reports 832. One targeted refinement replaced that unsupported layout
assumption with CPUID-derived bounds while retaining exact component sizes,
mask, image size and Hi16 layout. Attempt-02 then produced the qualified results
above. No further refinement or repeat was performed. Both attempts retain
their original source, binary, command streams, statuses and hashes.

The documented native-06 admission shape was reused with a new directory:

```text
/home/newton/work/dev-hermit/agent-utils/bin/dagrun run --dag /home/newton/work/dev-hermit/experiments/liteinst-xrstor-metadata-20260905/run.json --admission --max-cpus 2 --max-steps 1 --max-mem 2G --run-timeout 180 --perf-dir /home/newton/work/dev-hermit/experiments/liteinst-xrstor-metadata-20260905/resources-02
cc -std=c11 -O2 -Wall -Wextra -Werror -mgeneral-regs-only -fno-tree-vectorize probe.c probe.S -o attempt-02/probe
```

Admission was granted and cgroup-v2 containment observed: two-CPU quota,
2 GiB memory, no swap; step wall/CPU bounds 150/300 seconds. Each case's exact
executable/arguments are in its `command` file. This is not a product validation
receipt. No network, product edits, slot operations, extra agents or live timer
source reads occurred.

Host: AMD EPYC 9D85, AuthenticAMD, CPUID.1 EAX `00b10f10`, model 17, stepping 0;
Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`; GCC 11.5.0. Full host/toolchain output,
ELF/disassembly, CPU leaves, loader/libc hashes and per-case streams are retained.

- Final C: `c22b9f4d548e5b9872939879d6b21567ac1bda9864038bd4ad5e7914cd62e9d9`.
- Assembly: `e33163de0ae01ca71c6ae433e49dcc4164dbf7eafb621962094fc1381b46bd6c`.
- Attempt-02 binary: `1c3a6c762b741362cbc2f74f817f635561d5ee99b212dd742152d9e968497579`.
- Affected before image: `54a6160f38fd89cd7e115fc243dab7560103849082e129edc4859cb0ae416c7d`.
- Affected after image: `ca4b493f4465d978a6e1cb25a6f9bc04f4e5d410794f1d360ce6d7833096d418`.

`identities.sha256` covers all eight complete raw images, sources/driver and both
binaries. Per-attempt manifests preserve failed/unqualified inputs; the final
`EVIDENCE.sha256` additionally covers reports, logs and independent comparisons.

## Interpretation boundary

The extra direct XRSTOR is sufficient to reproduce the observed signature in
this native setup. The symptom therefore does not require a signal/kernel
return path or custom runtime. This does not establish that those other paths
have no additional defects, that all initial states were identical, or that
repeated executions are deterministic.

**The missing primary AMD manual rule remains outstanding.** This result is
not normative permission to change XSTATE_BV, an ISA-conformance verdict, proof
of benignness, or permission to weaken the original FP comparison. It neither
closes native-02 nor changes any product test/comparator or timer implementation.
