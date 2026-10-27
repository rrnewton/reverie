/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The same concrete Tool and RPC types are compiled into the caller and host test.
//! No native callback, terminal fact, or activation frame is manufactured here.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Serialize;

pub const NONCE: u64 = 73;
pub const MARKER_COOKIE: u64 = 0x4341_4c4c;
pub const LATER_EXIT_CODE: i32 = 31;
pub const EXPECTED_CONTROLLER_PANIC: &str = "LIFECYCLE_EXPECTED_CONTROLLER_RPC_PANIC";

// One host-test observer for this single concrete Global. The standalone Tool
// never reads or constructs it. It is neither a Tool registry nor runtime gate.
static PUBLIC_FAILURE_OBSERVER: Mutex<Option<Arc<PublicFailureObserver>>> = Mutex::new(None);

static SELECTIONS: AtomicUsize = AtomicUsize::new(0);
static CONSTRUCTORS: AtomicUsize = AtomicUsize::new(0);
static CONFIGURATION_ADDRESS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Invalid,
    SelectedTail,
    OriginalExit,
    SuppressOnce,
    RewriteAway,
    RewriteToTerminal,
    RpcDisconnect,
}

impl Mode {
    pub fn guest_argument(self) -> &'static str {
        match self {
            Self::Invalid => panic!("invalid fixture mode"),
            Self::SelectedTail => "1",
            Self::OriginalExit => "2",
            Self::SuppressOnce => "3",
            Self::RewriteAway => "4",
            Self::RewriteToTerminal => "5",
            Self::RpcDisconnect => "6",
        }
    }

    pub fn has_terminal_attempt(self) -> bool {
        matches!(
            self,
            Self::SelectedTail | Self::SuppressOnce | Self::RewriteAway | Self::RewriteToTerminal
        )
    }

    pub fn continues(self) -> bool {
        matches!(self, Self::SuppressOnce | Self::RewriteAway)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectPoint {
    #[default]
    None,
    Async,
    Sync,
    ThreadExit,
    ProcessExit,
    AsyncControllerPanic,
    Startup,
}

impl DisconnectPoint {
    pub fn guest_argument(self) -> &'static str {
        match self {
            Self::Async | Self::Sync | Self::AsyncControllerPanic => "6",
            Self::Startup | Self::ThreadExit => "7",
            Self::ProcessExit => "8",
            Self::None => panic!("disconnect case requires an explicit point"),
        }
    }

    fn matches(self, event: &Event) -> bool {
        match event {
            Event::StartHook => self == Self::Startup,
            Event::Disconnect { point } => *point == self,
            Event::ThreadExit { .. } => self == Self::ThreadExit,
            Event::ProcessExit { .. } => self == Self::ProcessExit,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terminal {
    #[default]
    Exit,
    ExitGroup,
}

impl Terminal {
    pub fn number(self) -> Sysno {
        match self {
            Self::Exit => Sysno::exit,
            Self::ExitGroup => Sysno::exit_group,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub nonce: u64,
    pub mode: Mode,
    pub disconnect: DisconnectPoint,
    pub terminal: Terminal,
    pub args: [u64; 6],
}

impl Config {
    pub fn new(mode: Mode, terminal: Terminal, code: u64) -> Self {
        let config = Self {
            version: 1,
            nonce: NONCE,
            mode,
            disconnect: DisconnectPoint::None,
            terminal,
            args: [code, 17, 34, 51, 68, 85],
        };
        config.validate();
        config
    }

    pub fn disconnect_at(point: DisconnectPoint) -> Self {
        assert_ne!(point, DisconnectPoint::None);
        let mut config = Self::new(Mode::OriginalExit, Terminal::ExitGroup, 23);
        config.disconnect = point;
        if matches!(
            point,
            DisconnectPoint::Async | DisconnectPoint::Sync | DisconnectPoint::AsyncControllerPanic
        ) {
            config.mode = Mode::RpcDisconnect;
        }
        config.validate();
        config
    }

    pub fn validate(&self) {
        assert_eq!(
            self.version, 1,
            "default configuration is not a fixture launch"
        );
        assert_eq!(self.nonce, NONCE);
        assert_ne!(self.mode, Mode::Invalid);
        match self.disconnect {
            DisconnectPoint::None => assert_ne!(self.mode, Mode::RpcDisconnect),
            DisconnectPoint::Async
            | DisconnectPoint::Sync
            | DisconnectPoint::AsyncControllerPanic => {
                assert_eq!(self.mode, Mode::RpcDisconnect);
                assert_eq!(self.terminal, Terminal::ExitGroup);
                assert_eq!(self.args[0], 23);
            }
            DisconnectPoint::Startup
            | DisconnectPoint::ThreadExit
            | DisconnectPoint::ProcessExit => {
                assert_eq!(self.mode, Mode::OriginalExit);
                assert_eq!(self.terminal, Terminal::ExitGroup);
                assert_eq!(self.args[0], 23);
            }
        }
        assert!(matches!(self.args[0], 23 | 0x117));
        assert_eq!(self.args[1..], [17, 34, 51, 68, 85]);
        if self.mode.continues() {
            assert_eq!(self.terminal, Terminal::Exit);
            assert_eq!(self.args[0], 23);
        }
        if self.mode == Mode::RewriteToTerminal {
            assert_eq!(self.terminal, Terminal::ExitGroup);
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.mode.continues() {
            LATER_EXIT_CODE
        } else {
            (self.args[0] & 0xff) as i32
        }
    }

    pub fn terminal_call(&self) -> Syscall {
        call(self.terminal.number(), self.args)
    }

    pub fn trigger_call(&self) -> Syscall {
        call(
            if self.mode == Mode::RewriteToTerminal {
                Sysno::getpid
            } else {
                self.terminal.number()
            },
            self.args,
        )
    }

    pub fn replacement(&self) -> Option<Syscall> {
        match self.mode {
            Mode::SelectedTail | Mode::RewriteToTerminal => Some(self.terminal_call()),
            Mode::RewriteAway => Some(call(Sysno::getpid, [0; 6])),
            Mode::SuppressOnce => None,
            Mode::OriginalExit | Mode::RpcDisconnect | Mode::Invalid => {
                panic!("mode has no selected trigger")
            }
        }
    }
}

fn call(number: Sysno, args: [u64; 6]) -> Syscall {
    Syscall::from_raw(
        number,
        SyscallArgs::new(
            args[0] as usize,
            args[1] as usize,
            args[2] as usize,
            args[3] as usize,
            args[4] as usize,
            args[5] as usize,
        ),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCall {
    pub number: i64,
    pub args: [u64; 6],
}

pub fn raw_call(syscall: Syscall) -> RawCall {
    let (number, args) = syscall.into_parts();
    RawCall {
        number: number.id() as i64,
        args: [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ],
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    StartHook,
    PostExecHook,
    Disconnect {
        point: DisconnectPoint,
    },
    Attempt {
        observed: RawCall,
        requested_replacement: Option<RawCall>,
    },
    Continued {
        result: i64,
    },
    ThreadExit {
        code: i32,
        attempts: usize,
        drops: usize,
        continued: bool,
    },
    ProcessExit {
        code: i32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    config: Config,
    event: Event,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    nonce: u64,
    sequence: usize,
}

#[derive(Debug)]
pub struct DisconnectWitness {
    pub sender: i32,
    pub sequence: usize,
    pub event: Event,
}

struct PendingRequest(Arc<AtomicUsize>);

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct PublicFailureSnapshot {
    pub config: Config,
    pub events: Vec<(i32, Event)>,
    pub globals_created: usize,
    pub globals_dropped: usize,
    pub active_requests: usize,
    pub requests_dropped: usize,
}

pub struct PublicFailureWatch {
    pub request: mpsc::Receiver<DisconnectWitness>,
    pub acknowledge: mpsc::Sender<()>,
    pub global_dropped: mpsc::Receiver<()>,
}

pub struct PublicFailureObserver {
    config: Config,
    events: Mutex<Vec<(i32, Event)>>,
    globals_created: AtomicUsize,
    globals_dropped: AtomicUsize,
    active_requests: AtomicUsize,
    requests_dropped: AtomicUsize,
    request: mpsc::Sender<DisconnectWitness>,
    acknowledgement: Mutex<mpsc::Receiver<()>>,
    global_dropped: mpsc::Sender<()>,
}

impl PublicFailureObserver {
    pub fn snapshot(&self) -> PublicFailureSnapshot {
        PublicFailureSnapshot {
            config: self.config.clone(),
            events: self.events.lock().unwrap().clone(),
            globals_created: self.globals_created.load(Ordering::SeqCst),
            globals_dropped: self.globals_dropped.load(Ordering::SeqCst),
            active_requests: self.active_requests.load(Ordering::SeqCst),
            requests_dropped: self.requests_dropped.load(Ordering::SeqCst),
        }
    }
}

struct PublicPendingRequest(Arc<PublicFailureObserver>);

impl Drop for PublicPendingRequest {
    fn drop(&mut self) {
        self.0.active_requests.fetch_sub(1, Ordering::SeqCst);
        self.0.requests_dropped.fetch_add(1, Ordering::SeqCst);
    }
}

pub struct LifecycleGlobal {
    config: Config,
    host_pid: u32,
    events: Mutex<Vec<(i32, Event)>>,
    disconnect_witness: Mutex<Option<mpsc::Sender<DisconnectWitness>>>,
    cancelled_requests: Arc<AtomicUsize>,
    public_observer: Option<Arc<PublicFailureObserver>>,
}

impl Default for LifecycleGlobal {
    fn default() -> Self {
        panic!("LifecycleGlobal requires explicit host initialization");
    }
}

impl LifecycleGlobal {
    /// Arm host-only witnesses before the public runner constructs its Global.
    /// The configuration still travels through the ordinary serialized launch.
    pub fn observe_public_failure(
        config: &Config,
    ) -> (Arc<PublicFailureObserver>, PublicFailureWatch) {
        config.validate();
        assert_eq!(config.disconnect, DisconnectPoint::AsyncControllerPanic);
        let (request, received) = mpsc::channel();
        let (acknowledge, acknowledgement) = mpsc::channel();
        let (global_dropped, dropped) = mpsc::channel();
        let observer = Arc::new(PublicFailureObserver {
            config: config.clone(),
            events: Mutex::new(Vec::new()),
            globals_created: AtomicUsize::new(0),
            globals_dropped: AtomicUsize::new(0),
            active_requests: AtomicUsize::new(0),
            requests_dropped: AtomicUsize::new(0),
            request,
            acknowledgement: Mutex::new(acknowledgement),
            global_dropped,
        });
        assert!(
            PUBLIC_FAILURE_OBSERVER
                .lock()
                .unwrap()
                .replace(Arc::clone(&observer))
                .is_none()
        );
        (
            observer,
            PublicFailureWatch {
                request: received,
                acknowledge,
                global_dropped: dropped,
            },
        )
    }

    /// Host-side observation of the real decoded request. This does not write
    /// a response or any activation/control frame.
    pub fn arm_disconnect(&self) -> mpsc::Receiver<DisconnectWitness> {
        assert_ne!(self.config.disconnect, DisconnectPoint::None);
        let (sender, receiver) = mpsc::channel();
        assert!(
            self.disconnect_witness
                .lock()
                .unwrap()
                .replace(sender)
                .is_none()
        );
        receiver
    }

    pub fn cancelled_requests(&self) -> usize {
        self.cancelled_requests.load(Ordering::SeqCst)
    }

    pub fn snapshot(&self) -> (Config, Vec<(i32, Event)>) {
        (self.config.clone(), self.events.lock().unwrap().clone())
    }
}

#[reverie::global_tool]
impl GlobalTool for LifecycleGlobal {
    type Config = Config;
    type Request = Request;
    type Response = Reply;

    async fn init_global_state(config: &Config) -> Self {
        config.validate();
        let public_observer = if config.disconnect == DisconnectPoint::AsyncControllerPanic {
            let observer = PUBLIC_FAILURE_OBSERVER
                .lock()
                .unwrap()
                .take()
                .expect("public runner observer was not armed on the actual host");
            assert_eq!(observer.config, *config);
            assert_eq!(observer.globals_created.fetch_add(1, Ordering::SeqCst), 0);
            Some(observer)
        } else {
            None
        };
        Self {
            config: config.clone(),
            host_pid: std::process::id(),
            events: Mutex::new(Vec::new()),
            disconnect_witness: Mutex::new(None),
            cancelled_requests: Arc::new(AtomicUsize::new(0)),
            public_observer,
        }
    }

    async fn receive_rpc(&self, from: Pid, request: Request) -> Reply {
        assert_eq!(std::process::id(), self.host_pid);
        assert!(from.as_raw() > 0);
        assert_ne!(from.as_raw() as u32, self.host_pid);
        assert_eq!(request.config, self.config);
        let sequence = {
            let mut events = self.events.lock().unwrap();
            if let Some((owner, _)) = events.first() {
                assert_eq!(from.as_raw(), *owner);
            }
            events.push((from.as_raw(), request.event.clone()));
            events.len()
        };
        if let Some(observer) = &self.public_observer {
            observer
                .events
                .lock()
                .unwrap()
                .push((from.as_raw(), request.event.clone()));
        }
        if self.config.disconnect.matches(&request.event) {
            if let Some(observer) = &self.public_observer {
                assert_eq!(
                    self.config.disconnect,
                    DisconnectPoint::AsyncControllerPanic
                );
                assert_eq!(observer.active_requests.fetch_add(1, Ordering::SeqCst), 0);
                let _pending = PublicPendingRequest(Arc::clone(observer));
                observer
                    .request
                    .send(DisconnectWitness {
                        sender: from.as_raw(),
                        sequence,
                        event: request.event,
                    })
                    .expect("public request witness owner is alive");
                // A separate host observer thread acknowledges receiving the
                // actual request before this deliberate controller-only panic.
                // No event mutex is held while panicking; normal unwinding
                // drops the real connection future and its actual socket.
                observer
                    .acknowledgement
                    .lock()
                    .unwrap()
                    .recv()
                    .expect("public request witness was not acknowledged");
                panic!("{EXPECTED_CONTROLLER_PANIC}");
            }
            let _pending = PendingRequest(Arc::clone(&self.cancelled_requests));
            let sender = self
                .disconnect_witness
                .lock()
                .unwrap()
                .take()
                .expect("host must arm the exact pending request before serving Config");
            sender
                .send(DisconnectWitness {
                    sender: from.as_raw(),
                    sequence,
                    event: request.event,
                })
                .expect("disconnect witness owner is alive");
            // The real server owns and cancels this future. No fabricated
            // response, wire error, native callback or wait status is involved.
            return std::future::pending::<Reply>().await;
        }
        Reply {
            nonce: self.config.nonce,
            sequence,
        }
    }
}

impl Drop for LifecycleGlobal {
    fn drop(&mut self) {
        if let Some(observer) = &self.public_observer {
            observer.globals_dropped.fetch_add(1, Ordering::SeqCst);
            // Do not create a second panic if the enclosing host test already
            // failed. Missing observation remains a failed assertion in it.
            let _ = observer.global_dropped.send(());
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct ThreadState {
    tid: i32,
    stamp: u64,
    continued: bool,
}

pub struct LifecycleTool {
    pid: Pid,
    config: Config,
    config_address: usize,
    attempts: AtomicUsize,
    drops: AtomicUsize,
    messages: AtomicUsize,
    thread_exits: AtomicUsize,
}

impl Default for LifecycleTool {
    fn default() -> Self {
        panic!("LifecycleTool requires its actual PID and received configuration");
    }
}

struct Attempt<'a>(&'a AtomicUsize);

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl LifecycleTool {
    fn check_config(&self, config: &Config) {
        assert_eq!(config, &self.config);
        assert_eq!(config as *const Config as usize, self.config_address);
    }

    async fn report<R: GlobalRPC<LifecycleGlobal>>(&self, rpc: &R, event: Event) {
        self.check_config(rpc.config());
        let sequence = self.messages.fetch_add(1, Ordering::SeqCst) + 1;
        let reply = rpc
            .send_rpc(Request {
                config: self.config.clone(),
                event,
            })
            .await;
        assert_eq!(
            reply,
            Reply {
                nonce: self.config.nonce,
                sequence,
            }
        );
    }
}

#[reverie::tool]
impl Tool for LifecycleTool {
    type GlobalState = LifecycleGlobal;
    type ThreadState = ThreadState;

    fn subscriptions(config: &Config) -> Subscription {
        config.validate();
        assert_eq!(SELECTIONS.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(CONSTRUCTORS.load(Ordering::SeqCst), 0);
        assert_eq!(
            CONFIGURATION_ADDRESS.swap(config as *const Config as usize, Ordering::SeqCst),
            0
        );
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::getppid);
        match config.mode {
            Mode::OriginalExit => {}
            Mode::RewriteToTerminal | Mode::RpcDisconnect => {
                subscriptions.syscall(Sysno::getpid);
            }
            _ => {
                subscriptions.syscall(config.terminal.number());
            }
        }
        subscriptions
    }

    fn new(pid: Pid, config: &Config) -> Self {
        config.validate();
        assert!(pid.as_raw() > 0);
        assert_eq!(SELECTIONS.load(Ordering::SeqCst), 1);
        assert_eq!(CONSTRUCTORS.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(
            CONFIGURATION_ADDRESS.load(Ordering::SeqCst),
            config as *const Config as usize
        );
        Self {
            pid,
            config: config.clone(),
            config_address: config as *const Config as usize,
            attempts: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
            messages: AtomicUsize::new(0),
            thread_exits: AtomicUsize::new(0),
        }
    }

    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &ThreadState)>) -> ThreadState {
        assert_eq!(tid, self.pid);
        assert!(parent.is_none());
        ThreadState {
            tid: tid.as_raw(),
            stamp: self.config.nonce,
            continued: false,
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        assert_eq!(guest.pid(), self.pid);
        assert_eq!(guest.tid(), self.pid);
        assert_eq!(guest.ppid(), None);
        assert_eq!(guest.thread_state().tid, self.pid.as_raw());
        assert_eq!(guest.thread_state().stamp, self.config.nonce);
        self.report(guest, Event::StartHook).await;
        guest.thread_state_mut().stamp += 1;
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        assert_eq!(guest.thread_state().stamp, self.config.nonce + 1);
        self.report(guest, Event::PostExecHook).await;
        guest.thread_state_mut().stamp += 2;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        self.check_config(guest.config());
        assert_eq!(guest.pid(), self.pid);
        assert_eq!(guest.tid(), self.pid);
        if self.config.mode == Mode::RpcDisconnect {
            assert_eq!(syscall.number(), Sysno::getpid);
            assert_eq!(raw_call(syscall).args, self.config.args);
            let point = self.config.disconnect;
            if point == DisconnectPoint::Sync {
                let sequence = self.messages.fetch_add(1, Ordering::SeqCst) + 1;
                let request = Request {
                    config: self.config.clone(),
                    event: Event::Disconnect { point },
                };
                // An unsupported sync method is not a transport failure and
                // must not silently switch to the async method for this case.
                let reply = guest.try_send_rpc_sync(request).map_err(|_| {
                    Error::Io(std::io::Error::other(
                        "fixture requires actual synchronous RPC",
                    ))
                })?;
                assert_eq!(
                    reply,
                    Reply {
                        nonce: self.config.nonce,
                        sequence
                    }
                );
            } else {
                assert!(matches!(
                    point,
                    DisconnectPoint::Async | DisconnectPoint::AsyncControllerPanic
                ));
                self.report(guest, Event::Disconnect { point }).await;
            }
            // A wrongly returned response lets the guest print an unexpected
            // continuation. The native fatal path must never reach this value.
            return Ok(self.config.nonce as i64);
        }
        if syscall.number() == Sysno::getppid {
            assert!(self.config.mode.continues());
            assert!(!guest.thread_state().continued);
            assert_eq!(self.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(self.drops.load(Ordering::SeqCst), 1);
            let observed = raw_call(syscall);
            let value = if self.config.mode == Mode::SuppressOnce {
                0
            } else {
                i64::from(self.pid.as_raw())
            };
            assert_eq!(observed.args, [MARKER_COOKIE, value as u64, 3, 4, 5, 6]);
            self.report(guest, Event::Continued { result: value }).await;
            guest.thread_state_mut().continued = true;
            guest.thread_state_mut().stamp += 8;
            return Ok(self.config.nonce as i64);
        }

        assert_ne!(self.config.mode, Mode::OriginalExit);
        assert_eq!(syscall, self.config.trigger_call());
        assert_eq!(self.attempts.fetch_add(1, Ordering::SeqCst), 0);
        let _attempt = Attempt(&self.drops);
        guest.thread_state_mut().stamp += 4;
        let replacement = self.config.replacement();
        self.report(
            guest,
            Event::Attempt {
                observed: raw_call(syscall),
                requested_replacement: replacement.map(raw_call),
            },
        )
        .await;
        match replacement {
            Some(replacement) => guest.tail_inject(replacement).await,
            None => Ok(0),
        }
    }

    async fn on_exit_thread<R: GlobalRPC<LifecycleGlobal>>(
        &self,
        tid: Pid,
        rpc: &R,
        state: ThreadState,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(tid, self.pid);
        assert_eq!(state.tid, tid.as_raw());
        assert_eq!(self.thread_exits.fetch_add(1, Ordering::SeqCst), 0);
        let expected_attempts = usize::from(self.config.mode.has_terminal_attempt());
        let attempts = self.attempts.load(Ordering::SeqCst);
        let drops = self.drops.load(Ordering::SeqCst);
        assert_eq!(attempts, expected_attempts);
        assert_eq!(
            drops, attempts,
            "Tool future still borrowed at consuming hook"
        );
        assert_eq!(state.continued, self.config.mode.continues());
        assert_eq!(
            state.stamp,
            self.config.nonce + 3 + 4 * attempts as u64 + 8 * u64::from(state.continued)
        );
        assert_eq!(status, ExitStatus::Exited(self.config.exit_code()));
        self.report(
            rpc,
            Event::ThreadExit {
                code: status.code().expect("normal explicit exit"),
                attempts,
                drops,
                continued: state.continued,
            },
        )
        .await;
        Ok(())
    }

    async fn on_exit_process<R: GlobalRPC<LifecycleGlobal>>(
        self,
        pid: Pid,
        rpc: &R,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(pid, self.pid);
        assert_eq!(self.thread_exits.load(Ordering::SeqCst), 1);
        assert_eq!(status, ExitStatus::Exited(self.config.exit_code()));
        self.report(
            rpc,
            Event::ProcessExit {
                code: status.code().expect("normal explicit exit"),
            },
        )
        .await;
        Ok(())
    }
}
