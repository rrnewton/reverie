/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// Native registration controls. No registry, owner, identity or status writes.
use nix::sys::signal::Signal;

use super::*;
use crate::ExitStatus;

const SETUP: Duration = Duration::from_secs(10);
const HANDOFF: Duration = Duration::from_secs(3);
const CLEANUP: Duration = Duration::from_secs(2);

static CAPTURE_PAUSES: LazyLock<Mutex<HashMap<Pid, BoundedTestPause>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn pause_capture(pid: Pid) {
    // Drop the installation lock before pausing: the original ptracer must
    // capture the same real child while this first snapshot is retained.
    let pause = CAPTURE_PAUSES.lock().remove(&pid);
    if let Some(pause) = pause {
        let _ = pause.captured.send(());
        let _ = pause.resume.recv_timeout(SETUP);
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn require<T>(value: Option<T>, message: &str) -> T {
    value.unwrap_or_else(|| {
        eprintln!("531 FAILED CUSTODY: {message}; no qualifying evidence");
        std::process::abort();
    })
}

struct Task<T> {
    thread: JoinHandle<()>,
    result: mpsc::Receiver<T>,
}

impl<T: Send + 'static> Task<T> {
    fn spawn(f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (send, result) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = send.send(f());
        });
        Self { thread, result }
    }

    fn join(self, value: T, deadline: Instant) -> T {
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        require(self.thread.is_finished().then_some(()), "thread join bound");
        require(self.thread.join().ok(), "thread panicked");
        value
    }

    fn receive(self, deadline: Instant) -> T {
        let value = require(
            self.result.recv_timeout(remaining(deadline)).ok(),
            "task return bound",
        );
        self.join(value, deadline)
    }

    fn stop_registrar(self, deadline: Instant) -> T {
        // Force an actual capture-error return on this exact thread. This
        // unwinds the real provisional claim; it does not edit wait authority.
        CAPTURE_THREAD_ERRORS
            .lock()
            .entry(self.thread.thread().id())
            .or_default()
            .push_back(Errno::EMFILE);
        self.receive(deadline)
    }
}

struct Child {
    pid: Pid,
    birth: OwnedFd,
    ptracer: libc::pid_t,
    done: bool,
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.done {
            eprintln!(
                "531 FAILED CUSTODY: original child {} not settled",
                self.pid
            );
            std::process::abort();
        }
    }
}

fn child() -> Child {
    let start = Instant::now();
    let mut gate = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(gate.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let parent = unsafe { libc::getpid() };
    let ptracer = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
    let raw = unsafe { libc::fork() };
    assert!(raw >= 0);
    if raw == 0 {
        unsafe {
            libc::close(gate[1]);
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 || libc::getppid() != parent
            {
                libc::_exit(91);
            }
            let mut byte = 0u8;
            if libc::read(gate[0], std::ptr::from_mut(&mut byte).cast(), 1) != 1 {
                libc::_exit(92);
            }
            libc::close(gate[0]);
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                libc::_exit(93);
            }
            libc::raise(libc::SIGSTOP);
            libc::raise(libc::SIGSTOP);
            loop {
                libc::pause();
            }
        }
    }
    unsafe {
        libc::close(gate[0]);
    }
    let pid = Pid::from_raw(raw);
    let birth = require(open_thread_pidfd_kernel(pid).ok(), "birth pidfd open");
    let child = Child {
        pid,
        birth,
        ptracer,
        done: false,
    };
    let byte = 1u8;
    assert_eq!(
        unsafe { libc::write(gate[1], std::ptr::from_ref(&byte).cast(), 1) },
        1
    );
    unsafe {
        libc::close(gate[1]);
    }
    loop {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        if status.lines().any(|line| line.starts_with("State:\tt")) {
            assert_eq!(
                worker_status_pid(&status, "TracerPid:").unwrap().as_raw(),
                ptracer
            );
            break;
        }
        require(
            (start.elapsed() < SETUP).then_some(()),
            "initial actual stop setup",
        );
        thread::yield_now();
    }
    child
}

fn stopped(wait: Wait, child: &Child) -> Stopped {
    match wait {
        Wait::Stopped(stopped, crate::Event::Signal(Signal::SIGSTOP)) => {
            assert_eq!(stopped.pid(), child.pid);
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t,
                child.ptracer
            );
            stopped
                .getregs()
                .expect("GETREGS on original ptracer and real SIGSTOP");
            stopped
        }
        other => panic!("actual SIGSTOP required: {other:?}"),
    }
}

fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(&futures::task::noop_waker()))
}

fn registrar(
    pid: Pid,
    current: bool,
) -> Task<(OwnedWaitFuture, Poll<Result<Wait, OwnedWaitError>>)> {
    Task::spawn(move || {
        let mut future = if current {
            // The ptracer has already consumed this child's actual stop.
            Stopped::try_new_current_unchecked(pid)
                .unwrap()
                .wait_owned()
        } else {
            Running::new(pid).wait_owned()
        };
        let result = poll_once(&mut future);
        (future, result)
    })
}

