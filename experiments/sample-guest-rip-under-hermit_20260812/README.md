# Sampling a guest RIP under Hermit

Task: `research-sample-guest-rip-under-hermit`. Date: 2026-08-12. Read-only research; no
product change, nothing pushed to Hermit.

Follow-on from `experiments/bpf-source-location-to-guest-rip_20260812/` (dev-hermit main
`861d9d1028c86fb4cd7d97f7f23a47b223cc8b74`), which proved the JITed-address → scheduler
source table exists and is reproducible base-relative, and left one gap: under
`hermit run qemu -accel tcg` a guest RIP is emulated CPU state inside QEMU, not a host RIP,
so the table had nothing to resolve.

## Answer

**Deterministic guest-RIP sampling under Hermit is achievable today, with no QEMU
modification, no debug symbols, no drgn, and no TCG plugin.** QEMU's own
`-d exec -dfilter <range> -D <file>` emits the guest PC of every translation-block entry,
filtered to the BPF/module address range. Under Hermit the resulting guest-PC sequence is
reproducible run to run; under bare QEMU it is not.

Strongest measurement: two `hermit run` invocations of the same SCX guest, with
`-d exec,nochain`, produced **byte-identical 3,149,844-byte trace files**
(md5 `d97fee12c16149fc5e8982e13b7e6226` both), 44,364 TB entries each.

Second result: the last link of the localization chain now closes. Two Hermit runs that
differ only in `--rng-seed` diverge at TB entry #1835, and that divergence resolves to two
named scheduler source statements — `main.bpf.c:664:18` versus `main.bpf.c:662:24`, inside
`queue_task_to_userspace`.

Third result, and a correction to my own prior report: I wrote that JITed base addresses are
never reproducible. That holds for **bare** QEMU and was re-measured here. It does **not**
hold under Hermit: the bases were identical across every Hermit run
(`0xffffffffa0000654` / `0xffffffffa0000b50` / `0xffffffffa0001138`), and the in-guest map
dump had the same md5 in all four Hermit configurations tested. Hermit determinizes the
guest kernel's `get_random_u32_below` draw. Base-relative normalization is still required
when comparing *across* configurations, and the tooling here does it, but within one Hermit
configuration absolute addresses are stable.

## Environment

| Thing | Value |
| --- | --- |
| Host | `devbig030.atn3.facebook.com`, `Linux 6.13.2-0_fbk15_hardened_0_g33ebba20e5e4` |
| QEMU | `QEMU emulator version 10.1.0 (qemu-kvm-10.1.0-22.el9)`, `/usr/libexec/qemu-kvm` |
| Guest kernel | `6.17.13-0_fbk0_crackerjackhost_0_g2b4321c50d79` (`ignored/qemu-linux/bzImage`) |
| Guest scheduler | `scx_rlfifo`, active ops `rlfifo_1.0.20_gcd701224_dirty_x86_64_unknown_linux_gnu` |
| Hermit | `hermit/target/release/hermit`, built 2026-08-12 13:36, hermit main `1f4e430c1` |
| Hermit profile | `--log error run --no-sequentialize-threads --preemption-timeout disabled --no-virtualize-cpuid` |
| QEMU profile | `-accel tcg,thread=single -smp 1 -icount shift=0,sleep=off -m 2048M -cpu max -rtc base=utc,clock=vm` |
| gdb | GNU gdb 9.1, `/usr/local/bin/gdb` |

The guest is a lean initramfs (`scripts/guest-init-lean.sh`) that attaches `scx_rlfifo`,
publishes its own JITed-address → source map with the `bpf_riplines` dumper from the prior
experiment, dumps module-range `/proc/kallsyms`, runs four concurrent CPU-bound tasks, and
powers off.

## Mechanism survey

