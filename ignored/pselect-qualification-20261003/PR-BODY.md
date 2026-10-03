[hermit2, unknown-agentname, gpt-6-astra, devbig014, role=impl]

Background: Reverie implements Linux system calls for programs running in its KVM virtual machine backend. Hermit uses immediate readiness samples to decide whether a program can proceed. A valid `pselect6` sample currently returns `ENOSYS` (not implemented), so the unchanged `poll-readiness` KVM verification case remains disabled.

Owner: kvm (Codex, devbig014) since 2026-10-03 07:48 AM ET

Implement one unmasked sample with an explicit zero timeout and a signed low-word descriptor limit of 0 through 64. Preserve Linux input validation, ordered partial copyout on faults, readiness counts across sets, and the untouched zero timeout. Virtual signal descriptors do not expose their private eventfd carrier's writable state. Zero readiness with an eligible pending guest signal remains an explicit unsupported result, since signal interruption and restart are outside this repair.

This is bounded progress on https://github.com/rrnewton/reverie/issues/620 . It does not complete masked/blocking waits, larger descriptor tables, signal-interrupted completion, or record/replay. No Hermit scheduler, manifest, timeout, assertion, or comparator is relaxed.

Qualification at source byte-identical to this head (the initial runs preceded the commit):

| Check | Result | Wall time |
| --- | --- | --- |
| Build library and static-ELF test binaries | rc 0 | 39.008 s |
| Focused syscall unit tests | 6 passed | 1.002 s |
| Native versus actual KVM integration | 3 passed; 18 native cases and 54 KVM executions across direct, Host and Tool modes | 9.004 s |
| Full KVM library tests | 1,097 passed | 68.013 s |
| Required `/dev/kvm` refusal control | 1 passed, including all 3 new test names | 1.002 s |
| Clippy, all KVM targets, warnings denied | rc 0 | 20.005 s |
| Workspace documentation, warnings denied | rc 0 | 44.017 s |
| Old ENOSYS handler regression mutant | expected rc 101; readiness test fails | 28.008 s |
| Removed pending-signal guard mutant | expected rc 101; pending-state test fails | 24.007 s |

The mutant runs changed only production code; both compiled and reached the intended failing assertions. Candidate bytes were restored exactly. Runs used eight CPUs, Cargo jobs 8, nice 10, no incremental/debug build data, bounded time/output and the lane's disk guard. The unchanged integration child deadline is 30 seconds with an exact named completion record.

The next consumer step is the unchanged eight-check Hermit `poll-readiness` fixture, with its original strict verification and bounds. It has not been run with this change yet; no additional green cell or canonical validation is claimed here. Independent reviews are pending.
