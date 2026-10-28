/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! Actual common Tool-entry controls with native-mechanism test callbacks.
//! These exercise real lifecycle handlers and real RpcServer exchanges, not
//! a scheduler, native C loader, private image, or physical guest.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

use super::super::native_test_support as fixture;
use super::super::native_test_support::Callback;
use super::super::native_test_support::Remote;
use super::super::native_test_support::Reply;
use super::super::native_test_support::Request;
use super::super::native_test_support::Runtime;
use super::super::native_test_support::Server;
use super::*;

#[derive(Default, Serialize, Deserialize)]
struct State {
    seed: u64,
    starts: u64,
    posts: u64,
    syscalls: u64,
    order: Vec<u64>,
}
impl State {
    fn seeded() -> Self {
        Self {
            seed: 71,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct Probe {
    saved_stack: AtomicUsize,
    syscall_entries: AtomicUsize,
}

#[reverie::tool]
impl Tool for Probe {
    type GlobalState = Remote;
    type ThreadState = State;

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        assert_eq!(
            guest.thread_state().seed,
            71,
            "original initialized state was replaced"
        );
        assert_eq!(guest.thread_state().starts, 0);
        assert_eq!(guest.thread_state().posts, 0);
        assert_eq!(guest.config(), &71);
        assert_eq!(guest.read_clock()?, 17);
        assert_eq!(guest.regs().await.rip, 0x1234);
        assert!(guest.has_cpuid_interception());
        guest.thread_state_mut().starts += 1;
        guest.thread_state_mut().order.push(1);
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        assert_eq!(guest.thread_state().starts, 1);
        assert_eq!(guest.thread_state().posts, 0);
        guest.thread_state_mut().posts += 1;
        guest.thread_state_mut().order.push(2);
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(guest.thread_state().starts, 1);
        self.syscall_entries.fetch_add(1, Ordering::SeqCst);
        guest.thread_state_mut().syscalls += 1;
        guest.thread_state_mut().order.push(3);
        let (_, args) = call.into_parts();
        match args.arg0 as u64 {
            7 => {
                let mut stack = guest.stack().await?;
                let saved = stack.push(0x7197_u64).as_raw();
                self.saved_stack.store(saved, Ordering::SeqCst);
                drop(stack.commit()?);
                guest.tail_inject(call).await
            }
            8 => Err(Errno::ERESTARTSYS.into()),
            value @ (100 | 101 | 500) => {
                let Reply(from, answer) = guest.send_rpc(Request(value)).await;
                assert_eq!(from, guest.tid().as_raw());
                assert_eq!(answer, value);
                Ok(answer as i64)
            }
            200 => {
                // Exercise the actual native poll/wake entry, independently
                // from the synchronous socket-progress control below.
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut worker = None;
                let value = std::future::poll_fn(|context| {
                    if worker.is_none() {
                        let sender = sender.clone();
                        let waker = context.waker().clone();
                        worker = Some(std::thread::spawn(move || {
                            sender.send(200).unwrap();
                            waker.wake();
                        }));
                        return std::task::Poll::Pending;
                    }
                    match receiver.try_recv() {
                        Ok(value) => std::task::Poll::Ready(value),
                        Err(std::sync::mpsc::TryRecvError::Empty) => std::task::Poll::Pending,
                        Err(error) => panic!("wake worker failed: {error}"),
                    }
                })
                .await;
                worker.unwrap().join().unwrap();
                Ok(value)
            }
            _ => Ok(guest.inject(call).await?),
        }
    }
    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<reverie::RdtscResult, Errno> {
        assert_eq!(guest.thread_state().posts, 1);
        guest.thread_state_mut().order.push(4);
        Ok(reverie::RdtscResult {
            tsc: 0x7197,
            aux: (request == Rdtsc::Tscp).then_some(83),
        })
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        guest.thread_state_mut().order.push(5);
        let mut info = guest.signal_info().expect("authentic owned native siginfo");
        assert_eq!(
            i32::from_ne_bytes(info[..4].try_into().unwrap()),
            signal as i32
        );
        info[16..20].copy_from_slice(&83_i32.to_ne_bytes());
        guest.set_signal_info(info)?;
        Ok(Some(signal))
    }
    async fn on_exit_thread<G: GlobalRPC<Remote>>(
        &self,
        tid: Pid,
        global: &G,
        state: State,
        status: reverie::ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(status, reverie::ExitStatus::Exited(0));
        assert_eq!(
            (state.seed, state.starts, state.posts, state.syscalls),
            (71, 1, 1, 1)
        );
        assert_eq!(state.order, [1, 2, 3]);
        assert_eq!(global.config(), &71);
        assert_eq!(
            global.send_rpc(Request(301)).await,
            Reply(tid.as_raw(), 301)
        );
        // Real Detcore synchronous helpers use this same installed global
        // entry. Check selected routing during the consuming callback too.
        assert_eq!(
            super::super::installed_tool::blocking_global_rpc::<Remote>(Request(303)).unwrap(),
            Reply(tid.as_raw(), 303)
        );
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Remote>>(
        self,
        pid: Pid,
        global: &G,
        status: reverie::ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(status, reverie::ExitStatus::Exited(0));
        assert_eq!(
            global.send_rpc(Request(302)).await,
            Reply(pid.as_raw(), 302)
        );
        Ok(())
    }
}

fn make_host() -> (ToolHost<Probe, Runtime>, std::os::unix::net::UnixStream) {
    let (rpc, peer) = fixture::configured_rpc();
    let subscriptions = [Sysno::getpid].into_iter().collect();
    let host = unsafe {
        ToolHost::new_native(
            Probe::default(),
            rpc,
            fixture::actual_pid(libc::SYS_getpid),
            subscriptions,
            true,
            Runtime,
        )
    };
    (host, peer)
}
fn event(operation: u64) -> SyscallEvent {
    SyscallEvent {
        number: libc::SYS_getpid,
        args: [operation, 0, 0, 0, 0, 0],
        instruction_pointer: 0x1234,
        result: 0,
        context: 0,
        dispatch: SyscallDispatch::InstalledHook,
        guest_pkru: None,
    }
}
fn value(result: NativeCallResult<'_, Probe, i64>) -> i64 {
    match result {
        NativeCallResult::Return(value) => value,
        NativeCallResult::Deferred(_) => panic!("expected completed native handler"),
    }
}

#[test]
fn native_actual_entries_preserve_initialized_lifecycle_and_event_order() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let id = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    for _ in 0..2 {
        let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
        assert_eq!(
            value(unsafe {
                host.dispatch_native_syscall(lease, &services, &rpc, &mut event(1))
                    .unwrap()
            }),
            i64::from(id.physical_pid().as_raw())
        );
    }
    let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
    match unsafe {
        host.dispatch_native_instruction(
            lease,
            &services,
            &rpc,
            &mut event(0),
            NativeInstruction::Rdtsc(Rdtsc::Tscp),
        )
        .unwrap()
    } {
        NativeCallResult::Return(NativeInstructionResult::Rdtsc(answer)) => {
            assert_eq!(answer.tsc, 0x7197);
            assert_eq!(answer.aux, Some(83));
        }
        _ => panic!("wrong instruction completion"),
    }
    let mut info = [0_u8; reverie::SIGNAL_INFO_SIZE];
    info[..4].copy_from_slice(&libc::SIGUSR1.to_ne_bytes());
    let signal_services = callback.services(Some(info));
    let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
    match unsafe {
        host.dispatch_native_signal(lease, &signal_services, &rpc, &mut event(0), libc::SIGUSR1)
            .unwrap()
    } {
        NativeCallResult::Return(NativeSignalResult::Deliver { signal, info }) => {
            assert_eq!(signal, reverie::Signal::SIGUSR1);
            assert_eq!(&info[16..20], &83_i32.to_ne_bytes());
        }
        _ => panic!("wrong signal completion"),
    }
    let state = unsafe { host.retire_native_thread(handle, id).unwrap() };
    assert_eq!(
        (state.seed, state.starts, state.posts, state.syscalls),
        (71, 1, 1, 2)
    );
    assert_eq!(state.order, [1, 2, 3, 3, 4, 5]);
    assert_eq!(callback.admitted.load(Ordering::SeqCst), 1);
    assert_eq!(callback.count(libc::SYS_getpid), 2);
    assert!(
        server.global.seen.lock().unwrap().is_empty(),
        "no fabricated lifecycle RPC"
    );
    // The native C filter selects interception independently of the local
    // subscription set. A valid event must still enter Tool when absent from
    // that set; the old native driver always called its Tool handler.
    assert!(!host.subscriptions.contains(&Sysno::getuid));
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
    let mut unsubscribed = event(1);
    unsubscribed.number = libc::SYS_getuid;
    assert_eq!(
        value(unsafe {
            host.dispatch_native_syscall(lease, &services, &rpc, &mut unsubscribed)
                .unwrap()
        }),
        i64::from(unsafe { libc::getuid() })
    );
    let state = unsafe { host.retire_native_thread(handle, id).unwrap() };
    assert_eq!((state.starts, state.posts, state.syscalls), (1, 1, 1));
    assert_eq!(state.order, [1, 2, 3]);
    assert_eq!(callback.count(libc::SYS_getuid), 1);
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_real_tool_rpc_allows_sibling_progress_while_primary_waits() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    *server.global.first_pending.lock().unwrap() = Some(waiting_tx);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let callback = Callback::new(1);
            let id = callback.identity();
            let services = callback.services(None);
            let handle = unsafe {
                host.register_native_image_thread(id, State::seeded())
                    .unwrap()
            };
            let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
            assert_eq!(
                value(unsafe {
                    host.dispatch_native_syscall(lease, &services, &rpc, &mut event(100))
                        .unwrap()
                }),
                100
            );
            assert_eq!(
                unsafe { host.retire_native_thread(handle, id).unwrap() }.order,
                [1, 2, 3]
            );
        });
        // This notification comes from real GlobalTool::receive_rpc after the
        // first Tool has sent its request and waits on the actual primary.
        waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(host.tool.try_lock().is_some());
        assert!(host.states.try_lock().is_some());
        let callback = Callback::new(1);
        let id = callback.identity();
        let services = callback.services(None);
        let handle = unsafe {
            host.register_native_image_thread(id, State::seeded())
                .unwrap()
        };
        let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
        assert_eq!(
            value(unsafe {
                host.dispatch_native_syscall(lease, &services, &rpc, &mut event(101))
                    .unwrap()
            }),
            101
        );
        assert_eq!(
            unsafe { host.retire_native_thread(handle, id).unwrap() }.order,
            [1, 2, 3]
        );
        assert_eq!(
            callback.count(libc::SYS_socket),
            1,
            "contention needs an independent connection"
        );
        fixture::assert_independent_connection_retired(
            &callback,
            rpc.descriptor(id.physical_pid()).expect("occupied primary"),
        );
        first.join().unwrap();
        let values = server
            .global
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, n)| *n)
            .collect::<Vec<_>>();
        assert_eq!(values, [100, 101]);
        fixture::close_test_primary(&rpc, &callback);
    });
}