| Mechanism | Gives | Deterministic? | Verdict here |
| --- | --- | --- | --- |
| QEMU `-d exec` + `-dfilter` + `-D` | guest PC of every TB entry in range | yes, under Hermit (measured) | **works; the recommended path** |
| QEMU `-d exec,nochain` | every TB entry, no chaining elisions | yes, under Hermit (measured, byte-identical files) | **works; complete but ~3x slower** |
| QEMU gdbstub, breakpoint at a guest address | exact RIP + all registers at a guest-chosen point | yes (stop is a guest address); verified twice under Hermit | **works; the point-sample path** |
| QEMU gdbstub, `interrupt` | exact RIP wherever the guest is | **no** — stop point is host timing | usable for exploration only |
| Read the `-D` log tail while QEMU is stopped | last TB entered *within the filter range* | yes | works, but **stale w.r.t. "now"** (measured) |
| TCG plugin (`qemu_plugin_register_vcpu_insn_exec_cb`) | per-*instruction* guest PC | would be | **unavailable**: this build reports `plugin interface not enabled in this build` |
| `/proc/<qemu-pid>/mem` → `CPUX86State.eip` | exact RIP with zero guest instructions | yes | **blocked**: no QEMU debuginfo (see below) |
| QEMU monitor `info registers` | exact RIP | stop point is host timing | not run |
| Guest-side sampling (perf) | guest PC | PMU not emulated under TCG | not pursued |

### Correcting my own earlier framing of the hypervisor-side path

The prior report said that path was blocked because drgn was not importable. That was the
wrong diagnosis. `demos/lib/drgn_hermit.py` reads **guest kernel memory** through
`/proc/<qemu-pid>/mem`, and drgn types it with the guest `vmlinux`. A guest RIP is not guest
kernel memory — it is QEMU's own `CPUX86State.eip`, a C struct inside the emulator. Reading
it needs QEMU's DWARF, not the guest kernel's. On this host:

```
$ nm /usr/libexec/qemu-kvm | wc -l          # 1  (binary is stripped)
$ ls /usr/lib/debug/usr/libexec/            # No such file or directory
$ with-proxy dnf --quiet list --available qemu-kvm-debuginfo qemu-kvm-core-debuginfo
Error: No matching Packages to list
```

So that path is blocked by the absence of QEMU debug symbols, and installing drgn would not
have unblocked it. It remains the right design for zero-perturbation sampling *if* a
debuginfo-carrying QEMU build is available; it was not attempted here.

## The recommended mechanism, concretely

```
qemu-kvm ... -d exec -dfilter 0xffffffffa0000000..0xffffffffa1000000 -D trace.txt
```

`-dfilter` restricts logging to guest addresses in the x86-64 module/vmalloc range, which is
where the BPF JIT places SCX programs. Output lines look like:

```
Trace 0: 0x7f4e7818b200 [00000000/ffffffffa000240c/18c0c2b0/ff022000]
```

The second bracketed field is the **guest PC**. The first is a host translation-block
pointer and must be discarded before comparing runs.

`scripts/normalize_trace.py` turns a trace plus the console of the same boot into
base-relative symbolic tokens, preferring the in-guest map (`prog#subprog+0xoff`, which also
carries the source location) and falling back to guest kallsyms (`symbol+0xoff`). That is the
form the prior report's determinism finding requires for any cross-configuration comparison.

Volume and cost, same guest, same workload:

| | wall time | trace size | TB entries logged |
| --- | --- | --- | --- |
| `-d exec`, bare | 27 s | 6.6 MB | 24,166 |
| `-d exec`, Hermit | 40–50 s | 6.6 MB | 24,162 |
| `-d exec,nochain`, bare | 78 s | 3.2 MB | 44,397 |
| `-d exec,nochain`, Hermit | 86–88 s | 3.1 MB | 44,364 |

`nochain` produces a *smaller* file with *more* samples: without it QEMU also prints a
`Linking TBs ...` line per chain operation (95,646 of them in one run), and chained TB
entries are not logged at all — so the default `-d exec` trace silently omits roughly 46% of
TB executions (24,162 logged vs 44,364 actual). **Use `nochain` whenever "first divergence"
has to mean something.**

## Determinism results

Every row is the same guest, same workload; `md5(PC seq)` is over the extracted guest-PC
sequence only. Full table with file md5s in `artifacts/trace-manifest.txt`.

