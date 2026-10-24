/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A traced thread's side of a gdb stop: waiting for gdb's resume while
//! answering its requests, and stopping for gdb's interrupt inside a Tool
//! callback, which holds the thread ([`ParkedForGdb`]).
//!
//! A thread answers gdb's requests itself, on its own channel. Inside a Tool
//! callback the callback holds the thread (`&mut TracedTask`), so a stop there
//! is made from what the thread set aside before the callback began: its
//! channels ([`GdbPorts`]), its tracee and breakpoints ([`GdbTracee`]), and
//! the registers it entered the callback with.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use futures::future;
use futures::future::Either;
use nix::sys::signal::Signal;
use reverie::Errno;
use reverie::Pid;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use safeptrace::AnySignal;
use safeptrace::Error as TraceError;
use safeptrace::Stopped;
use tokio::sync::mpsc;

use super::breakpoint_inserted;
use super::breakpoint_removed;
use crate::gdbstub::BreakpointType;
use crate::gdbstub::CoreRegs;
use crate::gdbstub::GdbRequest;
use crate::gdbstub::Interrupt;
use crate::gdbstub::ResumeAction;
use crate::gdbstub::ResumeInferior;
use crate::gdbstub::StopEvent;
use crate::gdbstub::StopReason;
use crate::gdbstub::StoppedInferior;
use crate::gdbstub::ThreadState;
use crate::gdbstub::ThreadStateGuard;
use crate::gdbstub::ThreadView;

/// How often a thread waiting inside a Tool callback, whose tracee runs a
/// syscall for the callback, looks again whether it can stop for gdb's
/// interrupt there (`ParkedForGdb::hold`).
const IN_PLACE_RECHECK: std::time::Duration = std::time::Duration::from_millis(10);

/// A thread's ends of its gdb channels, where gdb's resume and requests
/// arrive while the thread is in a gdb stop.
///
/// Shared with [`ParkedForGdb`], which stops the thread inside a Tool
/// callback that holds the thread, so that a stop there and a stop the
/// callback itself makes (at a clone it injects, say) use the same channels.
/// The two never overlap: a stop inside a callback happens while the
/// callback is not polled.
#[derive(Clone)]
pub(super) struct GdbPorts(Arc<StdMutex<Option<Ports>>>);

pub(super) struct Ports {
    resume_rx: mpsc::Receiver<ResumeInferior>,
    request_rx: mpsc::Receiver<GdbRequest>,
}

