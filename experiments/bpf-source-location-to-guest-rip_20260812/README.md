# Mapping SCX scheduler C source locations to RIPs inside guest JITed BPF code

Task: `research-bpf-source-location-to-guest-rip`. Date: 2026-08-12. Read-only research;
no product change, nothing pushed.

## Question

Boot Linux in a VM; the guest kernel JIT-compiles an SCX (sched_ext) scheduler from BPF
bytecode. Can a scheduler C source location be mapped to an RIP inside the guest's actual
JITed code — by what mechanism, and what breaks it?

## Answer

**Yes.** The kernel keeps, per loaded BPF program, an array of native code addresses paired
with an array of `(BPF instruction index, file, line, column)` records, and exports both
through `bpf(BPF_OBJ_GET_INFO_BY_FD)`. This works for `struct_ops` programs, i.e. for real
SCX schedulers. It was demonstrated end to end inside a QEMU guest running a live
`scx_rlfifo`.

Three qualifications decide whether it is usable for schedule-search localization:

1. **Absolute addresses are never reproducible; base-relative offsets always are.** The
   kernel deliberately randomizes where a program starts inside its allocation. Across two
   identical KVM boots and two identical TCG+`icount` boots, every program landed at a
   different address, while the `(subprogram, byte offset within subprogram) → source
   location` table was byte-identical every time. Any consumer must normalize against the
   per-subprogram base address read at runtime.
2. **`bpf_jit_harden` must be 0.** At `bpf_jit_harden=2`, 49 of 416 records for the same
   scheduler lost their native address entirely (reported as `0x0`), concentrated in exactly
   the multi-subprogram hot paths (`dispatch`, `enqueue`, `select_cpu`, `init`).
3. **Resolution is nearest-preceding, not exact.** A record marks the *start* of the JITed
   range belonging to a source location. Measured over a whole scheduler: 416 records over
   4878 JITed bytes, min 2 / median 9 / mean 11.7 / p90 25 / max 106 bytes per record.

A fourth point is not about BPF at all and is the real open problem: under
`hermit run qemu-system-x86_64 -accel tcg`, a *guest* RIP is emulated CPU state inside QEMU,
not a host RIP. This work establishes that the guest-RIP→source table exists and is
reproducible. It does **not** establish how to sample a guest RIP under Hermit. See
"What this does not answer".

## Environment

| Thing | Value |
| --- | --- |
| Host | `devbig030.atn3.facebook.com`, `Linux 6.13.2-0_fbk15_hardened_0_g33ebba20e5e4` |
| Host BPF config | `CONFIG_BPF_JIT=y`, `CONFIG_BPF_JIT_ALWAYS_ON=y`, `CONFIG_SCHED_CLASS_EXT=y`, `CONFIG_DEBUG_INFO_BTF=y`, `CONFIG_RANDOMIZE_BASE` not set |
| Host sysctls at test time | `net.core.bpf_jit_enable=1`, `bpf_jit_harden=0`, `bpf_jit_kallsyms=1`, `kernel.kptr_restrict=0` |
| Guest kernel | `6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79` (`ignored/qemu-linux/bzImage`) |
| QEMU | `QEMU emulator version 10.1.0 (qemu-kvm-10.1.0-22.el9)`, `/usr/libexec/qemu-kvm` |
| clang | 22.1.3 (CentOS 22.1.3-1.el9) |
| bpftool | `/usr/sbin/bpftool` v7.8.0, libbpf v1.8 |
| Guest scheduler | `scx_rlfifo`, reported ops name `rlfifo_1.0.20_gcd701224_dirty_x86_64_unknown_linux_gnu`, built from `/home/newton/work/rrn_scx_playground/scx` |
| Host-compiled scheduler object | `/home/newton/work/scx/scheds/rust/scx_rlfifo/main.bpf.c` |

**Kernel source read for citations:** `/home/newton/work/dev-hermit/ignored/linux`, commit
`f6a6c280059c`, `Makefile` says `6.16.0-rc7`. That tree is neither the host (6.13.2) nor the
guest (6.17.13) kernel. Every source claim below was cross-checked against a live
measurement on the host or guest kernel; line numbers are from the 6.16-rc7 tree only.