| run | configuration | TB entries | md5(PC seq) |
| --- | --- | --- | --- |
| noseed1 | bare TCG+icount | 24,293 | `065f395d013a…` |
| noseed2 | bare TCG+icount | 24,166 | `b435f4d3c137…` |
| seed1a | bare + `-seed 1` | 24,162 | `b792b6fbd4a2…` |
| seed1b | bare + `-seed 1` | 23,913 | `4f060ccdf037…` |
| hermit1 | Hermit | 24,162 | `7ba1e3c07742…` |
| hermit2 | Hermit | 24,162 | `7ba1e3c07742…` |
| rng1a | Hermit `--rng-seed 1` | 24,166 | `445ccebe09b6…` |
| rng1b | Hermit `--rng-seed 1` | 24,166 | `445ccebe09b6…` |
| rng5 | Hermit `--rng-seed 5` | 24,316 | `887e19b785d8…` |
| nc-rng1a | Hermit `--rng-seed 1`, nochain | 44,364 | `9cc1e94ae384…` |
| nc-rng1b | Hermit `--rng-seed 1`, nochain | 44,364 | `9cc1e94ae384…` |
| nc-rng5 | Hermit `--rng-seed 5`, nochain | 44,514 | `3f7b39268285…` |

Reading:

- **Bare QEMU is not deterministic**, with or without `-seed 1`. `-seed` seeds QEMU's own
  PRNG; it did not stabilize the guest. Every bare pair differs.
- **Under Hermit every same-configuration pair is identical**, three pairs out of three.
- With `nochain`, the two `--rng-seed 1` runs produced **byte-identical trace files**
  including QEMU's host TB pointers (md5 `d97fee12…` both), so even QEMU's internal
  allocation is reproduced. With chaining on, the `Linking TBs` lines vary by ~3 lines
  between otherwise identical runs while the guest-PC subsequence stays identical — so
  compare the extracted PC sequence, not the raw file, unless using `nochain`.
- Everything else about the Hermit runs matched too: guest kallsyms, guest console, and the
  in-guest map dump. The map md5 is a function of the Hermit configuration alone — stable
  across tracing mode and across having a gdb session attached, and different between rng
  seeds, which is exactly why cross-configuration comparison must be base-relative:

  | map md5 | runs |
  | --- | --- |
  | `a59347cb9a2584bada7753cb8e1cb170` | hermit1, hermit2, gdbh1, gdbh2 (two of them with gdb attached) |
  | `14e25d622365d3c3d83c61d9e09d362b` | rng1a, rng1b, nc-rng1a (one of them a `nochain` run) |
  | `826a3b00bdae4c70d30a9b61d2efa95a` | rng5, nc-rng5 |

  The only differing bytes in the whole capture of a same-configuration pair were two Hermit
  stderr lines carrying host wall-clock timestamps.
- These runs had CPUID interception **off** — Hermit logged `ARCH_SET_CPUID returned ENODEV`
  on this host — and determinism held anyway.

## End-to-end: localizing a schedule difference to a scheduler source line

Two Hermit runs differing only in `--rng-seed`, complete traces (`nochain`), compared in
base-relative form:

```
$ python3 scripts/normalize_trace.py --diff \
    trace-nc-rng1a.txt console-nc-rng1a.txt trace-nc-rng5.txt console-nc-rng5.txt
A: 44364 TB entries, 218 distinct sites
B: 44514 TB entries, 219 distinct sites
FIRST DIVERGENCE at TB entry #1835
  common #1832: rustland_enqueu#1+0x63
  common #1833: rustland_enqueu#1+0x7d
  common #1834: rustland_enqueu#1+0xdb
  A      #1835: rustland_enqueu#1+0x138
  B      #1835: rustland_enqueu#1+0x11b

  A source: .../scx_rlfifo/main.bpf.c:664:18
  B source: .../scx_rlfifo/main.bpf.c:662:24
```

`rustland_enqueue` subprogram 1 is `queue_task_to_userspace` (guest kallsyms
`bpf_prog_2045f2d99196eaab_queue_task_to_userspace`). The three source lines involved:

