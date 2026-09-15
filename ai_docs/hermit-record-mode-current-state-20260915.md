# `hermit record` current state and blocker list — 2026-09-15

Task: `repro-hermit-record-state`. Deliverable: current-state assessment of
record/replay, a ranked blocker list with effort estimates, and a recommended
first fix.

## Provenance

| item | value |
|---|---|
| hermit | `7d55fcd6441dbaacf7026e2005ac1468d80fb2d5` (main), release build |
| built at | `worktrees/recresearch/hermit/target/release/hermit`, sha256 `e3625ebc0c0ec9741a0e92b857e9d54ce16bdf6e83b68a0d074e117164755af9` |
| backend | ptrace (record/replay supports no other — see §2) |
| host | devbig030, 2026-09-15 |
| invocation | every run through `bin/safehermit` with an explicit `--sh-deadline` |
| fixtures, logs | `ignored/recresearch/` (fixtures, out, rr, drive.sh, rr.sh) |

Every number below is from a run reproduced in this session. Fixture sources are
in `ignored/recresearch/fixtures/`; each was checked to produce a non-empty,
meaningful observable natively before being run under hermit.

## 1. Does record/replay exist, run and replay?

Yes, and for the right guest it is solid. `record start --verify --verify-strict`
compares under `comparison=BitwiseInfoV1 relaxations=none` and passes:

| guest | what it exercises | result |
|---|---|---|
| `/bin/echo hello` | baseline | verify Success |
| `net_steps` | `socket`/`bind`/`listen`/`getsockname` | verify-strict Success, 6/6 recordings identical |
| `epoll_pipe` | `epoll_create1`/`epoll_ctl`/`epoll_wait` over a pipe | verify-strict Success, 6/6 identical |
| `blocking_read` | blocking pipe read that must wait 300 ms of real time | 5/5 correct (`read=5`) |
| `unix_named_spawn` child half | `fork`+`execve`+`connect`+`write`+`read` | child replays byte-correct and exits 0 |
| `tcp_client` | outbound TCP to a server **outside** the container | see §3 — works, and replays with the peer killed |

Process spawn (`fork`, `execve`) and `MAP_SHARED|MAP_ANONYMOUS` memory shared
across `fork` both work: the shm probe read back 4242 written by its child under
record.

## 2. Coverage: 1 enabled cell out of 360

Counted directly from `tests/e2e/manifests/*.yaml` (14 files) and cross-checked
against `ci/expected-e2e-plan.json`:

- 360 manifest tests; **all 360 declare a `replay` mode**; **exactly 1** has it
  enabled: `system-utils/record-getpid`, ptrace only, guest `tests/c/getpid.c`
  (`tests/e2e/manifests/system-utils.yaml:881-930`).
- In `ci/expected-e2e-plan.json`: 733 cells — 724 `verify`, 5 `chaos`,
  3 `custom`, **1 `replay`**.
- Record/replay is declared unsupported on every other backend: 351 entries each
  say "Record/replay is unsupported by KVM / SaBRe / LiteInst", 337 say the same
  for DBT. **Record/replay is ptrace-only.**
- The 359 disabled ptrace replay modes carry reasons such as "Record/replay
  qualification is separate from the initial strict verification" (265) and
  "The focused C sentinel owns the blocking replay contract" (28).
- Networking under record/replay has **zero** coverage. The one AF_UNIX
  networking guest, `tests/rust/network_hello_world.rs`, hardcodes a skip:

  ```rust
  if matches!(std::env::var("HERMIT_MODE"), Ok(mode) if mode == "record") {
      // TODO: Record mode currently hangs for this test
      eprintln!("Skipping test in record mode.");
      return;
  }
  ```
  (`tests/rust/network_hello_world.rs:23-26`). §4 explains exactly why it hangs.

## 3. Record mode is the only mode that can reach an external network