## The mechanism, end to end

### 1. Compiler side — `.BTF.ext` carries `line_info` (lead confirmed)

`clang -g -target bpf` emits a `.BTF.ext` section holding `func_info`, `line_info` and
`core_relo` sub-sections; `line_info` records are 16 bytes:
`(insn_off, file_name_off, line_off, line_col)`, where the two `*_off` fields index the
`.BTF` string table and `line_col` packs `line << 10 | col`.

Compiling the real scheduler:

```
clang -g -O2 -target bpf -mcpu=v3 -D__TARGET_ARCH_x86 \
  -Iinc -I$SCX/scheds/include -I$SCX/scheds/include/arch/x86 \
  -I$SCX/scheds/include/bpf-compat -I$SCX/scheds/vmlinux \
  -I$SCX/scheds/vmlinux/arch/x86 -I$SCX/scheds/rust/scx_rlfifo \
  -c $SCX/scheds/rust/scx_rlfifo/main.bpf.c -o scx_rlfifo.bpf.o
```

`scripts/parse_btf_ext.py scx_rlfifo.bpf.o` reports
`.BTF.ext: hdr_len=32 func_info_len=300 line_info_len=9332 core_relo_len=1188`,
`line_info rec_size=16`, and — the part that matters — a `line_info` block for **every**
`struct_ops` section, not just `.text`:

```
  section '.text': 240 line_info records
  section 'struct_ops/rustland_select_cpu': 41 line_info records
  section 'struct_ops/rustland_enqueue': 89 line_info records
  section 'struct_ops/rustland_dispatch': 50 line_info records
  section 'struct_ops.s/rustland_init': 58 line_info records
  ... (10 struct_ops members in total)
```

In the ELF, `insn_off` is a **byte** offset (multiples of 8); libbpf converts to an
instruction index before `BPF_PROG_LOAD`. In the kernel-facing uapi it is an instruction
index.

### 2. Kernel side — the records survive the load, and get native addresses

The verifier copies and validates the records
(`kernel/bpf/verifier.c:17910` `check_btf_line`, storing `prog->aux->linfo` /
`prog->aux->nr_linfo` at `verifier.c:18025`), requiring `insn_off` strictly increasing and
each offset to land on a real instruction (`verifier.c:17979`).

After JIT, `bpf_prog_fill_jited_linfo()` (`kernel/bpf/core.c:218`) walks the records and
writes `prog->aux->jited_linfo[i] = prog->bpf_func + insn_to_jit_off[...]` — i.e. the
native address of the first JITed byte for each record. The x86 JIT calls it at
`arch/x86/net/bpf_jit_comp.c:3744`.

`BPF_OBJ_GET_INFO_BY_FD` then exports, in `struct bpf_prog_info`
(`/usr/include/linux/bpf.h:6628-6641`):

| Field | Meaning |
| --- | --- |
| `line_info` / `nr_line_info` | `struct bpf_line_info[]` — BPF insn index → BTF string offsets |
| `jited_line_info` / `nr_jited_line_info` | `__u64[]` — native address for the *i*-th record |
| `jited_ksyms` / `nr_jited_ksyms` | per-subprogram native base addresses |
| `jited_func_lens` / `nr_jited_func_lens` | per-subprogram native byte lengths |
| `btf_id` | the BTF whose string table resolves `file_name_off` / `line_off` |

`bpftool prog dump jited ... linum` reads exactly these fields
(`tools/bpf/bpftool/prog.c:118-153`), so lead 2 is confirmed in both directions: the tool
adds nothing the uapi does not already provide.

### 3. It works on a real SCX `struct_ops` program (lead confirmed)

Loaded on the host **without registering the scheduler** — libbpf only registers in
`bpf_map__attach_struct_ops` (`tools/lib/bpf/libbpf.c:13089-13110`), so
`bpftool prog loadall` creates and JITs the programs but never activates them.
`/sys/kernel/sched_ext/state` read `disabled` before and after, confirming this.