impl GdbPorts {
    pub(super) fn new(
        resume_rx: mpsc::Receiver<ResumeInferior>,
        request_rx: mpsc::Receiver<GdbRequest>,
    ) -> Self {
        Self(Arc::new(StdMutex::new(Some(Ports {
            resume_rx,
            request_rx,
        }))))
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Ports>> {
        self.0
            .lock()
            .expect("should not be poisoned: no code panics while holding it")
    }

    /// The channels, for a gdb stop. `None` if a stop already has them.
    pub(super) fn take(&self) -> Option<Ports> {
        self.slot().take()
    }

    /// Returns the channels a gdb stop took.
    pub(super) fn put(&self, ports: Ports) {
        *self.slot() = Some(ports);
    }

    /// Whether no gdb stop has the channels.
    fn is_free(&self) -> bool {
        self.slot().is_some()
    }
}

/// Waits in a gdb stop, giving each of gdb's requests to `answer`, until gdb
/// resumes the thread. Returns gdb's resume, `None` if the resume channel
/// closed, and whether the session let the thread go without one, in which
/// case the resume is a detach.
///
/// The session drops the thread's stop channel (`stop_tx`) once gdb has let
/// the thread go: at a detach or a kill, which send a resume only to the
/// threads whose stops gdb has seen, and when gdb disconnects. No resume
/// comes after that, so the thread resumes as a detach resumes it, whether
/// it was waiting here already or arrives later.
pub(super) async fn serve_gdb_stop(
    ports: &mut Ports,
    stop_tx: Option<mpsc::Sender<StoppedInferior>>,
    mut answer: impl FnMut(GdbRequest),
) -> (Option<ResumeInferior>, bool) {
    let Ports {
        resume_rx,
        request_rx,
    } = ports;
    let mut resume_future = Box::pin(resume_rx.recv());
    let mut released = Box::pin(async move {
        match stop_tx {
            Some(stop_tx) => stop_tx.closed().await,
            None => future::pending().await,
        }
    });
    loop {
        let request_future = Box::pin(request_rx.recv());
        let resume_or_release = future::select(&mut resume_future, &mut released);
        match future::select(request_future, resume_or_release).await {
            Either::Left((Some(request), _)) => answer(request),
            Either::Left((None, _)) => {}
            Either::Right((Either::Left((resume, _)), _)) => break (resume, false),
            Either::Right((Either::Right(((), _)), _)) => {
                let detach = ResumeInferior {
                    action: ResumeAction::Continue(None),
                    detach: true,
                };
                break (Some(detach), true);
            }
        }
    }
}

/// The signal gdb resumed a thread with from an interrupt's stop, which the
/// thread is then sent: SIGKILL for a kill, the signal of `signal SIG`, none
/// for `continue`. gdb shows the interrupt as a SIGINT that the guest never
/// had, and passes it on, as `signal SIGINT` or `handle SIGINT pass`, only
/// when asked; a detach passes nothing.
pub(super) fn signal_after_interrupt(resume: Option<ResumeInferior>) -> Option<AnySignal> {
    let resume = resume?;
    match resume.action {
        ResumeAction::Continue(Some(sig)) if resume.detach => {
            (sig.as_nix() == Some(Signal::SIGKILL)).then_some(sig)
        }
        _ if resume.detach => None,
        ResumeAction::Continue(sig) | ResumeAction::Step(sig) => sig,
        ResumeAction::Stop | ResumeAction::StepUntil(..) => None,
    }
}

/// Sends `sig` to thread `tid` of process `pid`.
pub(super) fn send_signal(pid: Pid, tid: Pid, sig: AnySignal) {
    // SAFETY: a plain syscall with integer arguments.
    let sent = Errno::result(unsafe {
        libc::syscall(libc::SYS_tgkill, pid.as_raw(), tid.as_raw(), sig.raw())
    });
    if let Err(errno) = sent {
        tracing::debug!(%pid, %tid, %sig, %errno, "signal gdb resumed an interrupt with not sent");
    }
}

/// What gdb's requests act on: a stopped thread's tracee and the software
/// breakpoints of its address space. For a thread stopped inside a Tool
/// callback, also the registers it entered the callback with, which gdb is
/// shown instead of its live ones, and which it may not change: the callback
/// may be part way through injecting a syscall, and resumes the thread from
/// registers it keeps.
pub(super) struct GdbTracee<'a> {
    pub(super) task: &'a mut Stopped,
    pub(super) breakpoints: &'a StdMutex<HashMap<u64, u64>>,
    pub(super) parked_registers: Option<&'a CoreRegs>,
}

fn lock_breakpoints(
    breakpoints: &StdMutex<HashMap<u64, u64>>,
) -> std::sync::MutexGuard<'_, HashMap<u64, u64>> {
    breakpoints
        .lock()
        .expect("should not be poisoned: no code panics while holding it")
}