`hermit run` defaults to `NetworkingMode::Local` — an isolated loopback — and
`run --strict` rejects every route to host networking (`hermit-cli/src/bin/hermit/run.rs:2008`
`strict_rejects_every_route_to_host_networking`). `hermit record start` has no
`--network` flag at all and runs against the host network.

Measured against a Python echo server running on the host, same binary, same port,
seconds apart:

| mode | `connect` | outcome |
|---|---|---|
| native | 0 | `ECHO=[EXTERNAL-PING]` |
| `hermit run` | **-1 / ECONNREFUSED (111)** | no data; the guest then died of SIGPIPE |
| `hermit run --strict` | same | same |
| `hermit record start` | 0 | `ECHO=[EXTERNAL-PING]` |
| `hermit replay`, **host server killed first** | 0 | `ECHO=[EXTERNAL-PING]` |

The last row is the important positive result: outbound client traffic is fully
captured, and replay reproduces it with no peer present at all.

Caution on probes here: `readlink("/proc/self/ns/net")` is a **fabricated
constant** — `detcore/src/syscalls/namespace.rs:52` returns the literal
`net:[4026531840]` in both modes — so it cannot be used to tell the two
configurations apart. The connect result can.

Also measured, not record-specific but it hides failures: `hermit run` exited
**0** on the run whose guest was killed by SIGPIPE (`safehermit: exit_code=0`).

## 4. B1 — `accept`/`accept4` are not recorded, so every server hangs on replay

`Recorder::subscriptions()` (`hermit-cli/src/recorder.rs:382-459`) is an explicit
allowlist. It contains `socket`, `connect`, `sendto`, `sendmsg`, `recvfrom`,
`recvmsg`, `getsockname`, `getpeername`, `getsockopt`, `setsockopt`, `poll`,
`ppoll`, `epoll_wait`. It does **not** contain `accept`, `accept4`, `bind`,
`listen`, `socketpair`, `shutdown`, `select`, `pselect6`, `epoll_create1`,
`epoll_ctl`, `sendmmsg`, `recvmmsg`.

A syscall outside the allowlist is executed **live against the real kernel during
replay**. `connect` is inside it, so on replay `connect` returns success from the
event stream without opening any socket. The listening side then blocks forever
in a live `accept()` for a connection replay never makes.

Measured on three independent guests — all three record fine and hang on replay:

| guest | record | replay |
|---|---|---|
| `tcp_errno` (C, TCP loopback, server in a pthread) | rc=0, correct output | **rc=124, hang** |
| `rust_tcp` (Rust std `TcpListener::accept`) | rc=0, correct output | **rc=124, hang** |
| `unix_named_spawn` (named AF_UNIX, `fork`+`execve` child) | rc=0, correct output | **rc=124, hang** |

Replay DETLOG for `tcp_errno` (`ignored/recresearch/out/rep1.detlog`): dtid 5
logs `inbound syscall: accept(3, NULL, NULL) = ?` and **never logs a matching
finish line**, while dtid 3 completes `connect(4,…)=Ok(0)`, `write(4,…,7)=Ok(7)`,
`read(4,…,63)=Ok(7)` entirely from the recording and then parks in `futex`
(`pthread_join`). For `unix_named_spawn` the child process replays perfectly and
exits while the parent stops at the same single `accept` — `grep -c accept` over
the whole replay log is 1.

These are genuine hangs, not slow runs: each ran to the full wall-clock deadline
and was killed there — `safehermit` reported `unit_result=timeout` with
`elapsed_secs` exactly equal to `bound.wall` (60 s for `rep1`, 90 s for
`tcp_errno`). The accept can never be satisfied, because its peer's `connect` is
served from the log rather than performed.

## 5. B2 — spurious `EAGAIN` on blocking AF_UNIX socket reads, record mode only

