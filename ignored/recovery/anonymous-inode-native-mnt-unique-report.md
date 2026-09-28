# Native `STATX_MNT_ID_UNIQUE` supplement

Captured 2026-09-22 at checkout HEAD
`f05ff333ac752d820d53f7558b7de6b3c44d5fd8`. This is a separate native
Linux probe and does not modify the original anonymous-inode evidence. The
original `anonymous-inode-native-report.md` SHA-256 was and remains
`7567640a02445832f2d11f1a25cf6497fec50f4fd21b00a3c1b349a856d98aab`.
Hermit was not invoked.

## Environment and artifacts

- Kernel (`uname -a`): `Linux devbig014.atn7.facebook.com 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf #1 SMP PREEMPT Mon Aug 24 01:30:09 PDT 2026 x86_64 x86_64 x86_64 GNU/Linux`
- Compiler: `cc (GCC) 11.5.0 20240719 (Red Hat 11.5.0-15)`
- Source: `anonymous-inode-native-mnt-unique-probe.c`, 9,476 bytes,
  SHA-256 `db8bf450661d7818a1737fd03142308d8436fe4762a0579c7409c0d2ceaaf418`
- Binary: `anonymous-inode-native-mnt-unique-probe.bin`, 18,584 bytes,
  SHA-256 `b46db9c2765669f680a7ae734502bbce499bb4871f7f3aed67c248c986a7476d`
- Compile stdout/stderr are zero bytes. Compile timing was
  `real_s=0.07 user_s=0.05 sys_s=0.01 maxrss_kb=27996 exit=0`.

Exact compile command:

```sh
/usr/bin/time -f 'compile_time real_s=%e user_s=%U sys_s=%S maxrss_kb=%M exit=%x' -o ignored/recovery/anonymous-inode-native-mnt-unique-compile.time cc -std=c11 -O2 -Wall -Wextra -Werror -o ignored/recovery/anonymous-inode-native-mnt-unique-probe.bin ignored/recovery/anonymous-inode-native-mnt-unique-probe.c >ignored/recovery/anonymous-inode-native-mnt-unique-compile.stdout 2>ignored/recovery/anonymous-inode-native-mnt-unique-compile.stderr
```

For each end of a pipe and AF_UNIX stream socketpair, the probe issued all of
these through both `statx(fd, "", AT_EMPTY_PATH, ...)` and
`statx(AT_FDCWD, "/proc/self/fd/N", ...)`:

- legacy mount plus basic fields: request `0x17ff`;
- exact `STATX_MNT_ID_UNIQUE` only: request `0x4000`;
- basic fields plus `STATX_MNT_ID_UNIQUE`: request `0x47ff`.

It also read the legacy `mnt_id` from `/proc/self/fdinfo/N`.

## Three fresh-process runs

Exact command:

```sh
for unique_run in 01 02 03; do /usr/bin/time -f 'wall_time real_s=%e user_s=%U sys_s=%S maxrss_kb=%M exit=%x' -o "ignored/recovery/anonymous-inode-native-mnt-unique-run-${unique_run}.time" ignored/recovery/anonymous-inode-native-mnt-unique-probe.bin >"ignored/recovery/anonymous-inode-native-mnt-unique-run-${unique_run}.stdout" 2>"ignored/recovery/anonymous-inode-native-mnt-unique-run-${unique_run}.stderr"; unique_rc=$?; if [ "$unique_rc" -ne 0 ]; then exit "$unique_rc"; fi; done
```

| Run | PID | pipe inode | socket inodes | pipe legacy / unique mount ID | socket legacy / unique mount ID | elapsed ns |
| --- | ---: | ---: | --- | --- | --- | ---: |
| 01 | 233354 | 89950179 | 89950180, 89950181 | 17 / 2147483666 | 12 / 2147483661 | 130757 |
| 02 | 233397 | 152920315 | 152920316, 152920317 | 17 / 2147483666 | 12 / 2147483661 | 185789 |
| 03 | 233414 | 152920328 | 152920329, 152920330 | 17 / 2147483666 | 12 / 2147483661 | 137276 |

All three processes returned 0, printed four object-level `agreement=PASS`
lines and `RESULT PASS`, and had empty stderr. Internal timing ranged from
130,757 to 185,789 ns, mean 151,274 ns. Each `/usr/bin/time` record reports
`real_s=0.00 user_s=0.00 sys_s=0.00 maxrss_kb=0 exit=0`.

## Results

The exact `0x4000` request returned mask `0x47ff` on every call. Thus this
kernel returned the requested `STATX_MNT_ID_UNIQUE` bit plus all
`STATX_BASIC_STATS` bits even when only `0x4000` was requested. It did not set
the legacy `STATX_MNT_ID` bit (`0x1000`) in that result. Requesting the combined
`0x47ff` mask returned the same `0x47ff` mask and the same values.

The returned unique mount IDs were stable across all objects, routes, and
runs for each pseudo filesystem:

| Object filesystem | Device | fdinfo legacy `mnt_id` | legacy statx ID/mask | unique statx ID/mask | differs from fdinfo |
| --- | --- | ---: | --- | --- | --- |
| pipe | `0:15` | 17 | 17 / `0x17ff` | 2147483666 (`0x80000012`) / `0x47ff` | yes |
| socket | `0:10` | 12 | 12 / `0x17ff` | 2147483661 (`0x8000000d`) / `0x47ff` | yes |

For both kinds, the unique ID exceeded fdinfo's legacy ID by 2,147,483,649
(`0x80000001`) in this boot. This arithmetic relationship is an observation,
not a kernel ABI guarantee. The important semantic result is that the unique
ID is a different namespace/value: it must not be compared for numeric
equality with fdinfo's legacy `mnt_id`.

For every descriptor, the fd-empty and proc-path routes agreed on returned
mask, unique mount ID, device, inode, and mode. Both pipe ends shared the pipe
mount ID; both socket endpoints shared the socket mount ID; the pipe and socket
unique mount IDs differed.

After normalizing only PID, inode, and elapsed-nanosecond fields, all three
stdout streams had SHA-256
`8340104f795880bf90de05754a71f469616414a58fe3469d506dfff3d977a672`.
The full records are
`anonymous-inode-native-mnt-unique-run-01.{stdout,stderr,time}` through
`anonymous-inode-native-mnt-unique-run-03.{stdout,stderr,time}`.