```
sudo /usr/sbin/bpftool prog loadall scx_rlfifo.bpf.o /sys/fs/bpf/rip_research_a
sudo /usr/sbin/bpftool prog dump jited pinned /sys/fs/bpf/rip_research_a/rustland_enqueue linum
```

produces (`artifacts/jited_linum_enqueue.txt`, first lines):

```
bpf_prog_0a500fe12172ddd4_rustland_enqueue:
; void BPF_STRUCT_OPS(rustland_enqueue, struct task_struct *p, u64 enq_flags) [file:.../main.bpf.c line_num:730 line_col:6]
   0:	nopl	(%rax,%rax)
   ...
; s32 prev_cpu = scx_bpf_task_cpu(p), cpu; [file:.../main.bpf.c line_num:732 line_col:17]
  22:	movq	%r13, %rdi
  25:	callq	0xffffffff81d80f00
```

and `/proc/kallsyms` carries `ffffffffa0084ba8 t bpf_prog_0a500fe12172ddd4_rustland_enqueue	[bpf]`
(lead 3 confirmed) — the program lives in the module/vmalloc address range.

`scripts/bpf_riplines.c` is a ~200-line static binary that uses only `bpf(2)` (no libbpf, no
bpftool) and prints the same table directly (`artifacts/run_a_57890.txt`):

```
=== prog id=57890 name=rustland_enqueu type=27 tag=0a500fe12172ddd4 btf_id=45517
    jited_prog_len=845 xlated_prog_len=1448 nr_jited_ksyms=2 nr_line_info=86
    nr_jited_line_info=86 line_info_rec_size=16 jited_line_info_rec_size=8
    subprog[0] ksym=0xffffffffa0084ba8 len=447
    subprog[1] ksym=0xffffffffa0084d9c len=398
    jited_addr         bpf_insn   source
    0xffffffffa0084ba8 0          .../scx_rlfifo/main.bpf.c:730:6
    0xffffffffa0084bca 2          .../scx_rlfifo/main.bpf.c:732:17
    0xffffffffa0084bdd 8          .../scx/scheds/include/scx/compat.bpf.h:180:6
```

### 4. It works inside the VM, against a live scheduler

`scripts/guest-init.sh` (an initramfs `init`) boots, starts `scx_rlfifo`, waits for
`/sys/kernel/sched_ext/state == enabled`, then runs the static dumper and writes the table
to the serial console. Guest output confirms the scheduler was actually the active one:

```
GUEST-KERNEL: 6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79
PHASE A sched_ext state: enabled
PHASE A active ops: rlfifo_1.0.20_gcd701224_dirty_x86_64_unknown_linux_gnu
PHASE A dump bytes: 51563
```

10 `struct_ops` programs, 416 `line_info` records, 0 records without a native address
(`artifacts/guest/boot1-console.txt`, `artifacts/guest/mapA.txt`).

Resolving an address with `scripts/resolve_rip.py`:

```
$ python3 resolve_rip.py mapA.txt 0xffffffffa0000d90
0xffffffffa0000d90  rustland_enqueu subprog 0 +0x14  bpf_insn 0
  /home/newton/work/rrn_scx_playground/scx/scheds/rust/scx_rlfifo/main.bpf.c:730:6
  (record starts at +0x0, RIP is 20 byte(s) into it)
```

and line 730 of that file is, verbatim,
`void BPF_STRUCT_OPS(rustland_enqueue, struct task_struct *p, u64 enq_flags)`.

## Determinism: what must be pinned

### Randomized by design — the absolute address (no knob disables it)

Both JIT image allocators insert a random gap before the code:

```c
/* kernel/bpf/core.c, bpf_jit_binary_alloc() and bpf_jit_binary_pack_alloc() */
hole  = min_t(unsigned int, size - (proglen + sizeof(*hdr)), PAGE_SIZE - sizeof(*hdr));
start = get_random_u32_below(hole) & ~(alignment - 1);
/* Leave a random number of instructions before BPF code. */
*image_ptr = &hdr->image[start];
```

Measured, two identical KVM boots (`mapA.txt` vs `map2A.txt`): every one of the 10 programs
moved.

