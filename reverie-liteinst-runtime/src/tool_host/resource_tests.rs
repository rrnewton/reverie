use std::io::Read;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::Mutex;

use super::*;
use crate::rpc::resources::Resources;
use crate::rpc::tests::isolated;
use crate::rpc::tests::read_frame;
use crate::rpc::tests::write_frame;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct State {
    #[serde(skip)]
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl Drop for State {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("state-drop");
    }
}

#[derive(Default)]
struct TerminalTool {
    events: Arc<Mutex<Vec<&'static str>>>,
    resources: Option<Resources>,
    fail_process: bool,
}

#[reverie::tool]
impl Tool for TerminalTool {
    type GlobalState = ();
    type ThreadState = State;

    fn new(_: Pid, _: &()) -> Self {
        let tool = Self::default();
        tool.events.lock().unwrap().push("new");
        tool
    }

    fn init_thread_state(&self, _: Pid, parent: Option<(Pid, &State)>) -> State {
        assert!(parent.is_none());
        State {
            events: self.events.clone(),
        }
    }

    async fn on_exit_thread<G: GlobalRPC<()>>(
        &self,
        _: Pid,
        rpc: &G,
        state: State,
        _: reverie::ExitStatus,
    ) -> Result<(), Error> {
        self.events.lock().unwrap().push("thread");
        assert_eq!(self.resources.as_ref().unwrap().active(), 2);
        rpc.send_rpc(()).await;
        drop(state);
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<()>>(
        self,
        _: Pid,
        rpc: &G,
        _: reverie::ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(
            *self.events.lock().unwrap(),
            ["new", "thread", "state-drop"]
        );
        assert_eq!(self.resources.as_ref().unwrap().active(), 2);
        assert!(self.resources.as_ref().unwrap().acquire().is_err());
        self.events.lock().unwrap().push("process");
        rpc.send_rpc(()).await;
        if self.fail_process {
            return Err(Errno::EIO.into());
        }
        Ok(())
    }
}

fn terminal_path(fail_process: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("terminal");
    let listener = UnixListener::bind(&path).unwrap();
    let tid = raw_pid(libc::SYS_gettid);
    let server = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        write_frame(
            &mut peer,
            &reverie_rpc_transport::codec::encode(&()).unwrap(),
        );
        for _ in 0..2 {
            let request: reverie_rpc_transport::RequestEnvelope<()> =
                reverie_rpc_transport::codec::decode(&read_frame(&mut peer)).unwrap();
            assert_eq!(request.from, tid);
            write_frame(
                &mut peer,
                &reverie_rpc_transport::codec::encode(&()).unwrap(),
            );
        }
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    });
    let resources = Resources::new();
    let rpc = Arc::new(CoordinatorRpc::<()>::connect_bootstrap(&path, resources.clone()).unwrap());
    let (pid, actual_tid) = rpc.identity();
    assert_eq!(actual_tid, tid);
    assert_eq!(resources.active(), 1);
    let mut tool = TerminalTool::new(pid, rpc.config());
    tool.resources = Some(resources.clone());
    tool.fail_process = fail_process;
    let events = tool.events.clone();
    let registry = Registry::with_resources(pid, tool, rpc.resources());
    assert_eq!(resources.active(), 1);
    let scratch = DispatchScratchScope::enter();
    let mut invocation = registry
        .reserve(pid, tid, rpc.clone(), &scratch.owner, true)
        .unwrap();
    invocation.initialize(None).unwrap();
    let result = finish_tool_exit_callbacks(
        &registry,
        invocation,
        ToolExitContext {
            tid,
            pid,
            number: libc::SYS_exit_group,
            args: [7, 0, 0, 0, 0, 0],
        },
        |_| {
            assert_eq!(resources.active(), 2);
            events.lock().unwrap().push("stats");
            Ok(())
        },
        || {
            assert_eq!(resources.active(), 2);
            events.lock().unwrap().push("retire");
            rpc.retire()?;
            assert_eq!(resources.active(), 1);
            Ok(())
        },
    );
    assert!(!registry.tool_present());
    if fail_process {
        assert!(matches!(result, Err(Error::Errno(Errno::EIO))));
        assert_eq!(
            *events.lock().unwrap(),
            ["new", "thread", "state-drop", "process"]
        );
        assert_eq!(resources.active(), 1);
        assert!(rpc.raw_fd() >= 0);
        rpc.retire().unwrap();
        drive_ready(resources.drain()).unwrap();
    } else {
        assert!(result.unwrap());
        assert_eq!(
            *events.lock().unwrap(),
            ["new", "thread", "state-drop", "process", "stats", "retire"]
        );
        assert_eq!(resources.active(), 0);
        assert_eq!(rpc.raw_fd(), -1);
    }
    server.join().unwrap();
}

#[test]
fn callback_drain_precedes_terminal_rpc_and_final_resource_drain() {
    isolated(
        "tool_host::resource_tests::callback_drain_precedes_terminal_rpc_and_final_resource_drain",
        || terminal_path(false),
    );
}

#[test]
fn failed_terminal_rpc_hook_does_not_claim_resource_drain() {
    isolated(
        "tool_host::resource_tests::failed_terminal_rpc_hook_does_not_claim_resource_drain",
        || terminal_path(true),
    );
}
