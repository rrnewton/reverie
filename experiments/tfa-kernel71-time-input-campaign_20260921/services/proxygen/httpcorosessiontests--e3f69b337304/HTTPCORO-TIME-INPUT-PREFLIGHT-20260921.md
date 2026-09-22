# HTTPCoro time-input preflight

At `2026-09-21T18:14:41Z`, the pre-run boundary revalidated:

- host `devbig030.atn3.facebook.com`, Linux
  `7.1.3-0_fbk0_0_g295ad0959f34`, x86_64 AMD EPYC 9D85;
- CPU PMU type 4 and `perf_event_paranoid=1`;
- sealed Hermit binary size 415,330,552 and SHA-256
  `5c422b8599ebc9471f8395f52f4f8c286919fad09bee1b4e7b63199b010fe04a`;
- selected test ELF size 48,908,168 and SHA-256
  `00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc`;
- bounded filesystem `/dev/loop1`, Btrfs, mounted at
  `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt`.

The retained verifier-log subvolume is:

```text
/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-httpcoro-time-2936582-20260921-2670399-1790014467662735185
```

Its Btrfs qgroup is `0/407`.  Creation read back an 8,000,000,000-byte limit;
the pre-run report was:

```text
referenced=0 exclusive=0 limit=8000000000
```

This bounds explicit retained verifier logs.  Each invocation must separately
show applied safehermit wall, cgroup memory/swap, stderr-byte, and disk-log-root
bounds with `truncated=false`; otherwise the tuple is censored.