```
rustland_dispat  A=['0xffffffffa0001154','0xffffffffa000135c']  B=['0xffffffffa0001324','0xffffffffa0001520'] -> rel-identical
rustland_enqueu  A=['0xffffffffa0000d7c','0xffffffffa0000654','0xffffffffa00010d4']
                 B=['0xffffffffa0000d68','0xffffffffa00010cc','0xffffffffa000128c'] -> rel-identical
... (10/10 rel-identical, 0/10 absolute-identical)
```

Under deterministic-VM settings (`-accel tcg,thread=single -smp 1 -icount shift=0,sleep=off`)
the addresses moved by smaller amounts but still moved: 9 of 10 programs differed,
1 (`rustland_init_task`) happened to land identically (`map_tcg1.txt` vs `map_tcg2.txt`).
So even the deterministic QEMU configuration does **not** give reproducible absolute
addresses. I did not isolate the entropy source; `-cpu max` exposes `RDRAND`/`RDSEED` to the
guest, which QEMU services from host randomness, and that is a plausible but unverified
cause.

**Consequence:** absolute addresses are per-load state. The mapping must be captured in the
same boot (in fact the same program load) as the execution being localized, or the consumer
must read `jited_ksyms` / `/proc/kallsyms` at that moment and work in base-relative terms.

### Exactly reproducible — the base-relative table

`(subprogram index, byte offset within subprogram) → (BPF insn index, file:line:col)` was
byte-identical in every comparison performed:

| Comparison | Result |
| --- | --- |
| Two `bpftool prog loadall` of the same object, same host boot | identical (86/86 records) |
| Two identical KVM guest boots | identical (10/10 programs, 416 records) |
| Two identical TCG+`icount` guest boots | identical (10/10 programs) |

Note the JITed *bytes* are not identical between two loads even on one host: `call rel32`
displacements and `movabs` map-pointer immediates differ (`artifacts/jit_a.txt` vs
`artifacts/jit_b.txt`, 112 differing lines, all of them `e8 ..` displacements or map-address
immediates). Instruction *lengths* are unchanged, which is why the offset table survives.

### Knobs that must be pinned

| Knob | Required value | Why |
| --- | --- | --- |
| `net.core.bpf_jit_harden` | `0` (or `1` with a `CAP_BPF` loader) | see below — records lose their address |
| `net.core.bpf_jit_enable` | `1` | with `0` the program is interpreted; `bpf_prog_jit_attempt_done` frees `jited_linfo` (`kernel/bpf/core.c:184-187`) and `nr_jited_line_info` becomes 0 |
| `kernel.kptr_restrict` | `0`, or the reader holds `CAP_SYSLOG` | `jited_prog_insns` (`kernel/bpf/syscall.c:4816`), `jited_ksyms` (`:4851`), `jited_func_lens` (`:4882`) and `jited_line_info` (`:4940`) are each gated on `bpf_dump_raw_ok(file->f_cred)`, which is `kallsyms_show_value(cred)` (`include/linux/filter.h:1147`, `kernel/ksyms_common.c:28`). Note the *non*-JITed `line_info` is exported ungated. Note also it is the credentials of the **fd holder**, not the caller |
| build flags | `-g`, `.BTF.ext` not stripped | no `line_info` otherwise |
| `net.core.bpf_jit_kallsyms` | `1` if you want `/proc/kallsyms` names; not needed for the uapi path | |

`bpf_jit_harden` gate: `bpf_jit_blinding_enabled()` (`include/linux/filter.h`) returns true
when `bpf_jit_harden == 2`, or `== 1` and the loader lacks `CAP_BPF`.

### The `bpf_jit_harden` measurement

Phase B of the same guest boot set `bpf_jit_harden=2` and reloaded the same scheduler
(`artifacts/guest/mapB.txt`):

