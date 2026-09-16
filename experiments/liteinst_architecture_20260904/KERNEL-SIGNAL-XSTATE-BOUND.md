# Native signal-frame bound on the matched running kernel

Inspection: 2026-09-04 PDT (retained query 2026-09-05 UTC). This is a compiled-kernel allocation analysis, not a tested Hermit return implementation or general signal-support claim.

## Decision

For **this kernel image and native x86-64 ABI**, the exported `AT_MINSIGSTKSZ` is derived from the **maximum boot-configured user xstate size**, not the current thread's smaller/default allocation. Permission-driven native xstate growth stays within that maximum. The retained read-only host query returns **3,376 bytes**, with a maximum enabled user XSAVE size of **2,440 bytes**. No tile-state permission is available on this host.

This supplies a conservative **per-kernel-frame** allowance. It does **not** supply a sufficient entire signal-stack size: handler/compiler/libc stack consumption, simultaneous live frames, mapping ownership, and return/continuation lifetime still require independent proofs. `sigaltstack()` success and a guard page are not those proofs.

## Identity and provenance

- Running release: `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`.
- Image: `/usr/lib/modules/7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf/vmlinux-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`; `/boot/vmlinux-7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf` resolves to it.
- Image SHA256: `e4ca09342f074c21080a6d31d17fb7ad6f5fbe2c7b84e14759acfd97e16e0db2`.
- GNU build ID: `fa15f90afce5930f146e2138d2ba10eb95e3e5ad`.
- Prior availability audit byte-compared the image's entire 84-byte `.notes` with `/sys/kernel/notes`; SHA256 `4bab71b60fbb4fdb2a33125327bafac94f3f982378d5931f004dfd410eeab55a`. Image digest also matches its installed RPM manifest. See `KERNEL-LOCAL-DEBUG-AVAILABILITY.md` and its external evidence directory.
- Matching installed layout headers: `/usr/src/kernels/7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf/`, owned by the identically versioned `kernel-devel` RPM. Relevant header hashes/package owners are retained below.
- Full vendor `.c` sources, full source commit, and vendor patch set remain unavailable. `/rw/compile/...` below is **embedded DWARF provenance**, not an available source checkout. Instructions and DWARF offsets are the decisive evidence; header comments alone are not proof. The inspected ELF includes boot alternatives/static-branch sites, so on-disk NOPs are not assumed to be the live patched instructions.

Raw evidence root (`E` below): `/home/newton/work/dev-hermit/ignored/kernel-signal-xstate-bound-20260904/`. `identity-headers.txt`, `symbol-ranges.txt`, `source-lines.txt`, `SHA256SUMS`, and `commands.md` bind the analysis to exact inputs and commands.

## Allocation and exported size

Let `U` be the active thread's `fpstate.user_size`, `M = fpu_user_cfg.max_size`, `B` the altstack base, `L` its size, and `S` the interrupted native RSP. All formulas assume valid, nonwrapping canonical user addresses and native, non-compat delivery.

| Linked location | Proven operation | DWARF mapping |
| --- | --- | --- |
| `ffffffff821d43b6` | Native fixed `rt_sigframe` argument is `0x1b8 = 440` bytes; DWARF independently gives 440. | `arch/x86/kernel/signal_64.c:175` |
| `ffffffff821d4960..821d4985` | Read current fpstate pointer, then offset 4 (`user_size`); add 4 for XSAVE suffix; subtract, align down to 64. | `arch/x86/kernel/fpu/signal.c:441,498` |
| `ffffffff821d473a`, `821d477e..821d479a` | Native red zone subtracts 128 before stack selection; subtract fixed frame, align down to 16, subtract 8. | `arch/x86/kernel/signal.c:132` (red-zone instruction has unknown line) |
| `ffffffff8475dec5..8475ded8` | Read offset 0 of `fpu_user_cfg` (**max_size**, not default_size); add XSAVE suffix 4 and compat allowance 112. | `arch/x86/kernel/fpu/signal.c:513,527` |
| `ffffffff84759995..847599b0` | Store that result as `F`; compute `K = ((F + 813) OR 15) + 1`; store `max_frame_size`. | `arch/x86/kernel/signal.c:216,223` |
| `ffffffff8215ae35`, `8215bfae..8215bfba` | Getter returns K; native ELF loader stores auxv tag 51 (`AT_MINSIGSTKSZ`) and the getter's value. | Native `create_elf_tables` is inlined into `load_elf_binary`; exact mapping in `source-lines.txt`. |

