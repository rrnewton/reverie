# TLS terminator scheduler/NOW preflight

At the `2026-09-21T18:54:35Z` planning boundary, the sealed inputs were:

- host `devbig030.atn3.facebook.com`, Linux
  `7.1.3-0_fbk0_0_g295ad0959f34`, x86_64 AMD EPYC 9D85;
- CPU PMU type 4 with `perf_event_paranoid=1`;
- Hermit revision `4630ee2a61f93221a43e2baec0a15f34102a2c61`, binary
  size 415,330,552 and SHA-256
  `5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a`;
- selected local test ELF size 991,876,712 and SHA-256
  `e7a5278312bb9daa6d38a9a03cc2001cd31f8c64a92791d92d5aa47c20f66566`;
- bounded filesystem `/dev/loop1`, Btrfs, mounted at
  `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt`.

The retained verifier-log subvolume is:

```text
/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-tls-sched-now-2936582-20260921-1036584-1790016867759891372
```

Its Btrfs qgroup is capped at 8,000,000,000 bytes.  The pre-plan readback was:

```text
referenced=16384 exclusive=16384 limit=8000000000
```

This bounds explicit retained verifier logs.  Each invocation must separately
authenticate applied safehermit bounds of 120 seconds wall time, 16 GiB memory,
zero swap, 64 MiB stderr, and 8 GiB wrapper log-root disk, with
`truncated=false`.  A separate constant-memory stdout drain retains at most 64
MiB and cgroup-kills on first overflow; its counts, unit, command, and return
code are authenticated, and an overflow is censored.  Otherwise the tuple is
refused or censored.
