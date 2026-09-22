# AsyncSSL ObserverAttachedBeforeConnect scheduler continuation preflight

At `2026-09-21T21:44:37Z`, the prospective boundary fixed:

- host `devbig030.atn3.facebook.com`, Linux
  `7.1.3-0_fbk0_0_g295ad0959f34`, x86_64 AMD EPYC 9D85;
- CPU PMU type 4 and `perf_event_paranoid=1`;
- Hermit revision `4630ee2a61f93221a43e2baec0a15f34102a2c61`,
  binary size 415,330,552, SHA-256
  `5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a`;
- exact local test ELF size 17,651,096, SHA-256
  `47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46`;
- bounded `/dev/loop1` Btrfs filesystem mounted under
  `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt`.

The retained verifier-log subvolume is:

```text
/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-k71-asyncssl-factor2-2936582-179002704088319-1309413-1790027040951673595
```

Before any material run, this exact path must be provisioned as a fresh Btrfs
qgroup-limited subvolume and its live 8,000,000,000-byte limit authenticated by
the unchanged runner. No prior verifier logs may be placed there.

Every invocation must independently authenticate the exact committed record
and the create/run/admission helper blobs sealed in its plan receipt. It must
also authenticate the separate live lethal 64-MiB stdout cap, binary digests,
host facts, live qgroup limits, applied 120-second wall bound, 16-GiB memory
and zero-swap cgroup bounds, 64-MiB stderr bound, 8-GB log-root disk bound, and
safehermit stderr `truncated=false`. Otherwise it is refused or censored.

No Hermit invocation was made while preparing or validating this prospective
record.
