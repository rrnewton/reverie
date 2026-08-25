# Execution control: stop Linux under Hermit mid-run, inspect it, resume

Task `cc-exec-control`. Measured 2026-08-18.

Demo 7 already reads real guest kernel state with drgn, but only where QEMU has
cooperatively parked itself. This adds the missing half: stop the guest
**mid-execution**, read it there, resume, and show the pause cost the guest
nothing.

Files here are copies of demos 5, 6, 7 and their `lib/`, so the published demos
stay untouched. `lib/drgn_hermit.py` is the copy, with execution control added.

| file | what it is |
| --- | --- |
| `pause-midrun.py` | **the working result.** Stop mid-execution, drgn read, resume, measure the cost. |
| `exec-control.py` | the gdbserver route. Blocked; see below. |
| `startup-timing.py` | isolates the gdbserver deadlock from mere slowness. |
| `steerability/` | separate task `cc-guest-interleaving-steerable`. |

## The result

```bash
drgn -q -p $$ pause-midrun.py --label demo --dwell 60
```

Restores Demo 5's boot snapshot under Hermit, sets the guest running a fixed
command, then SIGSTOPs Hermit's exact tracer thread group at an arbitrary
moment **while the guest is executing** — not at a QMP boundary, which is what
Demo 7 does. At the stop it checks QEMU is in ptrace-stop `t` and the tracer in
`T`, walks the live task list with drgn, holds the stop, resumes, and runs to
the completion marker.

At the stop: `qemu=t tracer=T serial_delta=0`, 83 tasks walked via 6,919
physical reads of `/proc/<qemu-pid>/mem`. After resume the guest finishes
normally and the task list grows to 85 — the two `sleep &` it was told to
create.

## Does pausing perturb the guest? No.

This is the claim the whole idea rests on, so it gets the kernel's own clock
rather than a userspace read of it: two `/dev/kmsg` writes bracket the pause,
and the kernel prints each with a printk timestamp at 1 µs resolution. Three
repetitions per configuration:

| configuration | wall clock held | guest seconds across the pause |
| --- | --- | --- |
| no pause | — | 0.002797, 0.002772, 0.002796 |
| pause, 0.03 s held | 0.03 s | 0.002788, 0.002796, 0.002797 |
| pause, 60.03 s held | 60.03 s | 0.002824, 0.002789, 0.002797 |

All nine values lie in `[0.002772, 0.002824]` — a 52 µs spread, and the *same*
spread appears in the unpaused baseline, so it is run-to-run variation rather
than an effect of pausing. Baseline mean 2788 µs against 60 s-dwell mean
2803 µs: a 15 µs difference, smaller than the baseline's own 25 µs spread.

**60,000,000 µs of wall clock, held with the guest stopped mid-execution, cost
the guest under ~50 µs of its own time** — under one part per million, and not
distinguishable from zero at this resolution.

Corroborating state result, taken before the guest command emitted any timing
information: two unpaused baselines, a 0.03 s pause and a 30 s pause all
produced byte-identical advance transcripts (`b7b6898519efd655`) and identical
final task lists (`18b058891bccacc5`). State is unperturbed as well as time.

### What this does not show

- The stop lands at a **wall-clock-dependent point**, because it uses the
  SIGSTOP primitive rather than a breakpoint. **Superseded — see "Breakpoints
  now work" below; the blocker was fixed and a breakpoint does give a chosen
  stop point.**
- "Free" is bounded by measurement resolution. The leak is under ~50 µs per
  60 s pause; it is not shown to be exactly zero.
- The two paused runs happened to stop at different points (8 and 16 serial
  bytes in) and still agreed. That is supporting evidence, not a controlled
  sweep of stop positions.

## The whole loop, as one capability

```bash
HERMIT_RELEASE=<hermit> drgn -q -p $$ session.py --stop-at 100 --port 33301
```

