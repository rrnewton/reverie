# Differential btrfs repro kernels (BLOCKER B2 / goal-repro-btrfs-f6a6c280)

Task: `btrfs_repro_blocker_build_2` (hermit-275, 2026-07-28).

Two **lean, fast-boot** Linux kernels built with **identical config** so the ONLY
difference is the btrfs source — the substrate for a differential repro of the
subvolume-deletion inodes-xarray ABA race (CVE-2025-39884, upstream fix
`f6a6c280`).

## Artifacts

| file | commit | git describe | md5 |
|------|--------|--------------|-----|
| `bzImage-btrfs-buggy-9786531` | `9786531399a679fc2f4630d2c0a186205282ab2f` | `6.16.0-rc7-00239-g9786531399a6` | `8615440722b02318e92df9c4523c003b` |
| `bzImage-btrfs-fix-f6a6c280`  | `f6a6c280059c4ddc23e12e3de1b01098e240036f` | `6.16.0-rc7-00240-gf6a6c280059c` | `4bc5f1a869c26b60b114fbe6096c0ce8` |
| `kernel.config` | (used for BOTH) | — | `7d39f6ac67317130800a76624da814e3` |
| `hermit-btrfs.fragment` | (config delta over x86_64_defconfig) | — | — |

- **fix = buggy + exactly one commit** (`f6a6c280`): `fs/btrfs/inode.c`, +11/-1.
  Nothing else differs between the two source trees.
- Both bzImages 14,656,512 bytes (~14 MB) — leaner than the fbk 6.18.39 vm2
  kernel, comparable to the demo host kernel (12.7 MB), and boot ~3.6s guest time.

## Provenance / how they were built

Source: a **local clone** of `ignored/linux` (which is owned by hermit-linux and
STRICTLY READ-ONLY for others). The clone reads that tree via hardlinked objects
and never mutates its refs or working tree. Verified after build: `ignored/linux`
still on `research/btrfs-subvol-fix` @ `f6a6c280`, clean.

Clone lives at `ignored/btrfs-kernel-build/linux/` (gitignored). Branches:
`research/btrfs-subvol-buggy` (9786531) and `research/btrfs-subvol-fix` (f6a6c280).

Config = `x86_64_defconfig` + `hermit-btrfs.fragment`, which forces (per the
upstream reproducer + the harness needs):

- `CONFIG_PREEMPT_NONE=y` (upstream reproducer requires PREEMPT_NONE)
- `CONFIG_BTRFS_FS=y` (built-in — no module load in the initramfs)
- `CONFIG_BLK_DEV_RAM=y` (brd `/dev/ram0` for the disposable btrfs image)
- `CONFIG_BLK_DEV_INITRD`, `DEVTMPFS[_MOUNT]`, `TMPFS`, `SERIAL_8250_CONSOLE`
- `CONFIG_DETECT_HUNG_TASK=y` + `SOFTLOCKUP_DETECTOR=y` (candidate B4 oracle:
  the buggy kernel's failure mode is a cleaner-thread soft-lockup / hang)

Build host: devbig030, gcc 11.5.0, 316 cores. Build time ~40-64s each.

Reproduce with `./build.sh` (in the parent dir).

## Validation (all at these exact SHAs)

Boot smoke test = the existing ramdisk harness
(`ignored/btrfs-hermit-investigation/initramfs-ramdisk.cpio.gz`): dd a 128 MB
btrfs image into `/dev/ram0`, mount btrfs, run the generic/418 dio-invalidate
helper + a fixed-seed 3-worker concurrent VFS workload, assert file count, clean
power-down. Logs in `../boot-smoke/`.

| kernel | native TCG (-cpu max -smp 2) | hermit --strict (icount) |
|--------|------------------------------|--------------------------|
| buggy `9786531` | `BTRFS_HERMIT_PROBE_OK files=17`, exit 0, ~4.5s guest | `BTRFS_HERMIT_PROBE_OK files=17`, exit 0, 51s wall / 3.58s guest |
| fix `f6a6c280`  | `BTRFS_HERMIT_PROBE_OK files=17`, exit 0, ~4.5s guest | `BTRFS_HERMIT_PROBE_OK files=17`, exit 0, 63s wall / 3.58s guest |

Both kernels PASS this harness identically — expected: this workload is
generic/418, NOT the subvol-delete race. B2's job is only to produce two
bootable, hermit-strict-compatible kernels that differ solely by the fix commit;
actually EXPOSING divergence is B3 (race-exposure) + B4 (oracle), which need a
subvol-delete reproducer workload and the hung-task/soft-lockup oracle wired in.

### Reproducible hermit --strict invocation

```
RUST_LOG=warn hermit run --strict --no-rcb-time \
  --target-timeslice 100000 --max-timeslice 1000000000 -- \
  qemu-system-x86_64 -machine q35 -accel tcg -cpu max -smp 2 -m 512M \
  -display none -monitor none -serial mon:stdio -no-reboot \
  -icount shift=0,sleep=off -rtc base=2022-01-01T00:00:00,clock=vm \
  -kernel <artifacts>/bzImage-btrfs-{buggy-9786531,fix-f6a6c280} \
  -initrd ignored/btrfs-hermit-investigation/initramfs-ramdisk.cpio.gz \
  -append "console=ttyS0 panic=-1 rdinit=/init"
```

`-cpu max` is load-bearing (the dio helper uses SSE4.2; qemu64 traps illegal
opcode). `-icount shift=0,sleep=off` is required for hermit host-time
virtualization to not corrupt the guest clocksource. `--no-rcb-time` +
timeslice knobs match hermit-274's validated QEMU-under-hermit profile.