Isolated to one line. Two fixtures are the same 30-line program; the only
difference is `pipe(pfd)` versus `socketpair(AF_UNIX, SOCK_STREAM, 0, pfd)`. A
forked child sleeps 300 ms of real time and then writes 5 bytes; the parent
issues one **blocking** read.

| fd type | native | `hermit run` | `hermit record start` |
|---|---|---|---|
| pipe | read=5 | read=5 | **5/5 read=5** |
| AF_UNIX socketpair | read=5 | read=5 | **5/5 read=-1, errno 11 (EAGAIN)** |

DETLOG from the failing case: `finish syscall #35: read(3, 0x7fffffffc420, 15) = Err(Errno(EAGAIN))`.
The child then writes into a socket whose peer has gone and is killed by SIGPIPE.

EAGAIN on a socket the guest never made non-blocking is a POSIX violation.
Detcore deliberately forces fds *physically* non-blocking while keeping the
guest-visible `O_NONBLOCK` clear, and relies on "nonblockize-and-retry"
(`detcore/src/fd.rs:302-323`). In record mode that retry does not complete for
sockets. Note the asymmetry already visible in the source: the write path has a
Pipe-only logically-blocking helper
(`detcore/src/syscalls/files.rs:2005-2011`, `execute_blocking_pipe_write`) while
the read path sends `FdType::Socket` and `FdType::Pipe` to the same
`execute_nonblockable_fd_syscall` (`files.rs:1591-1598`) — so the exact line is
not yet identified and localizing it is the first task of the fix.

The natural-shaped version of the same thing, `unix_socketpair_fork` (no sleeps),
across 10 runs each:

| mode | correct | failing |
|---|---|---|
| native | 10/10 | 0 |
| `hermit run` | 10/10 | 0 |
| **`hermit record start`** | **3/10** | **7/10 EAGAIN; 5 of those also SIGPIPE-killed the child** |

## 6. B3 — `--verify` is structurally blind to a wrong recording

`record start --verify --verify-strict` on `unix_socketpair_fork`, 10 runs:

| outcome | count |
|---|---|
| rc=0, `Success: replay matched recording` | 3 |
| **rc=16, `Success: replay matched recording`** | **2** |
| rc=1, `Recording output did not match replay output!` | 5 |

Captured instance of the middle row, verbatim:

```
:: comparison=BitwiseInfoV1 relaxations=none
:: Success: replay matched recording.
```
with exit status **16** — the guest's own failure code from the EAGAIN path.

The mechanism is by design and documented in `hermit-cli/src/metadata.rs:280-320`:
`--verify` compares one recording against its own replay. A recording that
faithfully captures a *broken* execution replays faithfully and verifies green.
`--verify` therefore detects replay infidelity (§4, §7) and cannot detect
recording-time wrongness (§5, §8).

The cross-recording comparator that *would* detect it already exists and must not
be rebuilt: script `scripts/prototypes/cross-recording-compare.sh`, repository
`rrnewton/hermit`, branch `prototype/cross-recording-comparison`, tip
`754942211294ca62eb64e5f2bf393234c7516045`; an unregistered worktree sits at
`ignored/wt-xrec`. No PR exists for it.

## 7. B4 — `select`/`pselect6` are not recorded, so readiness returns 0 on replay

`select_pipe_fw` writes 7 bytes into a pipe, then calls `select` and `pselect`
with a 5 s timeout, then reads.

```
native / recording : SELECT=+001/000 ISSET=+001 PSELECT=+001/000 READ=+007 DATA=[SELDATA]
replay             : SELECT=+000/000 ISSET=-001 PSELECT=+000/000 READ=+007 DATA=[SELDATA]
```

On replay the `write` is served from the log and moves no real bytes, so the live
`select` correctly sees nothing ready and returns 0. `poll`, `ppoll` and
`epoll_wait` are recorded and do not have this problem — `select`/`pselect6` are
the missing members of the readiness family. `--verify-strict` **does** catch
this (rc=1, "did not match"); a bare `hermit replay` does not and silently
produced different guest output.