| program | jited bytes A→B | line_info records | records with address `0x0` under harden=2 |
| --- | --- | --- | --- |
| `rustland_dispatch` | 1111 → 2372 | 94 | 20 |
| `rustland_enqueue` | 1341 → 2818 | 131 | 20 |
| `rustland_init` | 959 → 2033 | 75 | 5 |
| `rustland_select_cpu` | 654 → 1329 | 50 | 4 |
| the 6 single-subprogram members | ~2× | 66 | 0 |
| **total** | | **416** | **49** |

The coordinator hypothesis that blinding desynchronizes `line_info` because it rewrites
instructions is **partly wrong and worth correcting**: `bpf_jit_blind_constants()`
(`kernel/bpf/core.c:1471`) patches through `bpf_patch_insn_single()`, which *does* fix the
records up — `bpf_adj_linfo(prog_adj, off, insn_delta)` at `kernel/bpf/core.c:535`. Measured
proof: for `rustland_exit` the record `insn_off` list moved from
`[0, 1, 23, 24, 25, 26, 29]` to `[0, 1, 58, 61, 62, 63, 66]` as the program grew from 30 to
67 instructions, while the source locations stayed the same. So the failure mode is not
garbage addresses; it is **lost coverage in multi-subprogram programs**, where 49/416
records end up with no native address at all. Either way the conclusion for us is the same:
`bpf_jit_harden` must be 0.

Two things I did **not** verify: whether the *surviving* records under blinding are
semantically correct (I checked only that they are non-zero and inside a subprogram range),
and the precise mechanism by which the 49 records are skipped (the plausible reading is that
`bpf_adj_linfo` shifts the whole shared `aux->linfo` array while each subprogram is blinded
and JITed separately, so `bpf_prog_fill_jited_linfo`'s per-subprogram window
`linfo[i].insn_off < insn_start + prog->len` no longer lines up — unconfirmed).

## Exactness: nearest-preceding within a subprogram

The lookup, which is what `bpftool` and `perf` do and what `scripts/resolve_rip.py`
implements:

1. find the subprogram with `ksym <= RIP < ksym + jited_func_len`;
2. inside it, take the last `jited_line_info` entry `<= RIP`;
3. report that record's `file:line:column`.

Properties measured on the live guest scheduler (416 records, 4878 JITed bytes):

- **Unique, but coarse.** Every RIP inside a subprogram resolves to exactly one record, so
  to exactly one `file:line:column`. But a record covers a *range*: min 2, median 9, mean
  11.7, p90 25, max 106 bytes. 18 records cover more than 32 bytes. So "this RIP is in the
  code generated for line N" is exact; "this RIP is the instruction for line N" is not.
- **Monotonic within a subprogram, not across them.** 0 of 15 subprogram sequences were
  non-monotonic. But subprograms of one program are not laid out in index order: in boot 1,
  `rustland_enqueue`'s three subprograms were at `0xa0000d7c`, `0xa0000654`, `0xa00010d4` —
  subprogram 1 *below* subprogram 0. A naive global binary search over `jited_line_info`
  gives wrong answers; partition by subprogram first.
- **Columns, and repeated lines.** Records carry a column and the same line appears in
  several records (`main.bpf.c:741:36` then `741:3`), and inlined helpers interleave
  (`compat.bpf.h:185:19` inside `main.bpf.c` code). 416 records covered 187 distinct
  `file:line` pairs across 3 files.
- **Not everything is covered.** A RIP inside the `struct_ops` trampoline
  (`bpf_struct_ops_prepare_trampoline` → `arch_prepare_bpf_trampoline`,
  `kernel/bpf/bpf_struct_ops.c:587/619`) is not inside any program image and resolves to
  nothing. A RIP inside a kfunc the scheduler calls (`scx_bpf_dsq_insert` and friends) is
  ordinary kernel text, resolved by ordinary kernel symbols, not by this table.

## Extraction paths for the VM setting

### (a) Run a tool in the guest and export the table — demonstrated, and cheapest

What it needs: a dumper in the guest image, `kptr_restrict=0` or `CAP_SYSLOG`, and a channel
out. `bpftool` works but on this host is dynamically linked against an fbcode loader;
`scripts/bpf_riplines.c` built with `gcc -static` is ~880 KB, has no dependency beyond
`bpf(2)`, and was dropped into the existing `initramfs-scx` tree unchanged.