fn finish(child: &mut Child, second: Stopped, cleanup: &TerminalCleanup, deadline: Instant) {
    let event = cleanup.event.event();
    let has_worker = event.worker_state.load(Ordering::Acquire) == WORKER_RUNNING;
    if !has_worker {
        assert_eq!(
            event.worker_state.load(Ordering::Acquire),
            WORKER_NOT_STARTED
        );
        assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_NONE);
    }
    // Only after all registrar/synchronous threads have joined. Resume on the
    // original ptracer and retain the actual Running capability through death.
    let running = second.resume_retaining(None).unwrap();
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                child.birth.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        },
        0
    );
    if has_worker {
        assert!(
            cleanup.wait(remaining(deadline)),
            "original worker retirement"
        );
    } else {
        let mut fd = libc::pollfd {
            fd: child.birth.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut fd, 1, remaining(deadline).as_millis() as i32) },
            1
        );
        assert_ne!(fd.revents & libc::POLLIN, 0);
    }
    let actual = running.wait().unwrap().assume_exited();
    assert_eq!(
        actual,
        (child.pid, ExitStatus::Signaled(Signal::SIGKILL, false))
    );
    assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_DONE);
    while NOTIFIER.pids.lock().contains_key(&child.pid) {
        require(
            (Instant::now() < deadline).then_some(()),
            "registry retirement",
        );
        thread::yield_now();
    }
    let mut parent = Box::pin(cleanup.reap_parent_terminal());
    let parent = loop {
        if let Poll::Ready(result) = poll_once(&mut parent) {
            break result.unwrap();
        }
        require(
            (Instant::now() < deadline).then_some(()),
            "real-parent reap",
        );
        thread::yield_now();
    };
    assert!(matches!(
        parent,
        ParentReap::Reaped | ParentReap::AlreadyReaped
    ));
    let mut fd = libc::pollfd {
        fd: child.birth.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut fd, 1, 0) }, 1);
    assert_ne!(fd.revents & libc::POLLHUP, 0, "original birth pidfd HUP");
    assert_eq!(cleanup.observed_exit_status(), Ok(Some(actual.1)));
    assert!(Instant::now() < deadline, "one total cleanup bound");
    child.done = true;
    eprintln!(
        "531 CUSTODY pid={}: actual={:?}, worker=DONE, registry_absent=true, parent={parent:?}, birth_pidfd_HUP=true",
        child.pid, actual.1
    );
}

fn released(event: Weak<Event>, identity: Weak<WorkerIdentity>, deadline: Instant) {
    while event.strong_count() != 0 || identity.strong_count() != 0 {
        require(
            (Instant::now() < deadline).then_some(()),
            "zero Event/identity owners",
        );
        thread::yield_now();
    }
    assert!(Instant::now() < deadline, "total ownership cleanup bound");
    eprintln!("531 RELEASE: Event=0, WorkerIdentity=0 strong owners");
}

fn after_sync(current: bool) {
    let mut child = child();
    let running = Running::new(child.pid);
    let cleanup = TerminalCleanup::new_unregistered(child.pid, &running.1);
    let first = stopped(running.wait().unwrap(), &child);
    let event = Arc::downgrade(cleanup.event.event());
    let identity = Arc::downgrade(cleanup.event.identity().unwrap());
    assert_eq!(
        cleanup.event.event().worker_state.load(Ordering::Acquire),
        WORKER_NOT_STARTED
    );
    assert_eq!(
        cleanup.event.event().wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NONE
    );
    let task = registrar(child.pid, current);
    let outcome = task.result.recv_timeout(HANDOFF);
    let timely = outcome.is_ok();
    let deadline = Instant::now() + CLEANUP;
    let (future, polled) = match outcome {
        Ok(value) => task.join(value, deadline),
        Err(_) => task.stop_registrar(deadline),
    };
    let expected = if timely {
        polled.is_pending()
    } else {
        matches!(
            polled,
            Poll::Ready(Err(OwnedWaitError::Errno(Errno::EMFILE)))
        )
    };
    // Public synchronous wait joins a worker if present, otherwise consumes
    // itself. The first stop was already consumed, so this is the actual second.
    let second = stopped(
        first.resume_retaining(None).unwrap().wait().unwrap(),
        &child,
    );
    let workers = SPAWN_WORKER_COUNTS
        .lock()
        .get(&child.pid)
        .copied()
        .unwrap_or(0);
    finish(&mut child, second, &cleanup, deadline);
    drop(future);
    drop(polled);
    drop(cleanup);
    released(event, identity, deadline);
    eprintln!(
        "531 AFTER_SYNC current={current}, timely={timely}, expected_poll={expected}, workers={workers}"
    );
    assert!(
        expected,
        "old stop returned twice or wrong capture cleanup error"
    );
    assert!(
        timely,
        "public registration exceeded original 3s handoff, after exact cleanup"
    );
    assert_eq!(workers, 1, "exactly one committed notifier worker");
}