impl GdbTracee<'_> {
    /// Answers one of gdb's requests.
    pub(super) fn answer(&mut self, request: GdbRequest) {
        match request {
            GdbRequest::SetBreakpoint(bkpt, reply_tx) => {
                if bkpt.ty == BreakpointType::Software {
                    let _ = reply_tx.send(self.add_breakpoint(bkpt.addr));
                }
            }
            GdbRequest::RemoveBreakpoint(bkpt, reply_tx) => {
                if bkpt.ty == BreakpointType::Software {
                    let _ = reply_tx.send(self.remove_breakpoint(bkpt.addr));
                }
            }
            GdbRequest::ReadInferiorMemory(addr, length, reply_tx) => {
                let _ = reply_tx.send(self.read_memory(addr, length));
            }
            GdbRequest::WriteInferiorMemory(addr, length, data, reply_tx) => {
                let _ = reply_tx.send(self.write_memory(addr, length, data));
            }
            GdbRequest::ReadRegisters(reply_tx) => {
                let _ = reply_tx.send(self.read_registers());
            }
            GdbRequest::WriteRegisters(core_regs, reply_tx) => {
                let _ = reply_tx.send(self.write_registers(core_regs));
            }
        }
    }

    /// The software breakpoints of this task's address space.
    fn breakpoints(&self) -> std::sync::MutexGuard<'_, HashMap<u64, u64>> {
        lock_breakpoints(self.breakpoints)
    }

    fn add_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            let task = &mut *self.task;
            // The word read here can include the int3 of another live
            // breakpoint up to 7 bytes later. That is harmless: only the
            // lowest byte, this breakpoint's own, is ever restored.
            let saved_insn: u64 = task.read_value(bkpt_addr)?;
            // `Z0` must be idempotent (GDB remote protocol, "Z0"). Inserting again
            // would save the int3 as the original byte, so a later `z0` would
            // leave the breakpoint in the tracee for good. An entry can be
            // stale when something other than gdb rewrote the code, such as an
            // exec. Trust the entry only while the int3 is still in memory;
            // otherwise insert again, replacing the stale entry.
            let mut breakpoints = lock_breakpoints(self.breakpoints);
            if breakpoints.contains_key(&addr) && saved_insn & 0xff == 0xcc {
                return Ok(());
            }
            task.write_value(bkpt_addr, &breakpoint_inserted(saved_insn))?;
            breakpoints.insert(addr, saved_insn);
        }
        Ok(())
    }

    fn remove_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        let insn = lock_breakpoints(self.breakpoints)
            .remove(&addr)
            .ok_or(Errno::ENOENT)?;
        let task = &mut *self.task;
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            // Restore this breakpoint's byte only. Writing the whole saved
            // word back would also restore the 7 bytes after it as they were
            // at insertion, erasing the int3 of any breakpoint inserted there
            // since, such as GDB's breakpoint on a following short source
            // line when it steps over this one.
            let current: u64 = task.read_value(bkpt_addr)?;
            task.write_value(bkpt_addr, &breakpoint_removed(current, insn))?;
        }
        Ok(())
    }

    fn read_memory(&self, addr: u64, mut size: usize) -> Result<Vec<u8>, TraceError> {
        // NB: dont' trust size to be sane blindly.
        if size > 0x8000 {
            size = 0x8000;
        }

        let mut res = vec![0; size];
        if let Some(addr) = Addr::from_raw(addr as usize) {
            let nb = self.task.read(addr, &mut res)?;
            res.resize(nb, 0);
        }

        // There could be a software breakpoint within the address requested,
        // we should return the orignal contents without the breakpoint insn.
        // This is *not* documented in gdb remote protocol, however, both
        // gdbserver and rr does this. see:
        // rr: https://github.com/rr-debugger/rr/blob/master/src/GdbServer.cc#L561
        // gdbserver: https://github.com/bminor/binutils-gdb/blob/master/gdbserver/mem-break.cc#L1914
        for (bkpt, saved_insn) in self.breakpoints().iter() {
            if (addr..addr + res.len() as u64).contains(bkpt) {
                // This abuses bkpt insn 0xcc is single byte.
                res[*bkpt as usize - addr as usize] = *saved_insn as u8;
            }
        }

        Ok(res)
    }

    fn write_memory(&mut self, addr: u64, size: usize, data: Vec<u8>) -> Result<(), TraceError> {
        let size = std::cmp::min(size, data.len());
        let addr = AddrMut::from_raw(addr as usize).ok_or(Errno::EFAULT)?;
        self.task.write(addr, &data[..size])?;
        Ok(())
    }

    fn read_registers(&self) -> Result<CoreRegs, TraceError> {
        if let Some(regs) = self.parked_registers {
            return Ok(regs.clone());
        }
        let regs = self.task.getregs()?;
        let fpregs = self.task.getfpregs()?;
        Ok(CoreRegs::from_parts(regs, fpregs))
    }

    fn write_registers(&self, core_regs: CoreRegs) -> Result<(), TraceError> {
        if self.parked_registers.is_some() {
            return Err(Errno::EBUSY.into());
        }
        let (regs, fpregs) = core_regs.into_parts();
        self.task.setregs(&regs)?;
        self.task.setfpregs(&fpregs)?;
        Ok(())
    }
}