## 8. B5 — SCM_RIGHTS descriptor passing is broken

`scm_rights` passes an open fd from a forked child to its parent over an AF_UNIX
socketpair; the parent reads `/etc/hostname` through the received fd.

| mode | `recvmsg` | got fd | read through it | content |
|---|---|---|---|---|
| native (8/8) | 1 | yes | 27 bytes | `devbig030.atn3.facebook.com` |
| `hermit run` (8/8) | 1 | yes | **-1 / EBADF (9)** | empty |
| `hermit record start` | **5/8 -1 / EAGAIN (11)**, 3/8 as above | — | -1 | empty |

Under `hermit run` this is deterministic and silently wrong: the control message
reports a descriptor the guest cannot use. Under record it is additionally
non-deterministic across independent recordings.

## 9. B6 — deliberate ENOSYS refusals

Same binaries, native versus under hermit:

| probe | native | under hermit (run and record) |
|---|---|---|
| `io_uring_setup` | returns fd 3 | **-1 / ENOSYS (38)** |
| `shmget(IPC_PRIVATE, …)` | returns id 0 | **-1 / ENOSYS (38)** |
| `mmap(MAP_SHARED\|MAP_ANONYMOUS)` + fork | 4242 | 4242 (works) |

These are intentional: `detcore/src/syscall_classification.rs:1051`
`is_unsupported_async_ipc_syscall()` returns a fixed ENOSYS for the
`io_setup`/`io_submit` family, `mq_*`, `msg*`, `sem*`, `shmat`/`shmctl`/`shmdt`/
`shmget`, with `io_uring` refused alongside. They are classified `Determinized`
because a fixed refusal *is* deterministic.

**The trap:** a green record/replay verdict on such a program means the guest took
its ENOSYS fallback path, not that io_uring worked. Any target that requires
io_uring or SysV shared memory is out of scope for record mode today.

## 10. Smaller defects found on the way

- **`hermit replay` has no timeout flag.** `record start` has `--record-timeout`;
  replay has none. Every hang in §4 had to be killed by `safehermit`'s deadline.
- **Stale help text.** `hermit record --help` tells the user the configuration
  sets `panic_on_unsupported_syscalls: false`
  (`hermit-cli/src/bin/hermit/record_start.rs:192`). `record_or_replay_config()`
  sets it **true** (`hermit-cli/src/metadata.rs:329`) and a test asserts true
  (`metadata.rs:472`). The help is wrong in the direction that matters: record
  fails closed.
- **`hermit run` exits 0 when the guest is killed by a signal** (§3).
- **`safehermit` exit-code collision.** `safehermit` documents 125 as "the log cap
  fired" and sets it itself on a cgroup kill (`bin/safehermit:44,424`), but it
  also passes the child's status through verbatim — and hermit's own
  `HERMIT_INTERNAL_FAILURE_EXIT` is **125**
  (`hermit-cli/src/lib.rs:124`). Observed on the `tcp_loopback` replay panic:
  `exit_code=125` with `truncated=false, bytes_written=4244`, i.e. the cap did
  not fire and the 125 was hermit's. A reader who trusts the documented meaning
  will diagnose a disk-guard trip that never happened. The same collision exists
  at 124 (deadline versus hermit's `run --timeout` fallback,
  `hermit-cli/src/bin/hermit/container.rs:1135`).

## 11. Ranked blockers, effort, and recommended first fix

Effort is engineer-days including a regression test, for someone already familiar
with the recorder.

