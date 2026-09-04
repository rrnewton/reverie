use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

#[derive(Default)]
struct CallbackGlobal {
    entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<(i32, u64)>>>,
    release: tokio::sync::Notify,
}

#[reverie::tool]
impl GlobalTool for CallbackGlobal {
    type Request = u64;
    type Response = u64;
    type Config = u64;

    async fn receive_rpc(&self, from: Pid, request: u64) -> u64 {
        self.entered
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((from.as_raw(), request))
            .unwrap();
        if request == 1 {
            self.release.notified().await;
        }
        request + 10
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct CallbackState {
    request: u64,
    #[serde(skip)]
    shared: Arc<AtomicUsize>,
}

#[derive(Default)]
struct CallbackTool;

#[reverie::tool]
impl Tool for CallbackTool {
    type GlobalState = CallbackGlobal;
    type ThreadState = CallbackState;

    fn init_thread_state(&self, _tid: Pid, parent: Option<(Pid, &CallbackState)>) -> CallbackState {
        match parent {
            Some((_, parent)) => {
                assert_eq!(parent.request, 1);
                parent.shared.fetch_add(1, Ordering::SeqCst);
                CallbackState {
                    request: 2,
                    shared: parent.shared.clone(),
                }
            }
            None => CallbackState {
                request: 1,
                shared: Arc::new(AtomicUsize::new(0)),
            },
        }
    }

    async fn handle_guest_progress<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        assert_eq!(*guest.config(), 73);
        assert_eq!(guest.read_clock()?, 42);
        let request = guest.thread_state().request;
        let response = guest.send_rpc(request).await;
        assert_eq!(response, request + 10);
        guest
            .thread_state_mut()
            .shared
            .fetch_add(10, Ordering::SeqCst);
        assert_eq!(guest.read_clock()?, 42);
        Ok(())
    }
}

#[test]
fn independent_real_clients_progress_while_an_entry_blocks_and_retirement_keeps_client_alive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("callbacks.sock");
    let global = Arc::new(CallbackGlobal::default());
    let (entered, requests) = std::sync::mpsc::channel();
    *global.entered.lock().unwrap() = Some(entered);
    let (ready, listening) = std::sync::mpsc::sync_channel(1);
    let server_global = global.clone();
    let server_path = path.clone();
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server =
                reverie_rpc_transport::RpcServer::bind(server_path, server_global, 73).unwrap();
            ready.send(()).unwrap();
            let (first, second) = tokio::join!(server.serve_one(), server.serve_one());
            first.unwrap();
            second.unwrap();
        });
    });
    listening.recv().unwrap();
    let (child_client, receive_child) = std::sync::mpsc::sync_channel(1);
    let (child_start, child_host) = std::sync::mpsc::sync_channel::<Arc<ToolHost<CallbackTool>>>(1);
    let (child_done, child_result) = std::sync::mpsc::sync_channel(1);
    let child_path = path.clone();
    let child = std::thread::spawn(move || {
        let rpc = Arc::new(CoordinatorRpc::<CallbackGlobal>::connect(child_path).unwrap());
        let (_, tid) = rpc.identity();
        child_client.send(rpc).unwrap();
        let host = child_host.recv().unwrap();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        let result = host.guest_progress(i64::from(tid.as_raw()), &mut context, 42);
        child_done.send(result).unwrap();
    });
    let (parent_ready, receive_parent) = std::sync::mpsc::sync_channel(1);
    let (parent_done, parent_result) = std::sync::mpsc::sync_channel(1);
    let parent = std::thread::spawn(move || {
        let rpc = Arc::new(CoordinatorRpc::<CallbackGlobal>::connect(path).unwrap());
        let (pid, tid) = rpc.identity();
        let host = Arc::new(ToolHost {
            registry: Registry::new(pid, CallbackTool),
            rpc,
            root_pid: pid,
            subscriptions: Default::default(),
            instruction_subscriptions: runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
            instruction_results_only: false,
            stats: crate::stats::GuestStatsHooks::DISABLED,
        });
        let shared;
        {
            let parent_scope = DispatchScratchScope::enter();
            let mut invocation = host
                .registry
                .reserve(pid, tid, host.rpc.clone(), &parent_scope.owner, true)
                .unwrap();
            invocation.initialize(None).unwrap();
            shared = invocation.parts().1.shared.clone();
            let rpc = receive_child.recv().unwrap();
            let (_, child_tid) = rpc.identity();
            let child_scope = DispatchScratchScope::enter();
            let child_identity = invocation
                .construct_child(child_tid, rpc, &child_scope.owner)
                .unwrap();
            assert_eq!(shared.load(Ordering::SeqCst), 1);
            let inspection_scope = DispatchScratchScope::enter();
            let mut child = host
                .registry
                .acquire(child_identity, &inspection_scope.owner)
                .unwrap();
            assert!(Arc::ptr_eq(&shared, &child.parts().1.shared));
            child.complete().unwrap();
            invocation.complete().unwrap();
            parent_ready
                .send((host.clone(), tid, child_tid, shared.clone()))
                .unwrap();
        }
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        parent_done
            .send(host.guest_progress(i64::from(tid.as_raw()), &mut context, 42))
            .unwrap();
    });
    let (host, parent_tid, child_tid, shared) = receive_parent.recv().unwrap();
    let deadline = std::time::Duration::from_secs(10);
    assert_eq!(
        requests.recv_timeout(deadline).unwrap(),
        (parent_tid.as_raw(), 1)
    );
    let identity = host.registry.identity(host.root_pid, parent_tid).unwrap();
    {
        let scratch = DispatchScratchScope::enter();
        assert!(host.registry.acquire(identity, &scratch.owner).is_err());
    }
    host.registry.retire(identity).unwrap();
    assert!(unsafe { libc::fcntl(host.rpc.raw_fd(), libc::F_GETFD) } >= 0);
    assert!(parent_result.try_recv().is_err());
    child_start.send(host.clone()).unwrap();
    assert_eq!(
        requests.recv_timeout(deadline).unwrap(),
        (child_tid.as_raw(), 2)
    );
    child_result.recv_timeout(deadline).unwrap().unwrap();
    assert_eq!(shared.load(Ordering::SeqCst), 11);
    global.release.notify_one();
    assert!(parent_result.recv_timeout(deadline).unwrap().is_err());
    assert_eq!(shared.load(Ordering::SeqCst), 21);
    parent.join().unwrap();
    child.join().unwrap();
    drop(host);
    server.join().unwrap();
}