`session.py` prints a readable transcript of the whole loop; two are recorded in
`sessions/`. It restores the boot snapshot, sets the guest running, stops it at
a chosen point, walks `task_struct.tasks` with drgn, and resumes — about 3.5 s
end to end.

| stop at | tasks | named `sleep` | state digest |
| --- | --- | --- | --- |
| 1st guest console byte | 83 | 0 | `2b2855580df35cf6` |
| 100th console byte | 85 | 2 | `441f64d5e27fa3d7` |

Each was run twice. Two independent runs at the same index produce
**byte-identical transcripts** once the legitimately-varying lines are
normalised out (QEMU pid, wall timings, port). A different index gives a
different and meaningful state: byte 1 lands before the guest command has
forked its two `sleep` processes, byte 100 after.

At the stop the read shows per-task pid, comm and decoded state — e.g.
`by state: I (idle kthread) x58, R (running) x2, S (sleeping) x19` — served by
7,392 physical reads (23 MB) of `/proc/<qemu-pid>/mem`, with QEMU in ptrace-stop
`t` and **zero** guest console bytes emitted during the read.

## The unified drgn interface: found, not yet buildable

The remaining gap is that this is gdb driving control and drgn driving
inspection, rather than drgn driving both. Omar Sandoval's execution-control
work is real and tracked at [osandov/drgn#626]. From his kernel status posts, it
introduces "a pluggable **execution controller** that manages stopping/resuming
threads and setting breakpoints", alongside pluggable thread finders and
register-state finders. The same work added drgn's QEMU/QMP target
(`Program.set_qemu_qmp`), which is what ships in 0.2.0.

It is not implementable against today:

| checked | result |
| --- | --- |
| installed drgn 0.2.0 API | no continue/step/breakpoint/controller hook anywhere |
| `osandov/drgn` main | no `ExecutionController` / `execution_controller` |
| branch `thread-finder` | the prep work only — thread and register-state finder APIs |
| all 14 published branches | no breakpoints or execution-control branch |

As of his July 2026 update he had reworked the whole thing to be fully async
after hitting stepping-over-software-breakpoints and REPL integration. The shape
is still moving, so a Hermit backend would be guessing at an API its author is
redesigning.

When it lands, the adapter should be small: Hermit already provides exactly the
primitives that controller is defined over — set a breakpoint, stop at a chosen
point, read memory, resume — as demonstrated above.

A half-step available now and **not** taken: switching the inspection half from
this harness's hand-rolled physical-memory callback to drgn's own
`set_qemu_qmp` target. Unresolved caveats — it wants `-device vmcoreinfo`, which
our QEMU command does not pass, and it would contend for the QMP socket the
harness already owns. It would not reduce the gdb glue for *control*, which is
the half blocked on #626.

## Breakpoints now work, and the stop point is chosen

The blocker below was two separate defects, both since fixed and pushed:

| defect | fix |
| --- | --- |
| reverie released frozen siblings one event too late after a breakpoint | reverie `gdbserver-thaw-siblings-on-resume_20260818` |
| hermit's *run* path never called `sequentialized_guest()`, so reverie froze threads that detcore had parked | hermit `gdbserver-sequentialized-guest-on-run-path_20260818` |

The second is what unblocked QEMU: hermit's default mode is sequentialized, and
the run path was missing a call the replay path already had. QEMU under `hermit
run --gdbserver` previously never reached its main loop — QMP `connect()`
returned `EAGAIN` for 520 s. It now reaches QMP, and `exec-control.py` runs the
whole loop — breakpoint stop, drgn read, resume — in about 3.5 s.

Stopping at the Nth guest console byte, two independent runs each:

| stop at | tasks at the stop | task-list digest | physical reads |
| --- | --- | --- | --- |
| byte 1 | 83 | `81779c19a80f9b5f` | 6,919 |
| byte 1 (repeat) | 83 | `81779c19a80f9b5f` | 6,919 |
| byte 100 | 85 | `18b058891bccacc5` | 6,999 |
| byte 100 (repeat) | 85 | `18b058891bccacc5` | 6,999 |

Same index reproduces the same guest state; a different index selects a
different one — byte 1 lands before the advance command has created its two
`sleep` tasks, byte 100 after. QEMU was in ptrace-stop `t` at every stop. That
is a **chosen** stop point, in contrast to the SIGSTOP pause above, which landed
at 8 or 16 console bytes depending on the wall clock.

Not claimed: full-run bitwise determinism. The whole-serial-log digest still
varies between runs. What is reproducible is the guest state at the chosen stop.

## Original blocker (historical): Hermit's gdbserver deadlocks on multithreaded QEMU

The intended route was `hermit run --gdbserver` with a breakpoint, which would
give a *deterministic* stop location. It does not work with QEMU.

- Demo 7 guest without the debugger: `start()` completes in **1.8 s**.
- Same guest with `--gdbserver`: QMP `connect()` returns `EAGAIN` for the full
  520 s timeout. QEMU creates `qmp.sock` but never reaches its accept loop.
- At the hang: all **three** QEMU threads in state `t`, `wchan=ptrace_stop`;
  all four Hermit tracer threads in `S`; gdb in `S`. Nothing is runnable, and
  gdb's log ends at `continue` with no stop reply ever returned.
- Not caused by Demo 7's scheduler flags — identical hang with
  `scheduler_flags=()`.
- Single-threaded control works: `hermit run --gdbserver --
  qemu-system-x86_64 --version` completes normally. It is not QEMU the binary;
  it is QEMU with threads. A four-thread *userspace* program also works.

In `reverie/reverie-ptrace/src/gdbstub/session.rs`, `vcont_resume` resumes the
matching inferiors, pushes a `wait_for_stop()` future for each, then `break`s
out of the collect loop on the first event that is not `ThreadExited`. A clone
arrives as `StopReason::NewTask`, registers a new `Inferior` with
`resume_pending: false`, and breaks — dropping the other threads' wait futures.
In the observed hang no stop reply reaches gdb at all, so the resume loop is
parked in `inferiors_to_wait.next().await` while every thread already sits in a
ptrace stop.

Fixing that is a Reverie core change to the gdbstub resume/stop plumbing, with
its own review surface.

Two smaller things found on the way:

- **Use `/usr/bin/gdb`, never bare `gdb`.** The PATH default here is a Meta
  build of GDB 9.1 which core-dumps on `qemu-system-x86_64`'s MiniDebugInfo
  (`stap-probe.c:1293: internal-error: sect_index_data not initialized`).
  `/usr/bin/gdb` is 16.3 and works.
- **Hermit's gdbstub silently drops the async interrupt.** `session.rs` parses
  packet `0x03` and then handles it with an empty match arm, under a comment
  saying `// handle interrupt`. A gdb `Ctrl-C` therefore never stops the guest
  and gdb waits forever. Combined with `server.rs` accepting exactly one
  connection and never reconnecting, every breakpoint must be armed at the
  first attach, before the guest starts.

## Choosing the breakpoint site, if the deadlock is ever fixed

Measured with `strace -f -c` over a 30 s standalone run of this QEMU
configuration:

| syscall | calls |
| --- | --- |
| futex | 2,950,650 |
| write | 1,437,920 |
| ppoll | 482,793 |
| read | 482,895 |
| **writev** | **243** |

The serial log was exactly 243 bytes. QEMU emits guest console output one byte
per `writev` and uses `writev` for nothing else here, so `break writev` means
"stop as the guest emits its next console byte" — rare enough to be affordable
over the RSP and at a meaningful point. `break write` would have been 1.4
million gdb round trips.

Also note that Demo 7's tracer freeze and the gdbserver are mutually exclusive:
SIGSTOPping the tracer thread group would freeze the thread serving the
gdbserver socket. A breakpoint stop has to *replace* the freeze, not compose
with it.
