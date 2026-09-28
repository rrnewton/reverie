# Hermit wait-consumption contract evidence

Hermit repository head: 3fe4f5929d07bc8a38e92aa16551c1cf436f60c4
Hermit tree: 11f519db98522ea49a16f5c98f3a294b3a0c37f2
That head pins Reverie base f7bd85e11dd258112148ed2cba6531501a1a00d9; the consumer snapshots in `consumer/` are exact files from this head.

Review the complete files rather than trusting this routing summary:

- Detcore only selects or parks child waits in scheduler state. Its scheduler `consume_child_wait` removes only Detcore's shadow lifecycle table.
- `wait4` ready-child branches inject the exact backend wait first in `detcore/src/syscalls/threads.rs`, as do ordinary/poll paths through `helpers.rs`; terminal shadow retirement happens only after the injected result or after `ECHILD` establishes the backend no longer owns it.
- `waitid` likewise performs nonblocking, blocking-ready, or legacy-poll injection before shadow retirement. `WNOWAIT` never consumes.
- KVM's `Guest::inject` reaches `ElfExecutor::execute`, whose wait implementation removes `state.children`, records the exact actually-selected PID, and only then retires the exact-parent family zombie.

The intended contract is therefore not a syscall-free Tool reap API: scheduler state controls when and which wait may run, while backend injection owns the actual reap. Scrutinize this claim against the complete consumer snapshots and the Reverie candidate code.