/// A thread held in a Tool callback while gdb is attached: it publishes the
/// registers it entered the callback with (`ThreadState::Parked`), which is
/// all gdb needs to show it, and stops there for gdb's interrupt.
///
/// It is made before the callback begins ([`TracedTask::park_for_gdb`]), from
/// what such a stop needs, since the callback then holds the thread; the
/// resume gdb chooses there is applied when the callback has returned
/// ([`TracedTask::unpark`]).
///
/// [`TracedTask::park_for_gdb`]: super::TracedTask::park_for_gdb
/// [`TracedTask::unpark`]: super::TracedTask::unpark
/// The signal gdb resumed a thread with from a stop made before the thread's
/// stop was handled, or inside a Tool callback, once sent to it: gdb chose
/// it, so its delivery is not reported to gdb again. Shared with the thread's
/// park, which sends it while the callback is still running.
#[derive(Clone, Debug, Default)]
pub(super) struct SentSignal(Arc<StdMutex<Option<AnySignal>>>);

impl SentSignal {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<AnySignal>> {
        self.0
            .lock()
            .expect("should not be poisoned: no code panics while holding it")
    }

    pub(super) fn set(&self, sig: AnySignal) {
        *self.slot() = Some(sig);
    }

    /// Whether `sig` is the signal sent, which then counts as delivered.
    pub(super) fn take_if(&self, sig: AnySignal) -> bool {
        let mut slot = self.slot();
        let sent = *slot == Some(sig);
        if sent {
            *slot = None;
        }
        sent
    }
}

pub(super) struct ParkedForGdb {
    pub(super) tid: Pid,
    pub(super) pid: Pid,
    pub(super) task: Stopped,
    pub(super) regs: libc::user_regs_struct,
    pub(super) core_regs: CoreRegs,
    pub(super) breakpoints: Arc<StdMutex<HashMap<u64, u64>>>,
    pub(super) view: ThreadView,
    pub(super) ports: GdbPorts,
    pub(super) stop_tx: Option<mpsc::Sender<StoppedInferior>>,
    pub(super) request_tx: Option<mpsc::Sender<GdbRequest>>,
    pub(super) resume_tx: Option<mpsc::Sender<ResumeInferior>>,
    pub(super) interrupt: Interrupt,
    pub(super) sent_signal: SentSignal,
    /// The Tool runs one thread at a time (`GdbServer::sequentialized_guest`):
    /// a thread whose turn has not come waits inside a callback, so it can
    /// stop for the interrupt there, and must hold while another is stopped
    /// for it. Otherwise the thread that takes the interrupt at its next
    /// ptrace stop freezes the others (`TracedTask::freeze_all`).
    pub(super) sequentialized: bool,
    /// gdb's resume from a stop made here, and whether the session let the
    /// thread go without one (`serve_gdb_stop`).
    pub(super) resumed: Option<(Option<ResumeInferior>, bool)>,
    /// Restores the thread's previous state, once the callback is done.
    pub(super) _parked: ThreadStateGuard,
}

