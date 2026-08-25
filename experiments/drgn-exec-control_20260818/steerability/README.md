# Can Hermit steer guest-kernel interleaving? TCG vs KVM in demos 5/6/7

Task `cc-guest-interleaving-steerable`, measured 2026-08-18 on this host.

The worry was that demos 5/6/7 might run under KVM, where the guest executes on
hardware and Hermit cannot see the interleaving. **That worry is refuted: all
three demos run under TCG.** But steering guest-kernel interleaving is still
blocked, for a different reason, and the blocker is `-icount`.

## 1. Which execution mode each demo uses

| demo | accelerator flag | measured `query-kvm` | verdict |
| --- | --- | --- | --- |
| 5 (`demos/lib/qemu_controller.py`) | none passed | `{enabled: false, present: true}` | TCG |
| 6 (`demos/lib/qemu_controller.py`) | none passed | `{enabled: false, present: true}` | TCG |
| 7 (`demos/lib/drgn_hermit.py:608`) | `-accel tcg` | `{enabled: false, present: true}` | TCG |

Demos 5 and 6 pass no `-accel` at all, so this could not be settled by reading
the command line. It was settled by asking the running QEMU over QMP and by
listing its open file descriptors: **zero `/dev/kvm` descriptors**. QEMU's
built-in default is TCG; KVM has to be requested.

The probe is not blind to KVM. This host has working KVM — `/dev/kvm` is
openable, `kvm_amd` is loaded, the CPU reports `svm` — and the same probe with
`-accel kvm` reports `{enabled: true}`, shows a `kvm-nx-lpage-re` kernel
thread, and boots roughly seven times faster.

Reproduce: `python3 accel_probe.py --smp 1 --icount --label demo-shape`

## 2. Is guest-kernel interleaving steerable in that mode?

**No, not as the demos are configured.**

With two guest CPUs, read the vCPU-to-host-thread map from QMP
`query-cpus-fast`:

| configuration | vCPU thread ids | meaning |
| --- | --- | --- |
| TCG + `-icount`, `-smp 2` | `[1930720, 1930720]` | one host thread runs both vCPUs |
| TCG, no `-icount`, `-smp 2` | `[1931121, 1931122]` | two host threads |

`-icount` forces QEMU into single-threaded round-robin TCG. Both guest CPUs are
multiplexed onto one host thread, and the switch points are chosen inside QEMU
against an instruction budget that is deliberately independent of host timing —
that independence is the entire purpose of `-icount`. Hermit schedules host
threads; with only one host thread carrying both vCPUs there is no scheduling
decision left for Hermit to make, and preempting that thread cannot reorder
guest-kernel events.

Dropping `-icount` gives MTTCG and two real host threads. That is the
**necessary** condition for Hermit to steer guest interleaving.

It is not a demonstration of **sufficiency**. Hermit's chaos scheduling was
never observed reordering guest-kernel events, because the no-`icount`
configuration does not boot under Hermit at all (section 3). Treat "Hermit can
steer MTTCG" as untested rather than proven.

## 3. What a two-CPU guest workload costs in a steerable mode

Wall-clock seconds to reach the fixed kernel line `clk: Disabling unused
clocks`, same bzImage, initramfs and snapshot throughout.

| # | accel | smp | icount | hermit | extra | host vCPU threads | result |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | kvm | 2 | – | no | – | 2 | boots, 3.8 s (guest 1.82 s, 2.1x) |
| 2 | tcg | 2 | no | no | – | 2 MTTCG | boots, 10.0 s (guest 3.62 s, 2.8x) |
| 3 | tcg | 2 | yes | yes | `--strict` | 1 RR | boots, 23.8 s (guest 1.55 s, 15.4x) — **the demos' mode** |
| 4 | tcg | 2 | no | yes | `--strict` | 2 | stall, 0 bytes in 750 s |
| 5 | tcg | 2 | no | yes | `--no-virtualize-time`, `--strict` | 2 | Hermit error: strict refuses `gettimeofday` |
| 6 | tcg | 2 | no | yes | `--no-virtualize-time`, no strict | 2 | stall, 0 bytes in 520 s |
| 7 | tcg | 1 | no | yes | `--strict` | 1 | stall, 0 bytes in 360 s |
| 8 | tcg | 1 | no | yes | `--no-virtualize-time`, no strict | 1 | 9,396 bytes then kernel panic |

Row 7 against row 3 is the isolation that matters: dropping `-icount` stalls the
guest under Hermit even with a **single** CPU. The blocker is `-icount`, not
MTTCG and not SMP.

Row 8's panic, in full:

```text
tsc: Fast TSC calibration failed
Kernel panic - not syncing: IO-APIC + timer doesn't work!
  ...
  setup_IO_APIC / apic_bsp_setup / x86_late_time_init / start_kernel
```

**Emulation speed is not the problem.** Two guest CPUs under MTTCG boot in
10.0 s against KVM's 3.8 s — 2.6x. Nobody should abandon this approach on
performance grounds. The problem is that the steerable mode does not run under
Hermit at all: every route to no-`icount` either produces zero output or panics
the guest kernel.

Reproduce: `python3 boot_timer.py --accel tcg --smp 2 --hermit --label x`

## 4. A stale claim in the Hermit docs

`hermit/docs/QEMU_BOOT.md` says of `--no-virtualize-time
--no-virtualize-metadata` that it "sacrifices time determinism for the whole
run but calibrates normally and reaches the expected boot outcome".

Measured, it does not. At one CPU it panics in timer setup (row 8); at two CPUs
it produces no output at all (row 6). With `--strict` it does not start, because
strict mode refuses the now-unvirtualized `gettimeofday` (row 5). The doc claim
is either stale or was only true for a configuration these runs did not
reproduce. It should not be relied on as an escape hatch.

## 5. What would actually unblock this

`QEMU_BOOT.md` already names the fix and scopes it out:

> A fully coherent multi-clock model (a single Hermit time base shared by
> `rdtsc`, `clock_gettime`, and their derived clocks, coordinated across
> threads) would remove the need for either workaround but is out of scope
> here.

That is the prerequisite. Today Hermit answers `rdtsc` from a synthetic
per-thread counter and `clock_gettime` from a separate virtual logical time
base; QEMU derives the guest TSC from the first and the PIT/APIC/PM timers from
the second, so the guest kernel compares two clock domains that drift apart and
fails calibration. `-icount` hides this by making QEMU derive everything from
one instruction-derived clock — which is exactly what also collapses the two
vCPUs onto one host thread.

So the dependency chain for steering guest-kernel SMP races is:

```
coherent Hermit clock model
  -> QEMU boots under Hermit without -icount
    -> MTTCG, two host vCPU threads
      -> Hermit's scheduler has a decision to make
        -> steering guest-kernel interleaving becomes testable
```
