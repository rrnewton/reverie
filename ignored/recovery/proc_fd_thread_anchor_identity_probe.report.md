# Native proc-fd anchor identity follow-up

Native-host-only follow-up measured on 2026-09-22. No Hermit invocation.
Environment is Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, x86-64, compiled
with GCC `11.5.0 20240719 (Red Hat 11.5.0-15)` using:

```text
gcc -std=c11 -O2 -Wall -Wextra -Werror -pthread ignored/recovery/proc_fd_thread_anchor_identity_probe.c -o ignored/recovery/proc_fd_thread_anchor_identity_probe
```

Compilation and all three executions exited zero; compile and runtime stderr
were empty.

## Exact run 1

```text
identity tgid=1669815 leader_tid=1669815 worker_tid=1669816
open anchor=/proc/self/fd fd=3 errno=0 (Success)
open anchor=/proc/thread-self/fd fd=4 errno=0 (Success)
readlink label=worker-opened-self path=/proc/self/fd/3 rc=16 errno=0 destination=/proc/1669815/fd
readlink label=worker-opened-thread-self path=/proc/self/fd/4 rc=29 errno=0 destination=/proc/1669815/task/1669816/fd
fstat label=worker-opened-self fd=3 rc=0 errno=0 dev=22 ino=3187808439 mode=040500
fstat label=worker-opened-thread-self fd=4 rc=0 errno=0 dev=22 ino=3187808442 mode=040500
stat label=canonical-process-fd path=/proc/1669815/fd rc=0 errno=0 dev=22 ino=3187808439 mode=040500
stat label=canonical-worker-fd path=/proc/1669815/task/1669816/fd rc=0 errno=0 dev=22 ino=3187808442 mode=040500
stat label=leader-thread-self path=/proc/thread-self/fd rc=0 errno=0 dev=22 ino=3203245169 mode=040500
worker_stat label=/proc/self/fd errno=0 dev=22 ino=3187808439
worker_stat label=/proc/thread-self/fd errno=0 dev=22 ino=3187808442
compare label=self-anchor-vs-canonical-process dev_equal=1 ino_equal=1 same_identity=1
compare label=self-anchor-vs-worker-direct-self dev_equal=1 ino_equal=1 same_identity=1
compare label=thread-anchor-vs-canonical-worker dev_equal=1 ino_equal=1 same_identity=1
compare label=thread-anchor-vs-worker-direct-thread-self dev_equal=1 ino_equal=1 same_identity=1
compare label=thread-anchor-vs-leader-thread-self dev_equal=1 ino_equal=0 same_identity=0
compare label=self-anchor-vs-thread-anchor dev_equal=1 ino_equal=0 same_identity=0
```

## Result and variability

All 3/3 executions had the same structure:

- The descriptor opened by the worker through `/proc/self/fd` read back as the
  canonical link target `/proc/<tgid>/fd`.
- The descriptor opened by the worker through `/proc/thread-self/fd` read back
  as `/proc/<tgid>/task/<worker-tid>/fd`.
- Each anchor's `(st_dev, st_ino)` exactly matched both its canonical direct
  path and the corresponding magic path statted by the worker.
- The worker's thread anchor did not match the leader's
  `/proc/thread-self/fd`, and it did not match the process-wide self anchor.
- Only PID/TID values and procfs inode numbers varied. Canonical target shape,
  inode equality/inequality relations, mode `040500`, return codes, and errno
  remained identical.

## SHA-256 evidence

```text
bb22cafcd3b315b3f9a32d68c5df5d331569b569702e0aa8485f942abb0868f3  proc_fd_thread_anchor_identity_probe.c
c3534be80f55d7646234495c5f1fbbaf6defa469cceab56e69c1a1c539ab46be  proc_fd_thread_anchor_identity_probe
0552a2b286c4ab33ab5fb8c395c87d6e1f400041d55ffa1186ab88b11143555d  proc_fd_thread_anchor_identity_probe.run1.stdout
3c13a1966b8e998f4b7e641e714fbec0e1e228c93d6bba04e181ec511f868293  proc_fd_thread_anchor_identity_probe.run2.stdout
fc1b1122a6adfd61d5e4340a4f17848d665e1fe9b9889442d9415e09d9b116b2  proc_fd_thread_anchor_identity_probe.run3.stdout
```

Binary BuildID is `b9b97b1a0f6cf317c46ab85f2135e359e301badb`; source is
8596 bytes and binary is 19168 bytes.
