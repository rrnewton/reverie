// Included inside terminal_cleanup::tests with the authenticated broker adapter.
#[test]
fn dropped_waiter_keeps_native_wait_and_owned_service_alive() {
    const TEST: &str =
        "terminal_cleanup::tests::dropped_waiter_keeps_native_wait_and_owned_service_alive";
    let Some(client) = crate::broker_library_tests::selected_child(TEST) else {
        return;
    };
    dropped_waiter_keeps_native_wait_and_owned_service_alive_body(client);
}

#[test]
fn a_cancelled_waiters_waker_is_dropped_on_the_file_exit_thread_without_rust_min_stack() {
    const TEST: &str = concat!(
        "terminal_cleanup::tests::",
        "a_cancelled_waiters_waker_is_dropped_on_the_file_exit_thread_without_rust_min_stack"
    );
    // The child runs without RUST_MIN_STACK, as a production process does, so
    // the service thread has exactly the stack it was spawned with.
    let Some(client) = crate::broker_library_tests::selected_child_without(TEST, &["RUST_MIN_STACK"])
    else {
        return;
    };
    a_cancelled_waiters_waker_is_dropped_on_the_file_exit_thread_body(client);
}

use super::*;

/// The stack the waker's state needs to drop: more than Rust's 2 MiB default
/// for a spawned thread, as an executor's task, or the Tool state that a task
/// owns, could need.
const DEEP_DROP_STACK_BYTES: usize = 4 << 20;

/// Which threads woke a [`DeepWaker`] and dropped its state.
#[derive(Default)]
struct WakerEvents {
    woken_on: Option<String>,
    dropped_on: Option<String>,
}

fn thread_name() -> Option<String> {
    Some(std::thread::current().name().unwrap_or_default().to_owned())
}

/// A waiter's waker whose state needs [`DEEP_DROP_STACK_BYTES`] of stack to
/// drop. It records which thread woke it and which dropped it.
struct DeepWaker(Arc<Mutex<WakerEvents>>);

impl std::task::Wake for DeepWaker {
    fn wake(self: Arc<Self>) {
        self.0.lock().unwrap().woken_on = thread_name();
    }
}

impl Drop for DeepWaker {
    fn drop(&mut self) {
        std::hint::black_box(use_stack(DEEP_DROP_STACK_BYTES));
        self.0.lock().unwrap().dropped_on = thread_name();
    }
}

/// Uses `bytes` of stack through a chain of 64 KiB frames.
#[inline(never)]
fn use_stack(bytes: usize) -> u8 {
    const FRAME: usize = 64 * 1024;
    let mut frame = [0u8; FRAME];
    std::hint::black_box(&mut frame);
    let below = if bytes > FRAME {
        use_stack(bytes - FRAME)
    } else {
        0
    };
    std::hint::black_box(frame[usize::from(below) % FRAME]) ^ below
}

// The guest's waiter registers its waker with the task's terminal cleanup and
// is then cancelled: its future, its own waker and the task's cleanup owner
// all go. The service's next wake then consumes the registered waker, the last
// reference to its state, on the service's own thread.
fn a_cancelled_waiters_waker_is_dropped_on_the_file_exit_thread_body(
    client: crate::native_exit_broker::BrokerClient,
) {
    use std::future::Future;
    use std::task::Context;
    let runs = Arc::new(Mutex::new(crate::executor::AbandonedRuns::default()));
    let admission = crate::executor::RunAdmission::begin(&runs).unwrap();
    let mut task = admission.terminal_factory(client).spawn().unwrap();
    let events = Arc::new(Mutex::new(WakerEvents::default()));
    {
        let waker = std::task::Waker::from(Arc::new(DeepWaker(events.clone())));
        let mut context = Context::from_waker(&waker);
        let mut waiter = Box::pin(task.ready());
        // Pending or ready, polling leaves the waker registered until the
        // service's next wake.
        let _ = waiter.as_mut().poll(&mut context);
    }
    assert!(
        events.lock().unwrap().dropped_on.is_none(),
        "cancelling the waiter dropped the registered waker"
    );
    let shared = task.shared.clone();
    drop(task); // Cancellation: the run's reaper now owns the service thread.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while events.lock().unwrap().dropped_on.is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the service never woke the registered waker"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    {
        let events = events.lock().unwrap();
        assert_eq!(
            events.woken_on.as_deref(),
            Some("kvm-task-file-exit"),
            "the service did not wake the registered waker"
        );
        assert_eq!(
            events.dropped_on.as_deref(),
            Some("kvm-task-file-exit"),
            "the registered waker's last reference was not dropped by the service's wake"
        );
    }
    {
        let state = shared.lock();
        assert!(state.failure.is_none(), "service failure: {:?}", state.failure);
        assert!(
            matches!(state.result, Some(Ok(()))),
            "the cancelled service did not complete"
        );
    }
    let workers = Arc::new(crate::executor::EmptyWorkers);
    crate::executor::AbandonedRuns::retire(&runs, &workers);
    drop(shared);
    admission.finish(&*workers);
}

