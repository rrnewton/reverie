# Native anonymous-inode probe evidence

Captured 2026-09-22 on checkout
`f05ff333ac752d820d53f7558b7de6b3c44d5fd8`. This was a native Linux-only
probe. Hermit was not invoked.

## Environment and artifacts

- Kernel (`uname -a`): `Linux devbig014.atn7.facebook.com 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf #1 SMP PREEMPT Mon Aug 24 01:30:09 PDT 2026 x86_64 x86_64 x86_64 GNU/Linux`
- Compiler: `cc (GCC) 11.5.0 20240719 (Red Hat 11.5.0-15)`
- Source: `anonymous-inode-native-probe.c`, 10,860 bytes, SHA-256 `618db1bbd0229de3ba546e001488b15f4ad99e213c915371367c096801f9607f`
- Binary: `anonymous-inode-native-probe.bin`, 18,848 bytes, SHA-256 `ce85854ca66c6803b3122a440bd71bd875a65f5bc4ab2c9833314b0dc261f16f`
- Compiler stdout and stderr are preserved in
  `anonymous-inode-native-compile.stdout` and
  `anonymous-inode-native-compile.stderr`; both are zero bytes.
- Compile timing in `anonymous-inode-native-compile.time`:
  `real_s=0.08 user_s=0.06 sys_s=0.02 maxrss_kb=28040 exit=0`.

Exact compile command:

```sh
/usr/bin/time -f 'compile_time real_s=%e user_s=%U sys_s=%S maxrss_kb=%M exit=%x' -o ignored/recovery/anonymous-inode-native-compile.time cc -std=c11 -O2 -Wall -Wextra -Werror -o ignored/recovery/anonymous-inode-native-probe.bin ignored/recovery/anonymous-inode-native-probe.c >ignored/recovery/anonymous-inode-native-compile.stdout 2>ignored/recovery/anonymous-inode-native-compile.stderr
```

The program creates a `pipe2(O_CLOEXEC)` and an
`AF_UNIX/SOCK_STREAM|SOCK_CLOEXEC` socketpair. For each of their four file
descriptors it observes `fstat`, raw `newfstatat` both by `AT_EMPTY_PATH` and
through `/proc/self/fd/N`, raw `statx` by the same two routes, the proc-fd
symlink text, and both `ino` and `mnt_id` from `/proc/self/fdinfo/N`.

## Eight independent process runs

Exact run command:

```sh
for probe_run in 01 02 03 04 05 06 07 08; do /usr/bin/time -f 'wall_time real_s=%e user_s=%U sys_s=%S maxrss_kb=%M exit=%x' -o "ignored/recovery/anonymous-inode-native-run-${probe_run}.time" ignored/recovery/anonymous-inode-native-probe.bin >"ignored/recovery/anonymous-inode-native-run-${probe_run}.stdout" 2>"ignored/recovery/anonymous-inode-native-run-${probe_run}.stderr"; probe_rc=$?; if [ "$probe_rc" -ne 0 ]; then exit "$probe_rc"; fi; done
```

Each row is one fresh native process. The inode columns are the values that
all same-object APIs reported within that process.

| Run | PID | pipe read/write inode | socket end 0 inode | socket end 1 inode | internal elapsed ns |
| --- | ---: | ---: | ---: | ---: | ---: |
| 01 | 3634261 | 4042318650 | 4042318651 | 4042318652 | 137586 |
| 02 | 3634263 | 4082249319 | 4082249320 | 4082249321 | 132859 |
| 03 | 3634265 | 4081365477 | 4081365478 | 4081365479 | 134462 |
| 04 | 3634267 | 4063301082 | 4063301083 | 4063301084 | 110185 |
| 05 | 3634269 | 4069662872 | 4069662873 | 4069662874 | 131137 |
| 06 | 3634272 | 4067762426 | 4067762427 | 4067762428 | 142153 |
| 07 | 3634274 | 4064970615 | 4064970616 | 4064970617 | 127061 |
| 08 | 3634276 | 4082249332 | 4082249333 | 4082249334 | 119149 |

All eight processes returned 0, emitted `RESULT PASS`, and had empty stderr.
The internal timings span 110,185--142,153 ns, with a mean of 129,324 ns.
The separate `/usr/bin/time` files all report
`real_s=0.00 user_s=0.00 sys_s=0.00 maxrss_kb=0 exit=0`; its display
resolution is too coarse for these short processes. Full outputs and timing
records are `anonymous-inode-native-run-01.{stdout,stderr,time}` through
`anonymous-inode-native-run-08.{stdout,stderr,time}`.

## Findings

Within every run and for every descriptor:

- `fstat`, `newfstatat(fd, "", AT_EMPTY_PATH)`,
  `newfstatat(AT_FDCWD, "/proc/self/fd/N", 0)`,
  `statx(fd, "", AT_EMPTY_PATH, STATX_BASIC_STATS)`, and
  `statx(AT_FDCWD, "/proc/self/fd/N", ..., STATX_BASIC_STATS)` agreed on
  device, inode, and mode.
- The decimal number in `pipe:[N]` or `socket:[N]` and `fdinfo`'s `ino: N`
  both equaled the stat/statx inode.
- `fdinfo`'s `mnt_id` equaled `stx_mnt_id` from both statx routes. The probe
  required `STATX_MNT_ID` to be set in each returned mask before making that
  comparison.

The end-to-end identity relationships were identical in all eight runs:

- Both pipe ends shared one `(device, inode)` identity and had identical
  `pipe:[N]` link text, `fdinfo ino`, and `fdinfo mnt_id`.
- The two socketpair ends had different inodes and different `socket:[N]`
  link text, while sharing the socket filesystem device and mount ID.
- The observed consecutive allocation (`socket0 = pipe + 1`,
  `socket1 = pipe + 2`) occurred in all eight runs, but is only an allocator
  observation, not an identity guarantee.

Across processes, PID, all representations of each object's inode (stat,
statx, proc link, and fdinfo), and internal elapsed time varied. After replacing
only `pid=[0-9]+`, proc-link bracket numbers, every `ino=[0-9]+`, and
`elapsed_ns=[0-9]+`, all eight stdout streams had the same SHA-256:
`00b23818fdc9da20d0a53aad859d491d542786bbd98603fe3bd7f89ed4bd6b87`.
Thus every other printed numeric field was stable in these eight runs,
including fd assignments 3/4/5/6, devices, modes, masks, mount IDs, rdev,
nlink, size, block size, and block count.

## statx mask and device details

The probe requested `STATX_BASIC_STATS` (`0x7ff`). Both statx routes returned
`0x17ff` for every object: all basic fields plus `STATX_MNT_ID` (`0x1000`).

| Object | `st_dev` / statx device | `fdinfo mnt_id` | statx `mnt_id` | statx rdev | mode |
| --- | --- | ---: | ---: | --- | --- |
| pipe read and write ends | raw 15 / `0:15` | 17 | 17 | `0:0` | `010600` |
| socketpair ends 0 and 1 | raw 10 / `0:10` | 12 | 12 | `0:0` | `0140777` |

The pipe and socket anonymous objects therefore occupied different pseudo
filesystem device/mount identities on this kernel. Empty-path and proc-path
statx calls returned the same device, inode, mask, and mount ID for a given
descriptor.