Cost, and it is the real one: the dumper executes guest instructions, so under Hermit it
perturbs the very execution being localized. It also must run in the same boot as the run
being studied, because the addresses are per-load. Practical shapes: dump immediately after
the scheduler attaches and before the interesting window; or dump at the end of the run and
rely on the program not being reloaded in between (verifiable via `enable_seq` /
program id).

### (b) Read guest kernel memory from the hypervisor side — feasible with machinery already here, not run

`demos/lib/drgn_hermit.py` already reads a paused Hermit/QEMU guest's physical memory
through read-only `pread()` on `/proc/<qemu-pid>/mem`, with QEMU in ptrace-stop and Hermit's
tracer stopped, executing zero guest instructions (`demos/07-drgn-kernel.md`). Walking
`bpf_prog` → `aux->linfo` / `aux->jited_linfo` / `aux->nr_linfo` from there is the
zero-perturbation version of path (a), and it works on a snapshot after the fact.

What it additionally needs, none of which I verified: guest type information for
`struct bpf_prog_aux` (the demo extracts a `vmlinux` from the `bzImage`; whether that image
carries DWARF or only `.BTF`, and whether the drgn in use can consume the latter, I did not
check — `import drgn` fails in the default python on this host), a guest virtual→physical
walk, and a way to enumerate loaded programs (the `prog_idr`, or the guest's `/proc/kallsyms`
`bpf_prog_<tag>_<name>` entries read from a paused guest).

### (c) `/sys/kernel/btf` — this lead is wrong as stated

`/sys/kernel/btf` holds `vmlinux` plus one entry per loaded kernel module (39 entries on this
host); there is no per-program entry. The scheduler's file and line **strings** live in the
program's own BTF object, reachable only as `BPF_BTF_GET_FD_BY_ID(prog_info.btf_id)` followed
by `BPF_OBJ_GET_INFO_BY_FD` on that fd — which is what `bpf_riplines.c` does. `/sys/kernel/btf/vmlinux`
is still relevant, but as the *kernel* type source that path (b) would need.

### (d) perf — plausible, source-verified only

`PERF_RECORD_KSYMBOL` (type 17) with `PERF_RECORD_KSYMBOL_TYPE_BPF` and
`PERF_RECORD_BPF_EVENT` (type 18) exist in `include/uapi/linux/perf_event.h:1178/1197` and
are emitted for BPF programs from `kernel/events/core.c:9913/9929`; perf then fetches
`bpf_prog_info` for the line table. I did not run perf, in the guest or on the host.

## What this does not answer

**Where the guest RIP comes from.** Under `hermit run qemu-system-x86_64 -accel tcg` — the
configuration the QEMU/Linux demos use — the guest does not execute natively. A guest RIP is
a value inside QEMU's emulated CPU state; the host RIP Hermit can see belongs to TCG
translated code and has no relation to it. So the consumer of this mapping must obtain guest
RIPs from QEMU (gdbstub, a TCG plugin, monitor register reads, `dump-guest-memory`) or from
guest-side sampling. This report proves the table exists, is exported, and is reproducible in
base-relative form. Producing a stream of guest RIPs under Hermit is a separate, unanswered
question, and it is the one that gates schedule-search localization.

Also not verified:

- perf's BPF symbolization end to end (source-read only).
- Path (b) executed against a real paused guest.
- Whether the records surviving `bpf_jit_harden=2` are semantically correct.
- The entropy source behind address variation under TCG+`icount`.
- Behaviour on kernels other than host 6.13.2 and guest 6.17.13; the cited line numbers come
  from a 6.16-rc7 tree.
- Anything about arm64 or any non-x86 JIT.

## Reproducing

