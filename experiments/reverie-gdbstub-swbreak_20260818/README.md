# A breakpoint stop wedges Hermit's gdbserver when sibling threads are blocked

Task `reverie-vcont-resume-drops-thread-waits`, measured 2026-08-18 against
reverie `af82f1b92a2762840dc5894984be5e1f5bfc5cb9`.

The task asked for a fix in `gdbstub/session.rs::vcont_resume`, on the theory
that it abandons other threads' wait futures when a clone arrives. **I did not
make that change.** The measurements below say it would not fix the hang, and
that a straightforward version of it would cause a worse one.

## What actually hangs

Not `vCont`. A gdb packet trace (`set debug remote 1`) of the hanging QEMU case
shows gdb receiving every `vCont` reply, including both thread creations:

```text
Sending packet: $vCont;c:p3.-1   -> Packet received: T05create:p03.05;...
Sending packet: $vCont;c:p3.-1   -> Packet received: T05create:p03.07;...
Sending packet: $vCont;c:p3.-1   -> Packet received: T05swbreak:;...thread:p03.03;
Sending packet: $m555556ffb360,18  -> (no reply, ever)
```

`NewTask` is delivered correctly and `vcont_resume` returns each time. The first
unanswered packet comes *after* a `swbreak` stop, and it is an ordinary request
— a memory read, or a breakpoint removal — that the stopped tracee should
service.

## Minimal reproducer

`livethreads.c`: two threads block in `read()` on a pipe; main sleeps, then hits
a breakpoint while they are still blocked.

```bash
gcc -g -O0 -pthread -o livethreads livethreads.c
hermit run --gdbserver --gdbserver-port 1303 -- ./livethreads
/usr/bin/gdb -batch -x lt.gdb ./livethreads     # break bkpt; continue; delete; continue
```

It hangs. gdb reports `Ignoring packet error, continuing...` repeatedly and then
`Cannot execute this command while the target is running.`

## What the condition is, and what it is not

| case | result |
| --- | --- |
| `manyThreads.c` (reverie's own fixture, 8 threads), no breakpoint | passes, prints `9 0` |
| `manyThreads.c`, same gdb script as below | passes, prints `9 0` |
| `livethreads`, no breakpoint | passes, prints `done 43` |
| `livethreads`, breakpoint | **hangs** |

So it is not thread count and not cloning. The differentiator is **sibling
threads blocked in a syscall at the moment of the breakpoint stop**. Note that
reverie's existing `manyThreads` fixture cannot catch this: its threads run and
exit promptly.

The trigger is sequentialization, and the direction is the surprise:

| configuration | result |
| --- | --- |
| `--sequentialize-threads` (Hermit's default) | **hangs**, 60 s timeout, no output |
| `--no-sequentialize-threads` | passes, prints `done 43` |

`task.rs::freeze_all()` early-returns when the guest is sequentialized, with a
comment that the gdbserver must not do its own thread serialization "otherwise
this could lead to deadlock". The hang therefore happens in exactly the
configuration where `freeze_all` does nothing. The last Hermit debug line before
everything goes idle is `[3] handle_signal: received signal SIGTRAP` — the
breakpoint — with the two sibling dettids sitting in the scheduler queue polling
`InternalIOPolling` for their reads.

That localizes the defect to the `resume_from_swbreak` / `freeze_all` /
scheduler interaction in `reverie-ptrace/src/task.rs`, not to `vcont_resume`.

## Why the requested fix would be harmful

`vcont_resume` resuming N inferiors and returning on the first stop is paired
with `freeze_all()`, which is the intended mechanism for stopping the siblings
afterwards. If `vcont_resume` instead awaited *all* resumed inferiors before
returning, a sibling blocked in `read()` — which never stops on its own — would
prevent it from ever returning, so gdb would never receive the breakpoint stop
at all. That turns a conditional hang into an unconditional one.

## Workaround, available today

QEMU under `hermit run --gdbserver` previously never reached its main loop: QMP
`connect()` returned `EAGAIN` for the full 520 s timeout. With
`--no-sequentialize-threads --max-timeslice disabled` it does — connecting to
QEMU's QMP socket returns the greeting
`{"QMP": {"version": {"qemu": {"major":10,"minor":1,"micro":2} ...}}}`.

State the cost wherever this is used: `--no-sequentialize-threads` gives up
Hermit's deterministic thread scheduling for that run. A breakpoint-stopped
QEMU session under it is a debugger session, not the deterministic-execution
story. `QEMU_BOOT.md`'s existing "fast compatibility" profile already uses that
flag, so this is a known profile rather than a new one.

## Side answer: the dropped async-interrupt packet

A parallel investigation could not tell whether the discarded `0x03` interrupt
packet is queued or never read, without rebuilding reverie. It is queued, and
that is settleable from structure.

`server.rs::run()` does `future::try_join(session.run(), self.relay_gdb_packets())`,
so the socket reader runs concurrently with the session. `relay_gdb_packets`
calls `recv_packet()` then `send_packet()` into `pkt_tx`, an mpsc channel of
capacity 1 (`server.rs:168`, `:191`). While the session is parked awaiting a
stop, an inbound `0x03` is taken off the socket and handed to that channel — it
is not ignored at the socket layer. Capacity is 1, so at most one packet buffers
and the reader blocks on the next send until the session drains. When the
session drains it, `session.rs:757-758` discards it through the empty match arm.

Parsed, queued, then discarded. This is a structural argument from reading the
code, not a measurement on a rebuilt reverie, and should be treated as such.