```
655:    task->pid = p->pid;                      <- last common TB
662:    task->weight = p->scx.weight;            <- run B goes here
664:    task->enq_cnt = ++tctx->enq_cnt;         <- run A goes here
```

The chained (`-d exec` without `nochain`) traces of the same two configurations independently
pick the same divergence site, at their own index #1327.

**Be precise about what this shows.** It identifies where the two executions' TB-entry
sequences first differ, and resolves both endpoints to named source statements. Because the
three statements are straight-line code, this is a difference in how the two runs' TBs were
carved and entered, not demonstrably a different branch decision. Distinguishing "different
branch" from "different TB partition of the same code" needs instruction granularity, which
`-d exec` does not provide and which on this host would require a QEMU built with
`--enable-plugins`. That is the single most valuable follow-up.

## Point sampling through the gdbstub

`scripts/gdbsample.sh` boots the guest with `-gdb unix:<sock>,server=on,wait=off`, waits for
the guest to publish its own map on the console, takes `rustland_enqueue` subprogram 0's base
out of that map, sets a breakpoint there, continues, and reads RIP:

```
breakpoint target (rustland_enqueue subprog 0 base): 0xffffffffa0000b50
Breakpoint 1, 0xffffffffa0000b50 in ?? ()
rip            0xffffffffa0000b50  0xffffffffa0000b50
GDB-RIP 0xffffffffa0000b50
```

Determinism: two independent Hermit runs of this script produced the same breakpoint address
and the same RIP, and the in-guest map md5 was identical to the Hermit runs with **no** gdb
session attached (`a59347cb9a2584bada7753cb8e1cb170`). So opening a gdbstub session did not
perturb the guest's deterministic execution in any way this measurement can see.

The stop condition is a guest address, so the sample point is a property of guest execution,
not of host timing — which is what the determinism requirement demands. The chicken-and-egg
(you need the map to know where to break) is resolved within one boot by reading the map off
the console first, as the script does.

`interrupt` instead of a breakpoint also returns a correct RIP but at a host-timing-chosen
moment; in the one run performed it stopped in guest userspace at `0x4d8726`. Useful for
exploration, useless for reproducible localization.

## The log-tail-at-stop idea, and why it is weaker than it looks

If QEMU flushes its `-D` log promptly, then whenever Hermit has QEMU stopped one could simply
read the tail of the log and get the guest PC — no QEMU internals, no debuginfo, zero guest
instructions. Tested against gdbstub ground truth:

- Breakpoint stop, chained trace: RIP `0xffffffffa0000b68`, log tail
  `ffffffffa0001fae` — a different function (the `bpf__sched_ext_ops_enqueue` trampoline).
- Breakpoint stop, `nochain`: RIP `0xffffffffa0000b64`, log tail `ffffffffa0001fb6`.
- `interrupt` stop: RIP `0x4d8726` (guest userspace), log tail `ffffffffa00021c9`.

Two separate effects. First, a breakpoint halts *before* the target TB is entered, so its
`Trace` line does not exist yet and the tail is legitimately the preceding TB — the tail
means "last TB entered", which is a well-defined and useful semantics. Second, and more
limiting, `-dfilter` means only in-range TBs are logged, so whenever the guest is executing
outside the filter the tail is arbitrarily old — as the userspace-RIP sample shows. The log
does appear to be flushed promptly (the tail was always the immediately preceding in-range
TB, never an old one).

Verdict: reading the log tail is a sound way to answer "where was the guest last inside the
scheduler", not "where is the guest now". Removing `-dfilter` would fix the staleness at
unmanageable log volume.

## Costs

- Wall clock: Hermit + `-d exec` is 40–50 s versus 27 s bare for this guest; `nochain` takes
  it to 86–88 s. Hermit's own overhead on this workload is about 1.5x.
- Storage: 3–7 MB per run at this workload size, entirely from the module-range filter. An
  unfiltered trace of a full boot would be orders of magnitude larger; not measured.
- Nothing must be built. No guest changes beyond having the map dumper present, which the
  prior experiment already established.