| # | blocker | severity | effort |
|---|---|---|---|
| B2 | spurious EAGAIN on blocking AF_UNIX socket reads (§5) | fatal for AF_UNIX IPC; corrupts every recording silently | 1 to localize + 1–3 to fix |
| B1 | `accept`/`accept4` not recorded (§4) | fatal for every accepting service | 2–4 |
| B3a | `--verify` does not surface a non-zero guest exit (§6) | false greens | hours |
| B4 | `select`/`pselect6` not recorded (§7) | fatal for any `select` user; caught by verify | 1–2 |
| B3b | productionize the existing cross-recording comparator (§6) | the only instrument that can see B2/B5 | 2–3 |
| B5 | SCM_RIGHTS descriptor passing (§8) | fatal for fd-passing services; needs a design | 3–5 |
| B7 | enable record/replay manifest cells beyond the 1 sentinel (§2) | no regression protection today | 0.5 per cell, gated on B1/B2 |
| B8 | `hermit replay --timeout` (§10) | hangs are unbounded | hours |
| B9 | stale `record --help` text (§10) | misleads readers | minutes |
| B6 | io_uring / SysV IPC support (§9) | capability gap, deliberate | weeks; treat as out of scope |

**Recommended first fix: B2.** Reasons, in order:

1. It has a **100 % deterministic 30-line repro** with a matched control (the
   pipe variant passes 5/5), so a fix is cheap to verify and impossible to
   hand-wave.
2. It is a *recording-time correctness* bug, and by B3 nothing in CI can see it.
   It is the one that turns every later measurement into evidence of nothing.
3. Every target in scope uses AF_UNIX IPC; a 70 % failure rate on the simplest
   possible case blocks all downstream work.
4. It is strictly smaller than B1: no new event type, no `RECORD_VERSION` bump.

Then B1, which is the largest capability gap and unlocks the already-written,
already-skipped `tests/rust/network_hello_world.rs`. Then B3a (hours) so that no
subsequent work can report a false green.

### Fix shape for B1, for whoever picks it up

Add `accept`/`accept4` to `Recorder::subscriptions()`; add an `AcceptEvent
{ return fd, peer sockaddr bytes, addrlen }` to `SyscallEvent`
(`hermit-cli/src/event.rs:65-107`); bump `RECORD_VERSION` from `0x116`
(`metadata.rs:129`). The replayer must **materialize a real placeholder
descriptor at the recorded fd number** — create a socketpair, `dup3` one end onto
the recorded number, close the rest — so that subsequent recorded
`read`/`write`/`getpeername`/`close` on that fd behave, and so that
non-subscribed calls such as `shutdown` still hit a real socket.

Two in-tree precedents to copy rather than invent:
`handle_replayed_side_effect` (`hermit-cli/src/replayer.rs:1983`) already does
record-the-fd, re-inject, assert-it-matched for `socket`/`dup`/`pidfd_open`; and
`handle_pidfd_getfd` (`replayer.rs:1997`) already handles the
"recreate a real kernel fd on replay and clean up when it lands on the wrong
number" case. `accept` needs that shape minus the re-injection, because the
re-injection is precisely what cannot be done.

## 12. Answering the sizing question directly

> What would it take to record a process that does TCP + AF_UNIX IPC + spawns children?

| capability | status today |
|---|---|
| spawns children (`fork`, `execve`) | **works** — measured, child replays byte-correct |
| outbound TCP client, external peer | **works** — replays with the peer killed |
| shared anonymous memory across fork | **works** |
| epoll readiness | **works** |
| AF_UNIX socketpair IPC | blocked by **B2** |
| named AF_UNIX server (`accept`) | blocked by **B1** (and B2) |
| inbound TCP server (`accept`) | blocked by **B1** |
| `select`/`pselect` readiness | blocked by **B4** |
| passing descriptors over AF_UNIX | blocked by **B5** |
| io_uring, SysV shm/sem/msg | refused ENOSYS by design — **B6** |

B1 + B2, plus B4 if the target uses `select`, is roughly **5–10 engineer-days** to
get the first such process recording and replaying. Add B5 (3–5 days) if it
passes descriptors. Do not start from jarvis: the fixtures in
`ignored/recresearch/fixtures/` are the floor, and each blocker above has a
repro there that a fix can be measured against.