On this XSAVE-enabled build:

```
F = M + 4 + 112
K = align_up(F + 814, 16) = align_up(M + 930, 16)
math = align_down(T - U - 4, 64)
frame_RSP = align_down(math - 440, 16) - 8
```

`T = B + L` on a fresh enabled `SA_ONSTACK` selection; on ordinary nested delivery that remains on the altstack, `T = S - 128`. Thus conservative native downward-consumption bounds are `U + 530` from a freshly selected top and `U + 658` from a nested interrupted RSP (63 and 15 are worst alignment losses; 8 is explicit). The alignment losses need not occur simultaneously: these are safe upper bounds, not exact measured frame lengths. Since `U <= M`, **K exceeds both**, including the native red-zone reservation on this particular IA32-enabled build. Do not generalize that last comparison to other kernel builds/ABIs without deriving their constants.

For the retained host query: `M=2440`, `F=2556`, `K=align_up(3370,16)=3376`; nested conservative consumption is at most `2440+658=3098` bytes. This inference comes from the allocation formula and maximum-size initialization, **not a sampled signal frame**.

The native copy path independently uses `fpstate.user_size` for the exported size fields (`821d4b7e..821d4b91`) and writes the four-byte `FP_XSTATE_MAGIC2` at `math+U` (`821d4bc2..821d4bd0`), consistent with the allocated suffix. The fixed native frame and this variable region are distinct; neither the 4160-byte C `struct fpstate` size nor a compacted supervisor/kernel XSAVE size is substituted for U.

## Why later permitted xstate cannot exceed M

1. `fpu__init_system_xstate` selects supported features, writes user `max_features` at `ffffffff8475e00f`, and a separate smaller `default_features` at `8475e02e`. The dynamic tile-data bit is omitted from default, not silently omitted from maximum.
2. `fpu__init_cpu_xstate` loads **user max_features** into XCR0 (`ffffffff81300941..81300951`). Initialization then reads CPUID leaf `0xD,0` EBX (`8475e130..8475e139`) and stores that user XSAVE extent as M (`8475e491..8475e495`). Component standard offsets/sizes come from CPUID and are ordered by offset; permission calculations use those same tables, not a compacted kernel-buffer size.
3. `fpstate_reset` sets active user size and group permission user size from **default_size** (`812fff82..812fffb0`). DWARF proves `max_size/default_size` offsets 0/4, `fpstate.user_size` offset 4, and permission `__user_state_size` offset 12. This build appends FPU data after the 3968-byte task struct: its pointer and permission offsets resolve to task-relative `0xf90` and `0xfa0`; an installed header comment's old `thread.fpu` spelling is not used as layout evidence.
4. `fpu_xstate_prctl` accepts a permission request only for feature 18, whose frozen request-table mask is `0x40000`; it rejects bits outside **user max_features** (`8130162f..81301669`). Under the sighand lock it computes the user extent for the permitted subset, verifies every configured thread-group altstack is at least `K - M + new_user_size`, otherwise returns `-ENOSPC`, and only then writes group permission/size (`81301743..81301835`). The xstate size routine chooses the furthest standard component end, not permission count times a guessed size.
5. `__xfd_enable_feature` checks the requested dynamic bit against group permission (`813012a5..813012ef`), allocates from the locked permitted sizes, and writes that user size into the replacement fpstate (`81301330`). It cannot create a larger *native* user feature set than the boot maximum through this path. KVM/guest-supervisor allocations are not being qualified as native user signal states.
6. The maximum/configuration and standard-offset/size tables lie inside the linked `__start_ro_after_init=ffffffff82c39200` to `__end_ro_after_init=ffffffff82cb1a40` range. The ordinary native permission path changes per-group/per-thread state, not these boot limits. This is not a claim against privileged kernel modification or a different boot/image.

Read-only query: supported, permitted, guest-permitted and XCR0 masks all `0x2e7`; CPUID standard extents end at PKRU offset 2432 + size 8 = 2440. Components 2/5/6/7/9 report their actual offsets/sizes in `query.stdout`; no AMX bit is present, so this host cannot grow to tile state through the inspected request path. No request/permission-changing operation was executed. The general inspected growth path also stays below the boot M on hardware where tile state is supported.

## Overflow and admission are different obligations