#[test]
fn native_actual_tool_decode_reentry_and_cross_thread_wake_preserve_progress() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let id = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    for operation in [500, 200] {
        let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
        assert_eq!(
            value(unsafe {
                host.dispatch_native_syscall(lease, &services, &rpc, &mut event(operation))
                    .unwrap()
            }),
            operation as i64
        );
    }
    let requests = server
        .global
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, n)| *n)
        .collect::<Vec<_>>();
    assert_eq!(requests, [500, 501]);
    assert_eq!(
        callback.count(libc::SYS_socket),
        1,
        "response decode reentered after primary release"
    );
    let state = unsafe { host.retire_native_thread(handle, id).unwrap() };
    assert_eq!((state.starts, state.posts, state.syscalls), (1, 1, 2));
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_deferred_entry_retains_original_scratch_until_real_completion() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let id = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
    let deferred = match unsafe {
        host.dispatch_native_syscall(lease, &services, &rpc, &mut event(7))
            .unwrap()
    } {
        NativeCallResult::Deferred(deferred) => deferred,
        NativeCallResult::Return(_) => panic!("tail operation must remain affine"),
    };
    assert_eq!(deferred.operation(), (libc::SYS_getpid, [7, 0, 0, 0, 0, 0]));
    assert_eq!(
        unsafe { host.enter_native_callback(handle, id) }
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let saved = host
        .tool
        .lock()
        .as_deref()
        .unwrap().as_ref().unwrap()
        .saved_stack
        .load(Ordering::SeqCst);
    assert_ne!(saved, 0);
    assert_eq!(unsafe { (saved as *const u64).read() }, 0x7197);
    assert_eq!(
        callback.count(libc::SYS_getpid),
        0,
        "Rust must not execute a deferred call early"
    );
    // This test owns the native stand-in: apply through the actual selected
    // invoker, then consume the original completion owner after its use ends.
    let (number, args) = deferred.operation();
    let answer = unsafe {
        callback
            .invocation()
            .invoke(number, args, reverie::InjectionPurpose::Application)
    };
    assert_eq!(answer, i64::from(id.physical_pid().as_raw()));
    unsafe { deferred.complete(callback.invocation()).unwrap() };
    let state = unsafe { host.retire_native_thread(handle, id).unwrap() };
    assert_eq!((state.starts, state.posts, state.syscalls), (1, 1, 1));
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_single_handler_error_is_not_restarted_or_published_as_success() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let id = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
    let error = unsafe { host.dispatch_native_syscall(lease, &services, &rpc, &mut event(8)) }
        .err()
        .unwrap();
    assert!(matches!(error, Error::Errno(Errno::ERESTARTSYS)));
    assert_eq!(
        host.tool
            .lock()
            .as_deref()
            .unwrap().as_ref().unwrap()
            .syscall_entries
            .load(Ordering::SeqCst),
        1,
        "native single-handler errors must not enter the local restart loop"
    );
    assert_eq!(callback.admitted.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe { host.enter_native_callback(handle, id) }
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EIO)
    );
    assert_eq!(callback.count(libc::SYS_getpid), 0);
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_wrong_current_thread_refuses_before_tool_or_socket_entry() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let id = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(id, State::seeded())
            .unwrap()
    };
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                // The negative fixture deliberately supplies the original facts
                // on another actual thread; its live NativeCurrent must reject.
                let lease = unsafe { host.enter_native_callback(handle, id).unwrap() };
                let error =
                    unsafe { host.dispatch_native_syscall(lease, &services, &rpc, &mut event(1)) }
                        .err()
                        .unwrap();
                assert!(
                    matches!(error, Error::Io(ref e) if e.raw_os_error() == Some(libc::ESTALE))
                );
            })
            .join()
            .unwrap();
    });
    assert_eq!(callback.admitted.load(Ordering::SeqCst), 0);
    assert!(callback.calls.lock().unwrap().is_empty());
    assert!(server.global.seen.lock().unwrap().is_empty());
    assert_eq!(
        unsafe { host.enter_native_callback(handle, id) }
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EIO)
    );
}

