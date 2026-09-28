# Native inherited-descriptor identity probe

Date: 2026-09-22

Repository HEAD: `5ccad573afc40fb8cda85c07cd9ded5518a12100`

Host:

```text
Linux devbig014.atn7.facebook.com 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf #1 SMP PREEMPT Mon Aug 24 01:30:09 PDT 2026 x86_64 x86_64 x86_64 GNU/Linux
cc (GCC) 11.5.0 20240719 (Red Hat 11.5.0-15)
```

This was a native Linux probe. Hermit was not invoked. The probe uses at most
three simultaneously live probe processes: the initial process, its fork/exec
child, and that child's post-exec fork. Six bounded launches were made. Every
launch exited successfully.

## Probe and commands

The probe source is `probe.c`. Each observation records and internally checks:

- `fstat(fd)`;
- `statx(fd, "", AT_EMPTY_PATH, ...)`, separately requesting
  `STATX_BASIC_STATS`, `STATX_BASIC_STATS | STATX_MNT_ID`, and
  `STATX_MNT_ID_UNIQUE`;
- `readlink("/proc/self/fd/N")`; and
- the `ino:` and `mnt_id:` fields of `/proc/self/fdinfo/N`.

Commands used, with the evidence directory abbreviated as `$evidence`:

```sh
probe_tmp=$(mktemp -d /tmp/inherited-anon-probe.XXXXXX)
cc -O2 -Wall -Wextra -Werror "$evidence/probe.c" -o "$probe_tmp/probe"
"$probe_tmp/probe" objects "$evidence/objects-run-1.txt"
"$probe_tmp/probe" objects "$evidence/objects-run-2.txt"
"$probe_tmp/probe" stdio "$evidence/stdio-regular-distinct.txt" \
  >"$evidence/stdout.reg" 2>"$evidence/stderr.reg"
"$probe_tmp/probe" stdio "$evidence/stdio-regular-shared.txt" \
  >"$evidence/shared.reg" 2>&1
```

The two pipe-capture launches used Python's bounded `subprocess.run`: one with
separate `stdout=PIPE, stderr=PIPE`, and one with `stdout=PIPE,
stderr=STDOUT`. Both asserted return code zero and empty captured payloads.
The resulting reports are `stdio-pipes.txt` and `stdio-pipe-shared.txt`.

The object mode creates one pipe and one Unix `SOCK_STREAM` socketpair, dups
one pipe endpoint and one socket endpoint, observes all descriptors in the
parent, then in a fork child, in that same child after `execv`, and in a
post-exec fork. The stdio mode similarly dups fd 1 and fd 2 and observes those
four descriptors through the same phases. Descriptors survive `execv` because
their native `FD_CLOEXEC` bit is clear.

The bounded compiler output directory was `/tmp/inherited-anon-probe.levGLz`
and contains one probe binary. Binary SHA-256 at the end of the probe:

```text
dea1c7c664504048d94ac5c57322c363c6363b8ddcda1f3ff771c4273cd7c56e
```

## Results

Every report has one `(type, dev, ino, statx mount IDs, fdinfo identity,
readlink target)` signature per underlying object across dup, fork, exec, and
post-exec fork. Each object report has 24 records (six descriptors times four
phases); each stdio report has 16 records (four descriptors times four phases).

Fresh object run 1:

- The pipe's read end, write end, and duplicate all report dev `0:15`, inode
  `2250674498`, legacy mount ID `17`, unique mount ID `2147483666`, and
  `pipe:[2250674498]`.
- Socket endpoint 0 and its duplicate report dev `0:10`, inode `2250674499`,
  legacy mount ID `12`, unique mount ID `2147483661`, and
  `socket:[2250674499]`.
- Socket endpoint 1 has a distinct inode, `2250674500`, but the same socket
  device and mount IDs.

Fresh object run 2 retained the device and mount IDs but received different
host inodes: pipe `2243195418`, socket endpoint 0 `2243195419`, and socket
endpoint 1 `2243195420`. Thus the inode is stable for a live underlying object
through descriptor and process inheritance, but is not stable between fresh
program lineages.

For the anonymous objects, `STATX_BASIC_STATS` returned mask `0x17ff` and
opportunistically supplied legacy mount ID; the explicit legacy request also
returned `0x17ff`; the unique-only request returned `0x47ff` and the distinct
unique mount ID. In every record:

- `fstat` dev/inode equals `statx` dev/inode;
- fdinfo `ino:` equals that inode;
- fdinfo `mnt_id:` equals legacy `statx.stx_mnt_id`; and
- the decimal inode in the proc-fd link equals that inode.

Caller-supplied stdout/stderr topology behaved as follows:

