/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Real TRACEME/ptrace controls. No synthetic status, identity or source receipt.
include!("wait_owner_reachability_tests.rs");
use std::task::Wake;

use super::*;
use crate::Signal;

const COMPONENT: Duration = Duration::from_secs(3);
const CLEANUP: Duration = Duration::from_secs(2);

pub(super) struct SyncConsumedHook(Box<dyn FnOnce(Pid, i32) + Send>);

impl std::fmt::Debug for SyncConsumedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncConsumedHook")
    }
}

pub(super) fn after_sync_consumption(event: &Event, pid: Pid, raw: i32) {
    let hook = event.sync_consumed_hook.lock().take();
    if let Some(hook) = hook {
        // This is the original synchronous caller, after real consumption and
        // gate release. A test may run a real same-thread control here.
        hook.0(pid, raw);
    }
}

fn gettid() -> libc::pid_t {
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

struct NativeChild {
    terminal: TerminalCleanup,
    done: bool,
}

impl NativeChild {
    fn cleanup(&self, deadline: Instant) -> Result<(), Errno> {
        // Registration occurs only after the synchronous owner has returned.
        self.terminal.ensure_registered()?;
        if !self.terminal.is_reaped()? {
            match self.terminal.request_sigkill() {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error),
            }
        }
        if !self
            .terminal
            .wait(deadline.saturating_duration_since(Instant::now()))
        {
            return Err(Errno::ETIMEDOUT);
        }
        let terminal = self
            .terminal
            .observed_exit_status()?
            .ok_or(Errno::ENODATA)?;
        let parent = futures::executor::block_on(self.terminal.reap_parent_terminal())?;
        if !matches!(parent, ParentReap::Reaped | ParentReap::AlreadyReaped)
            || !self.terminal.is_reaped()?
        {
            return Err(Errno::EBUSY);
        }
        loop {
            let registered = NOTIFIER
                .pids
                .lock()
                .get(&self.terminal.pid)
                .is_some_and(|entry| entry.handle == self.terminal.event);
            if !registered {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Errno::ETIMEDOUT);
            }
            thread::sleep(Duration::from_millis(1));
        }
        eprintln!(
            "490 cleanup pid={}: actual_terminal={terminal:?}, worker=DONE, registry_retired=true, parent={parent:?}, original_pidfd_hup=true",
            self.terminal.pid
        );
        // Separate stronger control: even the immediate unregistered-success
        // path must satisfy the ORIGINAL total cleanup deadline.
        if Instant::now() > deadline {
            return Err(Errno::ETIMEDOUT);
        }
        Ok(())
    }

    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.terminal.event.event().sync_consumed_hook.lock().take();
        // No custom callback survives into cancellation/worker retirement.
        self.terminal
            .event
            .event()
            .status_waker
            .register(&futures::task::noop_waker());
        let start = Instant::now();
        let result = self.cleanup(start + CLEANUP);
        eprintln!(
            "490 cleanup result={result:?}, elapsed={:?}, bound={CLEANUP:?}",
            start.elapsed()
        );
        if result.is_err() || start.elapsed() > CLEANUP {
            eprintln!("490 exact custody/deadline failure; refusing to discard owner");
            std::process::abort();
        }
        self.done = true;
    }
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        self.finish();
    }
}

fn child() -> (Running, NativeChild, libc::pid_t) {
    let start = Instant::now();
    let ptracer = gettid();
    let parent = unsafe { libc::getpid() };
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                libc::_exit(121);
            }
            if libc::getppid() != parent {
                libc::_exit(122);
            }
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                libc::_exit(120);
            }
            if libc::raise(libc::SIGSTOP) != 0 || libc::raise(libc::SIGSTOP) != 0 {
                libc::_exit(123);
            }
            loop {
                libc::pause();
            }
        }
    }
    let running = Running::new(Pid::from_raw(pid));
    let custody = NativeChild {
        terminal: TerminalCleanup::new_unregistered(running.pid(), &running.1),
        done: false,
    };
    // Authentic registration/capture only; it does not claim a kernel waiter
    // or start a notifier. Running::wait still selects its actual SYNC owner.
    let handle = NOTIFIER
        .sync_handle(running.pid(), running.1.event())
        .unwrap();
    assert!(Arc::ptr_eq(handle.event(), custody.terminal.event.event()));
    assert!(start.elapsed() <= Duration::from_secs(10), "setup bound");
    (running, custody, ptracer)
}

fn sync_wait(running: Running, custody: &NativeChild) -> Wait {
    let deadline = Instant::now() + COMPONENT;
    let cancel = TerminalCleanup::new_unregistered(running.pid(), &running.1);
    assert!(cancel.same_generation(&custody.terminal));
    let (release, wait) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let watchdog = scope.spawn(move || match wait.recv_timeout(COMPONENT) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let signal = cancel.request_sigkill();
                eprintln!("490 synchronous component TIMEOUT: original pidfd signal={signal:?}");
                (true, signal)
            }
            _ => (false, Ok(())),
        });
        let result = running.wait();
        drop(release);
        let (timed_out, signal) = watchdog.join().unwrap();
        assert!(
            !timed_out,
            "real synchronous component timed out: {signal:?}"
        );
        assert!(Instant::now() <= deadline, "synchronous component bound");
        result.expect("real synchronous wait")
    })
}

fn worker_wait(running: Running) -> Wait {
    let deadline = Instant::now() + COMPONENT;
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(running.wait_owned());
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result.expect("real original notifier wait");
        }
        assert!(Instant::now() <= deadline, "notifier component bound");
        thread::sleep(Duration::from_millis(1));
    }
}