impl ParkedForGdb {
    /// Runs `callback`, the Tool callback this thread is parked for, and
    /// stops the thread for gdb's interrupt at each point where it can do so
    /// without changing what the guest or the Tool sees, in a guest whose
    /// threads the Tool runs one at a time: before the callback begins; while
    /// it waits, if gdb interrupted the guest when no thread ran
    /// (`Interrupt::allow_in_place`); and once it has returned, before the
    /// thread resumes. While another thread is stopped for the interrupt, the
    /// thread holds at the first and last of these.
    pub(super) async fn hold<F: Future>(parked: &mut Option<Self>, callback: F) -> F::Output {
        let Some(parked) = parked.as_mut().filter(|parked| parked.sequentialized) else {
            return callback.await;
        };
        parked.interrupt_point("before the callback").await;
        let interrupt = parked.interrupt.clone();
        let output = {
            let mut callback = std::pin::pin!(callback);
            loop {
                let in_place = std::pin::pin!(interrupt.requested_in_place());
                match future::select(callback.as_mut(), in_place).await {
                    Either::Left((output, _)) => break output,
                    // Only while the tracee is in a ptrace stop, where gdb can
                    // read and write its memory.
                    Either::Right(((), _)) if parked.task.getregs().is_ok() => {
                        if parked.can_stop() && interrupt.take(parked.tid) {
                            tracing::debug!(
                                tid = %parked.tid,
                                "gdb's interrupt taken where the thread waits in a Tool callback"
                            );
                            parked.stop_for_interrupt().await;
                        }
                    }
                    // The callback has the tracee run a syscall, such as one
                    // that blocks. Look again shortly, running the callback
                    // meanwhile: the thread stops after it if it returns.
                    Either::Right(((), _)) => {
                        let later = std::pin::pin!(tokio::time::sleep(IN_PLACE_RECHECK));
                        if let Either::Left((output, _)) =
                            future::select(callback.as_mut(), later).await
                        {
                            break output;
                        }
                    }
                }
            }
        };
        parked.interrupt_point("after the callback").await;
        output
    }

    /// Takes a pending interrupt and stops for it, or, while another thread
    /// is stopped for one, holds until gdb resumes the guest.
    async fn interrupt_point(&mut self, point: &'static str) {
        if self.interrupt.is_requested() && self.can_stop() && self.interrupt.take(self.tid) {
            tracing::debug!(tid = %self.tid, point, "gdb's interrupt taken in a Tool callback");
            self.stop_for_interrupt().await;
        } else if self.interrupt.is_taken() {
            tracing::debug!(tid = %self.tid, point, "holding in a Tool callback for gdb's interrupt");
            self.interrupt.released().await;
        }
    }

    /// Whether the thread can serve a gdb stop: it has its channels, which a
    /// stop the callback made itself would hold, and gdb follows it.
    fn can_stop(&self) -> bool {
        self.stop_tx.is_some()
            && self.request_tx.is_some()
            && self.resume_tx.is_some()
            && self.ports.is_free()
    }

    /// Reports gdb's interrupt as this thread's SIGINT stop and waits in the
    /// gdb stop, answering gdb's requests, until gdb resumes it. The thread is
    /// shown where it entered the callback. Then sends the thread the signal
    /// gdb resumed it with, if any, at once: a kill cannot wait for the
    /// callback, which may never return. Called only if `can_stop`.
    async fn stop_for_interrupt(&mut self) {
        if let (Some(stop_tx), Some(request_tx), Some(resume_tx), Some(mut ports)) = (
            self.stop_tx.clone(),
            self.request_tx.clone(),
            self.resume_tx.clone(),
            self.ports.take(),
        ) {
            let stop = StoppedInferior {
                reason: StopReason::stopped(
                    self.tid,
                    self.pid,
                    StopEvent::Signal(Signal::SIGINT.into()),
                    self.regs.into(),
                ),
                request_tx,
                resume_tx,
                view: self.view.clone(),
                replied: None,
            };
            // A send fails once gdb has let the thread go; the wait below then
            // ends at once, as a detach.
            let _ = stop_tx.send(stop).await;
            let answering = self.view.enter(ThreadState::InGdbStop);
            let mut tracee = GdbTracee {
                task: &mut self.task,
                breakpoints: &self.breakpoints,
                parked_registers: Some(&self.core_regs),
            };
            let resumed =
                serve_gdb_stop(&mut ports, Some(stop_tx), |request| tracee.answer(request)).await;
            drop(answering);
            self.ports.put(ports);
            if let Some(sig) = signal_after_interrupt(resumed.0) {
                send_signal(self.pid, self.tid, sig);
                self.sent_signal.set(sig);
            }
            self.resumed = Some(resumed);
        }
        self.interrupt.release();
    }
}
