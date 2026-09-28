# Native proc-fd directory-anchor lifetime probe

Measured on 2026-09-22. This is a native-host-only pthread probe; it did not
invoke Hermit.

## Environment

```text
Linux 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf #1 SMP PREEMPT Mon Aug 24 01:30:09 PDT 2026 x86_64 GNU/Linux
gcc (GCC) 11.5.0 20240719 (Red Hat 11.5.0-15)
compiler path: /usr/bin/gcc
compiler SHA-256: 546023eae5ff58287b1d987e059d38e2733dfe49eda827282d6942707c4c25d0
glibc: 2.34
```

Build command and status:

```text
gcc -std=c11 -O2 -Wall -Wextra -Werror -pthread ignored/recovery/proc_fd_thread_anchor_native_probe.c -o ignored/recovery/proc_fd_thread_anchor_native_probe
compile_rc=0
compile stdout: empty
compile stderr: empty
```

Binary identification:

```text
ELF 64-bit LSB executable, x86-64, version 1 (SYSV), dynamically linked,
interpreter /lib64/ld-linux-x86-64.so.2,
BuildID[sha1]=ef77cca2f9d1f725a4f7d6e8aae64ac000b41a5e,
for GNU/Linux 3.2.0, not stripped
```

## Method

A non-leader pthread opened `/dev/null` and then opened both
`/proc/self/fd` and `/proc/thread-self/fd` with
`O_PATH|O_DIRECTORY|O_CLOEXEC`. The descriptors entered the process's ordinary
shared pthread file table. The leader called `readlinkat(dirfd, "3", ...)`
while the worker waited, asked it to exit, joined it, and repeated each call.

## Exact first-run output

```text
identity tgid=4010804 leader_tid=4010804 worker_tid=4010806
target fd=3 errno=0 (Success)
open anchor=/proc/self/fd fd=4 errno=0 (Success)
open anchor=/proc/thread-self/fd fd=5 errno=0 (Success)
readlink phase=worker-alive anchor=/proc/self/fd dirfd=4 target_fd=3 rc=9 errno=0 (Success) destination=/dev/null
readlink phase=worker-alive anchor=/proc/thread-self/fd dirfd=5 target_fd=3 rc=9 errno=0 (Success) destination=/dev/null
worker_join rc=0
readlink phase=worker-exited anchor=/proc/self/fd dirfd=4 target_fd=3 rc=9 errno=0 (Success) destination=/dev/null
readlink phase=worker-exited anchor=/proc/thread-self/fd dirfd=5 target_fd=3 rc=-1 errno=2 (No such file or directory)
```

All three executions exited zero with empty stderr and reproduced the same
semantic result (only TIDs/PIDs changed):

- The worker-opened `/proc/self/fd` anchor resolved target fd 3 to `/dev/null`
  while the worker was alive and after it exited.
- The worker-opened `/proc/thread-self/fd` anchor resolved the target while the
  worker was alive, but after worker exit `readlinkat` failed with
  `errno=2` (`ENOENT`). It did not retarget to the leader.

## SHA-256 evidence

```text
6d94c4d2f2e3e07f756cc4951709c2968aed423f4a171fa71317380ae273c1b5  proc_fd_thread_anchor_native_probe.c
dbf63a8d7a7c296a112eb5361582cca16ebd74ee078dc898238e6dee805966db  proc_fd_thread_anchor_native_probe
c9f24446ec6c7976a541ed03581ff5c5d0bf20075fb9b98631a7da1945bf81d2  proc_fd_thread_anchor_native_probe.run1.stdout
57cbbb941f79516eddead645be0c9d96461e499d44b83d63cdec24c0bcc522fe  proc_fd_thread_anchor_native_probe.run2.stdout
a45241177cf957544fc77871795e2bfd2ea45de33fd7c090e051ff9d2082edaa  proc_fd_thread_anchor_native_probe.run3.stdout
```

Source size: 4800 bytes. Binary size: 18696 bytes. Each run's stderr file is
zero bytes.