#[test]
fn native_consuming_thread_and_process_exit_use_real_background_rpc_once() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let identity = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(identity, State::seeded())
            .unwrap()
    };
    let lease = unsafe { host.enter_native_callback(handle, identity).unwrap() };
    value(unsafe {
        host.dispatch_native_syscall(lease, &services, &rpc, &mut event(1))
            .unwrap()
    });
    // This ordinary test thread owns real standard Rust TLS/socket service.
    // It does not emulate or qualify entry into a native private loader.
    let background = unsafe {
        super::super::native_rpc::NativeBackgroundRpc::from_admitted(
            server.path.clone(),
            host.rpc.shared_config(),
        )
        .unwrap()
    };
    let before = callback.calls.lock().unwrap().len();
    unsafe {
        host.dispatch_native_thread_exit(
            handle,
            identity,
            NativeExitRpc::Background(background.for_thread(identity.tid()).unwrap()),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    let error = unsafe {
        host.dispatch_native_thread_exit(
            handle,
            identity,
            NativeExitRpc::Background(background.for_thread(identity.tid()).unwrap()),
            reverie::ExitStatus::Exited(0),
        )
    }
    .err()
    .unwrap();
    assert!(matches!(error, Error::Io(ref e) if e.raw_os_error() == Some(libc::ESTALE)));
    unsafe {
        host.dispatch_native_process_exit(
            identity.pid(),
            background.for_thread(identity.pid()).unwrap(),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    let error = unsafe {
        host.dispatch_native_process_exit(
            identity.pid(),
            background.for_thread(identity.pid()).unwrap(),
            reverie::ExitStatus::Exited(0),
        )
    }
    .err()
    .unwrap();
    assert!(matches!(error, Error::Io(ref e) if e.raw_os_error() == Some(libc::ESTALE)));
    assert_eq!(
        &*server.global.seen.lock().unwrap(),
        &[
            (identity.tid().as_raw(), 301),
            (identity.tid().as_raw(), 303),
            (identity.pid().as_raw(), 302)
        ]
    );
    assert_eq!(
        callback.calls.lock().unwrap().len(),
        before,
        "background finalizers must not use an application callback context"
    );
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_application_thread_exit_retains_its_original_rpc_route() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let identity = callback.identity();
    let services = callback.services(None);
    let handle = unsafe {
        host.register_native_image_thread(identity, State::seeded())
            .unwrap()
    };
    let lease = unsafe { host.enter_native_callback(handle, identity).unwrap() };
    value(unsafe {
        host.dispatch_native_syscall(lease, &services, &rpc, &mut event(1))
            .unwrap()
    });
    unsafe {
        host.dispatch_native_thread_exit(
            handle,
            identity,
            NativeExitRpc::Application(rpc.application(callback.invocation())),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    assert_eq!(
        callback.count(libc::SYS_socket),
        1,
        "exit RPC retains the prepared primary"
    );
    assert_eq!(
        &*server.global.seen.lock().unwrap(),
        &[
            (identity.tid().as_raw(), 301),
            (identity.tid().as_raw(), 303)
        ]
    );
    let background = unsafe {
        super::super::native_rpc::NativeBackgroundRpc::from_admitted(
            server.path.clone(),
            host.rpc.shared_config(),
        )
        .unwrap()
    };
    unsafe {
        host.dispatch_native_process_exit(
            identity.pid(),
            background.for_thread(identity.pid()).unwrap(),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    fixture::close_test_primary(&rpc, &callback);
}

#[test]
fn native_wrong_background_thread_and_active_process_exit_refuse_before_consumption() {
    let server = Server::new();
    let (host, _peer) = make_host();
    let rpc = NativeRpc::new(server.path.clone(), host.rpc.shared_config()).unwrap();
    let callback = Callback::new(1);
    let identity = callback.identity();
    let handle = unsafe {
        host.register_native_image_thread(identity, State::seeded())
            .unwrap()
    };
    let background = unsafe {
        super::super::native_rpc::NativeBackgroundRpc::from_admitted(
            server.path.clone(),
            host.rpc.shared_config(),
        )
        .unwrap()
    };
    let error = unsafe {
        host.dispatch_native_process_exit(
            identity.pid(),
            background.for_thread(identity.pid()).unwrap(),
            reverie::ExitStatus::Exited(0),
        )
    }
    .err()
    .unwrap();
    assert!(matches!(error, Error::Io(ref e) if e.kind() == io::ErrorKind::WouldBlock));
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                assert_eq!(
                    background
                        .for_thread(identity.tid())
                        .err()
                        .unwrap()
                        .raw_os_error(),
                    Some(libc::ESTALE)
                );
            })
            .join()
            .unwrap();
    });
    assert!(server.global.seen.lock().unwrap().is_empty());
    let services = callback.services(None);
    let lease = unsafe { host.enter_native_callback(handle, identity).unwrap() };
    value(unsafe {
        host.dispatch_native_syscall(lease, &services, &rpc, &mut event(1))
            .unwrap()
    });
    unsafe {
        host.dispatch_native_thread_exit(
            handle,
            identity,
            NativeExitRpc::Background(background.for_thread(identity.tid()).unwrap()),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    unsafe {
        host.dispatch_native_process_exit(
            identity.pid(),
            background.for_thread(identity.pid()).unwrap(),
            reverie::ExitStatus::Exited(0),
        )
        .unwrap()
    };
    fixture::close_test_primary(&rpc, &callback);
}
