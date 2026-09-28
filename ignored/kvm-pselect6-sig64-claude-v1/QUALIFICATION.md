# Completed qualification

- Focused pselect6 validation matrix: 1 passed.
- Serialized `cargo test -p reverie-kvm --lib -- --test-threads=1`: 777 passed,
  0 failed, 0 ignored, 39.11 seconds.
- Production-shaped optimized regression:
  `cargo test --release -p reverie-kvm --features native-test-support --test native_entry_signal`:
  1 passed.
- `cargo clippy -p reverie-kvm --all-targets --features native-test-support -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Exact Hermit consumer: 291491661d280b0feaf5ffc514ce7e1fc3e9c5d0,
  release binary SHA-256 3301b368efda1af6b00dbaeb5cbe1f20ccd27c7dfae8b903f827ab383126bbc4.
- `/bin/true`, strict KVM canonical verification through
  `/home/newton/work/dev-hermit/bin/safehermit`: matched, 100/100 INFO records,
  33 syscalls and 5 scheduler turns in each run.
- `signal_delivery_sequence`, strict KVM canonical verification through
  `safehermit`: matched, 1009/1009 INFO records, identical 48-byte stdout,
  475 syscalls and 13 scheduler turns in each run. The fixture includes the
  ready-fd/inaccessible-inner-mask `pselect6` EFAULT probe.
- Native fixture control returned the same
  `signals raised=5 pending=1 coalesced=1 direct=5` output.

Limits: two earlier package-wide integration attempts at the first commit were
red in 12 pre-existing nested-LocalPool `static_elf` failures. No package-wide
green is claimed. The pselect6 change is validation-only and returns ENOSYS for
otherwise valid calls; general readiness, waiting and temporary-mask semantics
remain unsupported.
