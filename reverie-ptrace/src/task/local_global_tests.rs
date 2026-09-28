//! Local association controls using actual ptrace callbacks and guest bytes.
//! This bridge is not Hermit scheduler, native-file, or copy-exclusion proof.
use reverie::syscalls::RemoteIoVec;
use serde::Deserialize;
use serde::Serialize;

use super::*;

#[derive(Default)]
struct Log {
    instance: AtomicUsize,
    ready: StdMutex<Vec<(i32, usize, usize)>>,
    rpcs: AtomicUsize,
    observations: AtomicUsize,
    writes: AtomicUsize,
    refusals: AtomicUsize,
    post_execs: AtomicUsize,
}

impl Log {
    fn check_instance(&self) {
        let actual = self as *const Self as usize;
        match self
            .instance
            .compare_exchange(0, actual, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {}
            Err(recorded) => assert_eq!(recorded, actual),
        }
    }

    fn check_ready(&self, tid: Tid, state: &State) {
        self.check_instance();
        assert_eq!(
            self.ready
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|row| row.0 == tid.as_raw())
                .copied(),
            Some((
                tid.as_raw(),
                Arc::as_ptr(&state.phase) as usize,
                state.phase.load(Ordering::SeqCst),
            ))
        );
    }
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = ();
    type Request = ();
    type Response = ();

    async fn receive_rpc(&self, _from: Pid, _: ()) {
        self.check_instance();
        self.rpcs.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    phase: Arc<AtomicUsize>,
}

impl AsRef<State> for State {
    fn as_ref(&self) -> &State {
        self
    }
}
impl AsMut<State> for State {
    fn as_mut(&mut self) -> &mut State {
        self
    }
}

#[derive(Default)]
struct BridgeTool;
impl AsMut<BridgeTool> for BridgeTool {
    fn as_mut(&mut self) -> &mut BridgeTool {
        self
    }
}

/// Hold only immutable Guest/global/thread borrows across a real suspension,
/// then obtain exactly one actual Memory for both synchronous native methods.
async fn observe_and_write<G: Guest<BridgeTool>>(
    guest: &G,
    write: Option<reverie::syscalls::Read>,
) -> Result<i64, reverie::Error> {
    guest.send_rpc(()).await;
    let tid = guest.tid();
    let global = guest
        .local_global_state()
        .expect("real ptrace Guest must borrow its actual global instance");
    let state = guest.thread_state();
    global.check_ready(tid, state);
    let rpcs = global.rpcs.load(Ordering::SeqCst);
    assert!(rpcs > 0);
    tokio::task::yield_now().await;
    assert!(std::ptr::eq(global, guest.local_global_state().unwrap()));
    assert!(std::ptr::eq(state, guest.thread_state()));
    global.check_ready(tid, state);
    global.observations.fetch_add(1, Ordering::SeqCst);

    let Some(read) = write else { return Ok(0) };
    assert_eq!(read.len(), 8);
    let mut memory = guest.memory();
    let local = [std::io::IoSlice::new(b"borrowed")];
    let remote = [RemoteIoVec::new(read.buf().unwrap(), 8).unwrap()];
    if read.fd() == -902 {
        // Both real native boundaries reject a different target, with no copy.
        assert_eq!(
            memory.validate_native_user_key0_write_access(0),
            Err(Errno::ESRCH)
        );
        assert_eq!(
            memory.write_native_user_vectored(0, &local, &remote),
            Err(Errno::ESRCH)
        );
        global.refusals.fetch_add(1, Ordering::SeqCst);
        return Err(Errno::ESRCH.into());
    }
    memory.validate_native_user_key0_write_access(tid.as_raw())?;
    assert_eq!(
        memory.write_native_user_vectored(tid.as_raw(), &local, &remote),
        Ok(8)
    );
    global.writes.fetch_add(1, Ordering::SeqCst);
    Ok(8)
}