fn stopped(wait: Wait, custody: &NativeChild) -> Stopped {
    let (stopped, event) = wait.assume_stopped();
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
    assert_eq!(stopped.pid(), custody.terminal.pid);
    assert!(Arc::ptr_eq(
        stopped.1.event().event(),
        custody.terminal.event.event()
    ));
    stopped
}

fn authenticate(stopped: &Stopped, ptracer: libc::pid_t) {
    assert_eq!(gettid(), ptracer);
    let source = stopped.source_stop().expect("genuine new source");
    source.validate_current().unwrap();
    let acquisition = source.begin_acquisition().expect("genuine acquisition");
    acquisition.with_stopped(|s| s.getregs()).unwrap().unwrap();
    acquisition.finish_binding();
    source.validate_current().unwrap();
}

fn same_value_control(pid: Pid, raw: i32, ptracer: libc::pid_t) -> Result<(), String> {
    assert_eq!(
        gettid(),
        ptracer,
        "physically valid original ptracer control"
    );
    assert_eq!(raw, (libc::SIGSTOP << 8) | 0x7f);
    let alias = Wait::from_raw(pid, raw).unwrap().assume_stopped().0;
    assert_eq!(alias.source_stop().map(|_| ()), Err(Errno::ENODATA));
    let regs = alias.getregs().map_err(|e| format!("GETREGSET: {e:?}"))?;
    alias
        .setregs(&regs)
        .map_err(|e| format!("SETREGSET: {e:?}"))?;
    let after = alias.getregs().map_err(|e| format!("readback: {e:?}"))?;
    assert_eq!(format!("{regs:?}"), format!("{after:?}"));
    eprintln!(
        "490 actual same-value SETREGSET pid={pid}, original_ptracer_tid={ptracer}, control_tid={}",
        gettid()
    );
    Ok(())
}

#[test]
fn native_next_genuine_stop_authenticates_after_raw_invalidation() {
    let (running, mut custody, ptracer) = child();
    custody.terminal.ensure_registered().unwrap();
    let first = stopped(worker_wait(running), &custody);
    let source = first.source_stop().unwrap();
    same_value_control(first.pid(), (libc::SIGSTOP << 8) | 0x7f, ptracer).unwrap();
    assert_eq!(source.validate_current(), Err(Errno::ESTALE));
    let next = stopped(worker_wait(first.resume_retaining(None).unwrap()), &custody);
    authenticate(&next, ptracer);
    custody.finish();
}

#[test]
fn native_sync_unchanged_consumption_authenticates() {
    let (running, mut custody, ptracer) = child();
    let stopped = stopped(sync_wait(running, &custody), &custody);
    assert_eq!(
        custody
            .terminal
            .event
            .event()
            .worker_state
            .load(Ordering::Acquire),
        WORKER_NOT_STARTED
    );
    authenticate(&stopped, ptracer);
    custody.finish();
}

#[test]
fn native_sync_consumption_gap_same_thread_setregs_refuses_source() {
    let (running, mut custody, ptracer) = child();
    let observed = Arc::new(Mutex::new(None));
    let record = Arc::clone(&observed);
    let weak = Arc::downgrade(custody.terminal.event.event());
    *custody.terminal.event.event().sync_consumed_hook.lock() =
        Some(SyncConsumedHook(Box::new(move |pid, raw| {
            let event = weak.upgrade().unwrap();
            assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_SYNC);
            assert_eq!(
                event.worker_state.load(Ordering::Acquire),
                WORKER_NOT_STARTED
            );
            assert!(
                event.status.lock().pending.is_empty(),
                "actual consumed status not yet published"
            );
            assert!(
                event.source.try_lock().is_some(),
                "consume gate released before callback"
            );
            *record.lock() = Some(same_value_control(pid, raw, ptracer));
        })));
    let stopped = stopped(sync_wait(running, &custody), &custody);
    let refusal = stopped.source_stop().map(|_| ());
    let actual_control = observed.lock().take();
    custody.finish();
    assert_eq!(actual_control, Some(Ok(())));
    assert_eq!(refusal, Err(Errno::EPERM));
}

struct ReentrantControl {
    event: Weak<Event>,
    pid: Pid,
    ptracer: libc::pid_t,
    fired: AtomicBool,
    result: Mutex<Option<(bool, Result<(), String>)>>,
}

impl Wake for ReentrantControl {
    fn wake(self: Arc<Self>) {
        if self.fired.swap(true, Ordering::AcqRel) {
            return;
        }
        let event = self.event.upgrade().unwrap();
        let raw = event
            .status
            .try_lock()
            .and_then(|state| state.pending.entries.front().map(|e| e.raw));
        let locks_free = raw.is_some()
            && event.source.try_lock().is_some()
            && NOTIFIER.pids.try_lock().is_some();
        let control = if locks_free {
            same_value_control(self.pid, raw.unwrap(), self.ptracer)
        } else {
            Err("publication held a status/source/registry lock across wake".to_owned())
        };
        *self.result.lock() = Some((locks_free, control));
    }
}

#[test]
fn native_sync_publication_waker_reenters_actual_same_thread_control() {
    let (running, mut custody, ptracer) = child();
    let callback = Arc::new(ReentrantControl {
        event: Arc::downgrade(custody.terminal.event.event()),
        pid: running.pid(),
        ptracer,
        fired: AtomicBool::new(false),
        result: Mutex::new(None),
    });
    custody
        .terminal
        .event
        .event()
        .status_waker
        .register(&Waker::from(Arc::clone(&callback)));
    let stopped = stopped(sync_wait(running, &custody), &custody);
    let refusal = stopped.source_stop().map(|_| ());
    let observed = callback.result.lock().take();
    custody.finish();
    assert_eq!(
        observed,
        Some((true, Ok(()))),
        "actual reentrant same-thread control"
    );
    assert_eq!(refusal, Err(Errno::EPERM));
}