#[test]
fn current_alias_after_sync_consumption() {
    after_sync(true);
}

#[test]
fn fresh_registration_after_sync_consumption() {
    after_sync(false);
}

#[test]
fn delayed_capture_adoption_spawn_failure_reinsertion() {
    let mut child = child();
    let delayed = Running::new(child.pid);
    let exit = delayed.exit_event();
    let old_event = Arc::downgrade(delayed.1.event().event());
    let (captured, paused) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    CAPTURE_PAUSES
        .lock()
        .insert(child.pid, BoundedTestPause { captured, resume });
    let sync = Task::spawn(move || delayed.wait());
    require(
        paused.recv_timeout(SETUP).ok(),
        "real delayed first proc snapshot",
    );
    let first = stopped(Running::new(child.pid).wait().unwrap(), &child);
    let cleanup = TerminalCleanup::new_unregistered(child.pid, &first.1);
    let event = Arc::downgrade(cleanup.event.event());
    let identity = Arc::downgrade(cleanup.event.identity().unwrap());
    SPAWN_WORKER_ERRORS.lock().insert(child.pid, libc::EAGAIN);
    let spawn = Task::spawn(move || {
        let mut exit = exit;
        let result = poll_once(&mut exit);
        (exit, result)
    });
    let spawn_outcome = spawn.result.recv_timeout(HANDOFF);
    let reached = spawn_outcome.is_ok();
    let spawn_deadline = Instant::now() + CLEANUP;
    let (exit, spawn_result) = match spawn_outcome {
        Ok(value) => spawn.join(value, spawn_deadline),
        Err(_) => spawn.stop_registrar(spawn_deadline),
    };
    let eagain = matches!(spawn_result, Poll::Ready(Err(Error::Errno(Errno::EAGAIN))));
    let rolled_back = !NOTIFIER.pids.lock().contains_key(&child.pid)
        && cleanup.event.event().worker_state.load(Ordering::Acquire) == WORKER_NOT_STARTED
        && cleanup.event.event().wait_owner.load(Ordering::Acquire) == WAIT_OWNER_NONE;
    let adopted = Arc::ptr_eq(exit.event.event(), cleanup.event.event());
    eprintln!(
        "531 SPAWN: reached={reached}, result={spawn_result:?}, adopted={adopted}, rollback={rolled_back}"
    );
    // Supply the genuine second stop to the delayed synchronous waiter.
    drop(release);
    let resumed = first.resume_retaining(None).unwrap();
    let second = stopped(sync.receive(Instant::now() + HANDOFF).unwrap(), &child);
    drop(resumed);
    let registry_alias = {
        let pids = NOTIFIER.pids.lock();
        let entry = pids.get(&child.pid).unwrap();
        assert!(Arc::ptr_eq(entry.handle.event(), cleanup.event.event()));
        entry.handle != entry.handle.resolved_handle()
    };
    assert_eq!(
        cleanup.event.event().wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NONE
    );
    assert!(
        cleanup.event.event().status.lock().pending.is_empty(),
        "both real stops consumed once"
    );
    let task = registrar(child.pid, false);
    let outcome = task.result.recv_timeout(HANDOFF);
    let timely = outcome.is_ok();
    let deadline = Instant::now() + CLEANUP;
    let (future, polled) = match outcome {
        Ok(value) => task.join(value, deadline),
        Err(_) => task.stop_registrar(deadline),
    };
    let expected = if timely {
        polled.is_pending()
    } else {
        matches!(
            polled,
            Poll::Ready(Err(OwnedWaitError::Errno(Errno::EMFILE)))
        )
    };
    let workers = SPAWN_WORKER_COUNTS
        .lock()
        .get(&child.pid)
        .copied()
        .unwrap_or(0);
    finish(&mut child, second, &cleanup, deadline);
    drop(future);
    drop(polled);
    drop(exit);
    drop(spawn_result);
    drop(cleanup);
    released(event, identity, deadline);
    require(
        (old_event.strong_count() == 0).then_some(()),
        "provisional Event release",
    );
    eprintln!(
        "531 REINSERTION: EAGAIN={eagain}, registry_alias={registry_alias}, timely={timely}, expected_poll={expected}, workers={workers}"
    );
    assert!(
        reached && eagain && adopted && rolled_back,
        "required original EAGAIN sequence not reached"
    );
    assert!(
        expected,
        "second stop returned twice or wrong cleanup error"
    );
    assert!(
        timely,
        "post-EAGAIN registration exceeded original 3s handoff, after exact cleanup"
    );
    assert_eq!(workers, 1, "exactly one committed notifier worker");
    assert!(!registry_alias, "registry retained a forwarding alias");
}