#[reverie::tool]
impl Tool for BridgeTool {
    type GlobalState = Log;
    type ThreadState = State;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::read, Sysno::getpgid]);
        subscriptions
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        guest.local_global_state().unwrap().check_instance();
        *guest.thread_state_mut() = State {
            phase: Arc::new(AtomicUsize::new(1)),
        };
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        *guest.thread_state_mut() = State {
            phase: Arc::new(AtomicUsize::new(2)),
        };
        let global = guest.local_global_state().unwrap();
        global.check_instance();
        assert_eq!(guest.thread_state().phase.load(Ordering::SeqCst), 2);
        global.post_execs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn on_thread_state_ready(
        &self,
        tid: Tid,
        global: &Log,
        state: &State,
    ) -> Result<(), reverie::Error> {
        global.check_instance();
        global.ready.lock().unwrap().push((
            tid.as_raw(),
            Arc::as_ptr(&state.phase) as usize,
            state.phase.load(Ordering::SeqCst),
        ));
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        if let Syscall::Read(read) = call {
            if [-900, -901, -902].contains(&read.fd()) {
                if read.fd() == -901 {
                    let expected_global =
                        guest.local_global_state().unwrap() as *const Log as usize;
                    let expected_state = Arc::as_ptr(&guest.thread_state().phase) as usize;
                    let wrapped = guest.into_guest();
                    let forwarded: &Log =
                        Guest::<BridgeTool>::local_global_state(&wrapped).unwrap();
                    assert_eq!(forwarded as *const Log as usize, expected_global);
                    assert_eq!(
                        Arc::as_ptr(&Guest::<BridgeTool>::thread_state(&wrapped).phase) as usize,
                        expected_state
                    );
                    return observe_and_write(&wrapped, Some(read)).await;
                }
                return observe_and_write(guest, Some(read)).await;
            }
        }
        if matches!(call, Syscall::Getpgid(_)) {
            observe_and_write(guest, None).await?;
        }
        Ok(guest.inject(call).await?)
    }
}

// An RPC implementation can own a local-looking object and still must not
// inherit direct local authority from the default Guest method.
#[derive(Default)]
struct RpcOnlyGuest {
    log: Log,
    state: State,
}

#[reverie::tool]
impl GlobalRPC<Log> for RpcOnlyGuest {
    async fn send_rpc(&self, _: ()) {
        self.log.receive_rpc(self.tid(), ()).await;
    }
    fn config(&self) -> &() {
        &()
    }
}

#[reverie::tool]
impl Guest<BridgeTool> for RpcOnlyGuest {
    type Memory = reverie::syscalls::LocalMemory;
    type Stack = GuestStack;
    fn tid(&self) -> Pid {
        Pid::from_raw(7)
    }
    fn pid(&self) -> Pid {
        self.tid()
    }
    fn ppid(&self) -> Option<Pid> {
        None
    }
    fn memory(&self) -> Self::Memory {
        panic!("default local refusal must not access memory")
    }
    fn thread_state(&self) -> &State {
        &self.state
    }
    fn thread_state_mut(&mut self) -> &mut State {
        &mut self.state
    }
    async fn regs(&mut self) -> libc::user_regs_struct {
        panic!("unexpected registers")
    }
    async fn stack(&mut self) -> Self::Stack {
        panic!("unexpected stack")
    }
    async fn daemonize(&mut self) {
        panic!("unexpected daemonization")
    }
    async fn inject<S: SyscallInfo>(&mut self, _: S) -> Result<i64, Errno> {
        panic!("unexpected injection")
    }
    async fn tail_inject<S: SyscallInfo>(&mut self, _: S) -> Never {
        panic!("unexpected tail injection")
    }
    fn set_timer(&mut self, _: TimerSchedule) -> Result<(), reverie::Error> {
        panic!("unexpected timer")
    }
    fn set_timer_precise(&mut self, _: TimerSchedule) -> Result<(), reverie::Error> {
        panic!("unexpected timer")
    }
    fn read_clock(&mut self) -> Result<u64, reverie::Error> {
        panic!("unexpected clock")
    }
}