// This body runs inside terminal_cleanup's cfg(test) module and observes its
// private Shared state. Broker identity comes from the authenticated session.
fn dropped_waiter_keeps_native_wait_and_owned_service_alive_body(
    client: crate::native_exit_broker::BrokerClient,
) {
    use std::future::Future;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::IntoRawFd;
    use std::task::Context;
    use std::task::Poll;
    // This exact daemon identity/pidfd arrives with the authenticated session,
    // not from an environment PID or the launcher's SO_PEERPIDFD.
    let identity = client
        .authenticated_broker_identity()
        .expect("complete authenticated broker identity required by STOP control");
    let broker_pid = identity.pid();
    let broker_pidfd = identity.pidfd().try_clone_to_owned().unwrap();
    let runs = Arc::new(Mutex::new(crate::executor::AbandonedRuns::default()));
    let admission = crate::executor::RunAdmission::begin(&runs).unwrap();
    let mut task = admission.terminal_factory(client).spawn().unwrap();
    futures::executor::block_on(task.ready()).unwrap();
    // This is an owned-test daemon stop only. It keeps the worker's actual wait
    // receipt unavailable while we drop the waiter; no test-only product hook.
    fn signal(fd: &std::os::fd::OwnedFd, signal: i32) -> libc::c_long {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        }
    }
    fn assert_live(fd: &std::os::fd::OwnedFd) {
        let mut interest = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut interest, 1, 0) },
            0,
            "authenticated broker exited or pidfd observation failed"
        );
        assert_eq!(interest.revents, 0);
    }
    struct Resume {
        fd: std::os::fd::OwnedFd,
        armed: bool,
    }
    impl Resume {
        fn resume(mut self) {
            assert_eq!(signal(&self.fd, libc::SIGCONT), 0);
            self.armed = false;
        }
    }
    impl Drop for Resume {
        fn drop(&mut self) {
            if self.armed {
                let _ = signal(&self.fd, libc::SIGCONT);
            }
        }
    }
    let resume = Resume {
        fd: broker_pidfd,
        armed: true,
    };
    assert_live(&resume.fd);
    assert_eq!(signal(&resume.fd, libc::SIGSTOP), 0);
    // The native parent remains the trusted launcher. Inspect the exact
    // retained broker generation until STOP is effective before submission.
    // This observer borrows /proc metadata, never owns a socket reference.
    let stopped_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let original_stat = std::fs::read_to_string(format!("/proc/{broker_pid}/stat")).unwrap();
    let original_fields: Vec<_> = original_stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    let generation: u64 = original_fields[19].parse().unwrap();
    loop {
        assert_live(&resume.fd);
        let stat = std::fs::read_to_string(format!("/proc/{broker_pid}/stat")).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        assert_eq!(fields[19].parse::<u64>().unwrap(), generation);
        // Exclude a retired/reused numeric PID on either side of the read.
        assert_live(&resume.fd);
        if fields[0] == "T" {
            break;
        }
        assert!(
            std::time::Instant::now() < stopped_by,
            "owned broker did not acknowledge STOP"
        );
        unsafe {
            libc::poll(std::ptr::null_mut(), 0, 1);
        }
    }
    let (socket, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let shared = task.shared.clone();
    task.submit(vec![crate::native_exit_broker::SocketReference::Owned(
        unsafe { std::fs::File::from_raw_fd(socket.into_raw_fd()) },
    )]);
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    let mut waiter = Box::pin(task.finish());
    assert!(matches!(waiter.as_mut().poll(&mut context), Poll::Pending));
    drop(waiter); // Cancellation must not own or cancel the terminal batch.
    drop(task); // The actual prestarted reaper now owns the join handle.
    assert!(matches!(
        crate::executor::AbandonedRuns::admit(&runs),
        Err(crate::Error::AbandonedRunNotRetired)
    ));
    assert!(
        shared.lock().result.is_none(),
        "stopped broker cannot supply native wait"
    );
    resume.resume();
    futures::executor::block_on(std::future::poll_fn(|cx| {
        shared.waker.register(cx.waker());
        let state = shared.lock();
        if let Some(error) = &state.failure {
            panic!("retained service failure: {error:?}");
        }
        if let Some(result) = &state.result {
            assert!(result.is_ok());
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    let mut byte = [0u8];
    assert_eq!(std::io::Read::read(&mut peer, &mut byte).unwrap(), 0);
    let workers = Arc::new(crate::executor::EmptyWorkers);
    crate::executor::AbandonedRuns::retire(&runs, &workers);
    drop(shared);
    drop(peer);
    admission.finish(&*workers);
    // Final process cleanup must use the launcher's actual daemon wait and
    // owned-thread census. State.result alone is NOT a host-join receipt.
}