```bash
E=experiments/bpf-source-location-to-guest-rip_20260812

# host: compile a real SCX scheduler with line info and inspect the ELF
mkdir -p /tmp/inc && ln -sfn <linux>/tools/lib/bpf /tmp/inc/bpf
python3 <linux>/scripts/bpf_doc.py --header --filename <linux>/include/uapi/linux/bpf.h \
  > /tmp/inc/bpf_helper_defs.h
SCX=/home/newton/work/scx
clang -g -O2 -target bpf -mcpu=v3 -D__TARGET_ARCH_x86 -I/tmp/inc \
  -I$SCX/scheds/include -I$SCX/scheds/include/arch/x86 -I$SCX/scheds/include/bpf-compat \
  -I$SCX/scheds/vmlinux -I$SCX/scheds/vmlinux/arch/x86 -I$SCX/scheds/rust/scx_rlfifo \
  -c $SCX/scheds/rust/scx_rlfifo/main.bpf.c -o /tmp/scx_rlfifo.bpf.o
python3 $E/scripts/parse_btf_ext.py /tmp/scx_rlfifo.bpf.o

# host: load WITHOUT registering the scheduler, then dump
cat /sys/kernel/sched_ext/state                     # must read: disabled
sudo /usr/sbin/bpftool prog loadall /tmp/scx_rlfifo.bpf.o /sys/fs/bpf/rip_research_a
cat /sys/kernel/sched_ext/state                     # still: disabled
sudo /usr/sbin/bpftool prog dump jited pinned /sys/fs/bpf/rip_research_a/rustland_enqueue linum
gcc -O2 -Wall -static -o /tmp/bpf_riplines $E/scripts/bpf_riplines.c
sudo /tmp/bpf_riplines --all-struct-ops
sudo rm -rf /sys/fs/bpf/rip_research_a              # clean up

# guest: build an initramfs carrying the static dumper, boot, read the console
cp -a ignored/qemu-linux/initramfs-scx /tmp/root
cp /tmp/bpf_riplines /tmp/root/bin/
cp $E/scripts/guest-init.sh /tmp/root/init && chmod +x /tmp/root/init
( cd /tmp/root && find . -print0 | cpio --null -o -H newc ) | gzip -1 > /tmp/initramfs.cpio.gz
/usr/libexec/qemu-kvm -accel kvm -smp 4 -m 2048M -cpu host \
  -kernel ignored/qemu-linux/bzImage -initrd /tmp/initramfs.cpio.gz \
  -display none -serial stdio -monitor none -no-reboot \
  -append 'console=ttyS0 panic=-1 rdinit=/init' > /tmp/boot.log

# analyse
sed -n '/--- PHASE A MAP BEGIN ---/,/--- PHASE A MAP END ---/p' /tmp/boot.log | sed 's/\r$//' > /tmp/mapA.txt
python3 $E/scripts/resolve_rip.py /tmp/mapA.txt --coverage
python3 $E/scripts/resolve_rip.py /tmp/mapA.txt 0x<some-guest-rip>
```

The deterministic-VM variant replaces the accelerator flags with
`-accel tcg,thread=single -smp 1 -icount shift=0,sleep=off -cpu max`.

## Files

```
scripts/parse_btf_ext.py   parse .BTF/.BTF.ext out of a BPF ELF, print line_info records
scripts/bpf_riplines.c     static, libbpf-free dumper: bpf(2) -> jited addr / insn / src table
scripts/resolve_rip.py     RIP -> source lookup, and --coverage granularity report
scripts/guest-init.sh      initramfs init used for the in-guest runs (phase A/B)
artifacts/jited_linum_enqueue.txt   bpftool dump jited ... linum, host, scx_rlfifo enqueue
artifacts/run_a_57890.txt, run_b_57981.txt   two host loads of the same object
artifacts/jit_a.txt, jit_b.txt               their JITed opcode dumps
artifacts/guest/boot1-console.txt, boot2-console.txt   two KVM boots (phase A harden=0, phase B harden=2)
artifacts/guest/tcg1-console.txt, tcg2-console.txt     two TCG+icount boots
artifacts/guest/mapA.txt, mapB.txt, map2A.txt, map_tcg1.txt, map_tcg2.txt   extracted tables
```

Binaries produced during the run (`bpf_riplines`, `scx_rlfifo.bpf.o`, the staged initramfs
under `scratch/bpf-rip-guest/`) are deliberately not committed; the commands above rebuild
them.