#[test]
fn local_global_default_and_into_guest_do_not_invent_rpc_local_authority() {
    let mut guest = RpcOnlyGuest::default();
    assert!(guest.local_global_state().is_none());
    let wrapped = guest.into_guest();
    assert!(Guest::<BridgeTool>::local_global_state(&wrapped).is_none());
    assert_eq!(guest.log.rpcs.load(Ordering::SeqCst), 0);
    assert_eq!(guest.log.observations.load(Ordering::SeqCst), 0);
}

#[test]
fn native_local_global_and_into_guest_hold_identity_across_await_and_actual_stores() {
    let (output, log) = crate::testing::test_fn::<BridgeTool, _>(|| unsafe {
        for fd in [-900, -901, -902] {
            let mut bytes = [0xa5u8; 32];
            let result = libc::syscall(libc::SYS_read, fd, bytes.as_mut_ptr().add(8), 8usize);
            if fd == -902 {
                assert_eq!(result, -1);
                assert_eq!(*libc::__errno_location(), libc::ESRCH);
                assert_eq!(bytes, [0xa5; 32]);
            } else {
                assert_eq!(result, 8);
                assert_eq!(&bytes[..8], &[0xa5; 8]);
                assert_eq!(&bytes[8..16], b"borrowed");
                assert_eq!(&bytes[16..], &[0xa5; 16]);
            }
        }
    })
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(log.rpcs.load(Ordering::SeqCst), 3);
    assert_eq!(log.observations.load(Ordering::SeqCst), 3);
    assert_eq!(log.writes.load(Ordering::SeqCst), 2);
    assert_eq!(log.refusals.load(Ordering::SeqCst), 1);
}

#[test]
fn native_local_global_fork_and_clone_keep_one_global_and_current_thread_states() {
    let (output, log) = crate::testing::test_fn::<BridgeTool, _>(|| unsafe {
        assert!(libc::syscall(libc::SYS_getpgid, 0) > 0);
        for shared in [false, true] {
            let child = if shared {
                libc::syscall(
                    libc::SYS_clone,
                    libc::CLONE_FILES | libc::SIGCHLD,
                    0usize,
                    0usize,
                    0usize,
                    0usize,
                ) as libc::pid_t
            } else {
                libc::fork()
            };
            assert!(child >= 0);
            if child == 0 {
                let result = libc::syscall(libc::SYS_getpgid, 0);
                libc::_exit(if result > 0 { 0 } else { 31 });
            }
            let mut status = -1;
            assert_eq!(libc::waitpid(child, &mut status, 0), child);
            assert_eq!(status, 0);
        }
    })
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|row| row.2 == 1));
    assert_eq!(
        ready
            .iter()
            .map(|row| row.0)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(log.rpcs.load(Ordering::SeqCst), 3);
    assert_eq!(log.observations.load(Ordering::SeqCst), 3);
    assert_eq!(log.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn native_local_global_exec_uses_actual_post_exec_replacement_state() {
    let (output, log) = crate::testing::test_cmd::<BridgeTool>("/bin/true", &[]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 2);
    assert_eq!((ready[0].2, ready[1].2), (1, 2));
    assert_eq!(ready[0].0, ready[1].0);
    assert_ne!(ready[0].1, ready[1].1);
    assert_eq!(log.post_execs.load(Ordering::SeqCst), 1);
}

#[test]
fn native_local_global_failed_exec_keeps_current_instance_and_state() {
    let (output, log) = crate::testing::test_fn::<BridgeTool, _>(|| unsafe {
        let path = c"/hermit-local-global-no-such-executable";
        assert_eq!(
            libc::syscall(
                libc::SYS_execve,
                path.as_ptr(),
                std::ptr::null::<*const libc::c_char>(),
                std::ptr::null::<*const libc::c_char>()
            ),
            -1
        );
        assert_eq!(*libc::__errno_location(), libc::ENOENT);
        assert!(libc::syscall(libc::SYS_getpgid, 0) > 0);
    })
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].2, 1);
    assert_eq!(log.post_execs.load(Ordering::SeqCst), 0);
    assert_eq!(log.observations.load(Ordering::SeqCst), 1);
}