- Distinct regular-file redirections: stdout inode `992361188` and stderr
  inode `992361189`, with distinct path link targets. Both use dev `0:48`,
  legacy mount ID `1972`, and unique mount ID `2552483954`.
- Shared regular-file redirection (`2>&1`): stdout and stderr both use inode
  `992361191` and the same path link target.
- Separate Python pipes: stdout inode `2256052376` and stderr inode
  `2256052377`, with distinct `pipe:[...]` links.
- Shared Python pipe (`stderr=STDOUT`): stdout and stderr both use inode
  `2296987023` and the same `pipe:[2296987023]` link.

All four stdio cases remained stable across the duplicate, fork, exec, and
post-exec fork observations. Regular-file `STATX_BASIC_STATS` returned mask
`0x9fff` and the unique-only request returned `0xcf3f` on this filesystem; the
same cross-interface identity equalities held. These are native OS-level
redirections/captures, not Hermit's in-memory capture mode.

## Implications for KVM initialization

The deterministic identity must be initialized per live underlying object,
not per fd number or per stream name. A host `(st_dev, st_ino)` grouping while
all initial descriptors are live has the required behavior: both pipe ends and
all dups share; socketpair endpoints differ; stdout/stderr share only when the
caller supplied the same object. Anonymous host keys must remain internal and
must not become the guest-visible identity.

The existing classifier and allocator at
`reverie-kvm/src/executor.rs:8836-8912` already classify pipefs/socketfs and
group live objects through the shared identity table. Initial state instead
starts with an empty `fd_object_inodes` map at
`reverie-kvm/src/elf.rs:1652-1734`; stdin is installed only afterward at
`reverie-kvm/src/vm.rs:1715-1737`, while physical stdout/stderr are resolved
directly by `host_fd` at `reverie-kvm/src/executor.rs:15681-15701`.

Therefore initial open standard descriptors need one bounded classification
pass before the first `FileTableState` snapshot at
`reverie-kvm/src/executor.rs:2668-2698`. Seed `fd_object_inodes` only for
pipe/socket objects (or insert Ordinary identities while preserving their
native metadata); do not synthesize ordinary regular-file dev/inode, mount ID,
or proc-fd path. If Hermit's in-memory output capture is enabled, fd 1/2 must
remain in the separate captured-output identity domain: capture-specific stat
and link handling already precedes generic fd identity at
`reverie-kvm/src/executor.rs:8053-8055` and `:13069-13076`, and fdinfo capture
is explicitly refused at `:2289-2306`. Stdin is host-backed and should still be
classified.

Fork already clones the identity map and shared registry at
`reverie-kvm/src/elf.rs:1062-1085`. Guest exec is the remaining lifecycle
hazard: `inherit_process_state_locked` currently retains identities only when
`files.contains_key(fd)` (`reverie-kvm/src/elf.rs:1175-1179`). Physical open
fd 0/1/2 are intentionally absent from `files`, so seeded identities would be
dropped on exec even though native Linux retains them. The retention predicate
must also include each still-open, non-`FD_CLOEXEC`, unshadowed standard slot,
using the same `stdin`/`closed_standard_fds`/`files` conditions as
`is_open_standard`. It must not make a closed, shadowed, or close-on-exec
standard descriptor reappear.

With those identities present, the current fstat/statx sanitizers
(`reverie-kvm/src/executor.rs:15534-15573`), proc-fd link sanitizer
(`:8048-8090`), fdinfo snapshot (`:1923-1939`), proc-fd reopen (`:8581-8596`),
and dup propagation (`:9112-9173`) can preserve one deterministic anonymous
identity across all probed routes. Leaving the initial map empty instead makes
`guest_fd_object_identity` fall back to fd-number-based `Ordinary` identity at
`:15513-15527`, exposing the host pipe/socket metadata and giving later aliases
the wrong kind.

## Evidence hashes

```text
50f749599b85dc772115d0824cf8244772c1fb8c9727896897b77020be35c287  probe.c
d662f1d0d58bf9fac82c27ee9ba7c5d8d0c5a1643a598ed7aedf4e678cc6f2fa  objects-run-1.txt
c7f7653064561e453fbaa70098f2f2c2084236c782f583c62c520fb0d1cf7664  objects-run-2.txt
21bedef9f307b62f1369b7b6d242b8e3da8080f235dbdfa8fd3f884cf94e529a  stdio-regular-distinct.txt
84793fdc37910c27808c53bc62fd536106754de82c52561134444adc68cf989f  stdio-regular-shared.txt
40b110024485675b9f948ecd81134cf4dd2b1fe1228fe8818856f2d9475175f6  stdio-pipes.txt
84f5495c31a5559781ac6667bc271123123d0472b20bb0810ca11be5be6cd594  stdio-pipe-shared.txt
```