- Before copying fpstate, `get_sigframe` checks the final frame RSP is **strictly above B and at most B+L** when it determined that delivery is nested on, or entering, the altstack (`821d479d`, `821d484f..821d486a`). Failure returns all-ones without that fpstate copy. `x64_setup_rt_frame` then fails with `-EFAULT`; `signal_setup_done` routes failure to `force_sigsegv`. This is a signal-delivery failure, not a successful stack extension or a synchronous error returned to the interrupted guest. No overflow was experimentally induced here; terminal behavior is not assumed independent of signal dispositions.
- **Important boundary:** the initial nested-stack test uses S, but `SA_ONSTACK` selection tests the red-zone-adjusted `S-128` (`821d473a`, `821d4822`). If S is already within the bottom 128 bytes, that later test can select the altstack top again rather than recognize an exhausted nested allocation. The new frame can then pass the final range check. This compiled edge is another reason not to use kernel overflow handling as the stack-ownership proof. It was not executed as an experiment.
- `SS_AUTODISARM` changes stack recognition, and successful delivery clears the registered altstack (`signal_setup_done+0x6b`). An argument relying on uninterrupted registration must exclude it or separately prove correct ownership/rearming. Simply checking the saved frame's stack metadata is insufficient.
- `do_sigaltstack` checks the legacy 2048-byte floor and `sigaltstack_size_valid`. The latter can preserve permissive behavior absent strict sizing/dynamic permission; it is not an unconditional K check. Its dynamic branch is a boot-patched static key. Do not infer a safe allocation from syscall success, nor interpret its disk NOP as proof of the live branch.

**Concrete sufficient admission predicate, conditional on handler bounds:** pin this kernel/ABI contract; acquire K from trusted startup auxv (not guest-modified data); own a nonwrapping mapped writable interval `[B,B+L)`, disjoint from guest and other continuation storage; keep it registered without autodisarm, replacement, unmapping, or protection changes; require all admitted entries to obey the intended `SA_ONSTACK`/mask policy. Prove a maximum of N simultaneous kernel frames and independent maxima `H_i` for all handler, compiler, assembly, libc/Tool and other stack consumption between entries, including arbitrary stack adjustments and writes. A conservative sufficient size is:

```
L > 128 + N*K + sum(H_i) + other_reserved_bytes
```

Check the same bound at every reachable nesting prefix and ensure handlers never move RSP outside the owned interval. The explicit 128-byte spare keeps a nested interrupted S away from the stack-selection edge above. If the proposed four-source chain is actually proved to have `N=4`, its kernel-frame portion is **13,504 bytes**, not a proof of the entire stack; the inequality still needs handler/reservation bounds. An unbounded Tool/libc call graph, additional guest-handler nesting, abandoned live frames, or an unproved continuation transfer defeats this admission. Guard pages/canaries/high-water measurements neither bound arbitrary stack-pointer movement nor establish those missing H/N facts.

## Validation and limits

- Offline objdump/nm/addr2line/DWARF inspection only; no target execution or debugger attachment. GDB auto-load and debuginfod were disabled. Initial GDB member queries using old `thread.fpu` spelling failed and are retained; corrected appended-FPU offsets are separately recorded in `dwarf-offsets.txt`.
- Tiny external native query compiled with GCC 11.5.0; **1 core, 512 MiB, 30 seconds build; 10 seconds query**, enforced two-level cgroups. Build and both query invocations exited 0. The first runner summary retained only its final stdout line; the second explicitly retained complete stdout/stderr. This is not a frame-allocation test, backend test, or determinism claim. No permission changes, signal delivery, kernel instrumentation, network, or raw kernel-memory access occurred.
- Query source SHA256 `acbfdd8d0498e3a5304dd75128abdb6aa8fb4fbb1eb8ecc7eef057462085f4fb`; binary SHA256 `62f329aa248ee6677891e3578b9906b4547d7007381bdeb49ff6cf5c1a1fb298`. Exact commands, resource receipts and timestamps are external.
- Full vendor source provenance remains missing, but is no longer the obstacle to identifying this compiled kernel-frame allowance. **The remaining implementation blocker is a proved handler/continuation stack and nesting bound**, plus the independent original-frame lifetime/return ownership contract. No return alternative is approved by this inspection.
- Owned `liteinst-exec` stays clean at `8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`. No product source, task status, coverage labels, prior failures, or the frozen 340-row denominator changed.