## What this does not answer

- **Instruction granularity.** Everything here is TB-entry granularity. Per-instruction guest
  PCs need TCG plugins, and this QEMU build has them disabled.
- **Correlating a Hermit scheduling decision to a position in the trace.** The trace and
  Hermit's own log are separate files with no shared ordering key, and QEMU does not stamp
  trace lines with the icount value. Nothing here bridges them. Note that the localization
  demonstrated above does not need that bridge: it compares two whole traces.
- **`--chaos`.** Four `--chaos --sched-seed` runs were attempted and abandoned: with the
  `--preemption-timeout disabled` compatibility profile the guest made almost no progress
  (97 KB of trace and no guest console output after ~2 minutes, versus a 44 s complete run
  without `--chaos`). Whether chaos works with a different QEMU/Hermit profile was not
  determined. `--rng-seed` was used instead as the "two different but individually
  deterministic executions" lever, which is what the localization demo needs.
- **`-d cpu`** (full register dump per TB) was not run; only `-d exec`.
- **Whether determinism survives a longer or more concurrent guest.** Everything here is
  `-smp 1` with a short workload.
- **The `/proc/<qemu-pid>/mem` path**, for the debuginfo reason above.
- **KVM acceleration.** `-d exec` is a TCG feature; none of this applies to `-accel kvm`.

## Reproducing

```bash
cd <scratch>            # traces are large; keep them out of the repo
E=experiments/sample-guest-rip-under-hermit_20260812

# guest image: the prior experiment's initramfs plus the lean init used here
gcc -O2 -Wall -static -o bpf_riplines \
    experiments/bpf-source-location-to-guest-rip_20260812/scripts/bpf_riplines.c
cp -a ignored/qemu-linux/initramfs-scx root
cp bpf_riplines root/bin/
cp $E/scripts/guest-init-lean.sh root/init && chmod +x root/init
( cd root && find . -print0 | cpio --null -o -H newc ) | gzip -1 > lean.cpio.gz

cp $E/scripts/run.sh . && chmod +x run.sh

# determinism pair under Hermit, complete traces
HERMIT_ARGS="--rng-seed 1" ./run.sh nc-rng1a hermit -rtc base=utc,clock=vm -d exec,nochain
HERMIT_ARGS="--rng-seed 1" ./run.sh nc-rng1b hermit -rtc base=utc,clock=vm -d exec,nochain
md5sum trace-nc-rng1a.txt trace-nc-rng1b.txt        # expect identical

# a second, different-but-deterministic execution, then localize the divergence
HERMIT_ARGS="--rng-seed 5" ./run.sh nc-rng5 hermit -rtc base=utc,clock=vm -d exec,nochain
python3 $E/scripts/normalize_trace.py --diff \
  trace-nc-rng1a.txt console-nc-rng1a.txt trace-nc-rng5.txt console-nc-rng5.txt

# point sample at a guest address through the gdbstub
cp $E/scripts/gdbsample.sh . && chmod +x gdbsample.sh && ./gdbsample.sh gdbh1 hermit
```

## Files

```
scripts/normalize_trace.py     trace + console -> base-relative symbolic tokens; --diff mode
scripts/run.sh                 one boot: bare | seed | hermit, with the filtered trace
scripts/gdbsample.sh           gdbstub point sample at a guest address from the guest's own map
scripts/gdbinterrupt.sh        gdbstub interrupt + log-tail cross-check
scripts/guest-init-lean.sh     the initramfs init used for every run here
artifacts/trace-manifest.txt   every run: size, TB entries, md5 of PC sequence and of the file
artifacts/console-*.txt        guest consoles (each carries that boot's MAP and KALLSYMS blocks)
artifacts/diff-*.txt           the normalized-trace comparisons quoted above
artifacts/distinct-sites-*.txt the 218 distinct traced sites with source locations
artifacts/gdb-hermit-run*.txt  the two gdbstub point-sample sessions under Hermit
```

Trace files themselves (3–7 MB each, 13 runs) are deliberately not committed; the manifest
records their md5s and the commands above regenerate them.
