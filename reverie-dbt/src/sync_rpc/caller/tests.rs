// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

//! Protected-primary unit controls using real coordinator framing and sockets.
//! These are not native callback/admission evidence: CallContext identities
//! are supplied by the test, and the syscall bridge executes Linux directly.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use std::time::Instant;

use super::*;

const CHILD: &str = "REVERIE_DBT_CALLER_PRIMARY_TEST_CHILD";
const NAME: &str = "primary_binding_and_ports_refuse_changed_provenance_before_invoker_io";
const CONFIG: u64 = 0x6a71;
const FORBIDDEN_REQUEST: u64 = 999;

static SERIALIZED: AtomicUsize = AtomicUsize::new(0);
static DESERIALIZED: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, PartialEq)]
struct Wire(u64);

impl serde::Serialize for Wire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        assert!(
            GUEST_CLIENT.connection.try_lock().is_ok(),
            "user serialization ran under the primary I/O lease"
        );
        SERIALIZED.fetch_add(1, Ordering::SeqCst);
        self.0.serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for Wire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        assert!(
            GUEST_CLIENT.connection.try_lock().is_ok(),
            "user deserialization ran under the primary I/O lease"
        );
        DESERIALIZED.fetch_add(1, Ordering::SeqCst);
        u64::deserialize(deserializer).map(Self)
    }
}

#[derive(Default)]
struct Global {
    requests: Mutex<Vec<(i32, u64)>>,
}

#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = Wire;
    type Response = Wire;
    type Config = u64;

    async fn receive_rpc(&self, from: Tid, request: Wire) -> Wire {
        self.requests
            .lock()
            .unwrap()
            .push((from.as_raw(), request.0));
        Wire(request.0 ^ CONFIG)
    }
}

#[derive(Default)]
struct Invocations {
    count: AtomicUsize,
    wrong_purpose: AtomicBool,
}

// Reach the real Linux socket/config/RPC operations through exactly the
// callback supplied to PrimaryBinding. Never unwind across this C boundary.
unsafe extern "C" fn invoke_linux(
    context: usize,
    number: i64,
    args: *const u64,
    purpose: u32,
) -> i64 {
    let calls = unsafe { &*(context as *const Invocations) };
    calls.count.fetch_add(1, Ordering::SeqCst);
    if purpose != reverie::InjectionPurpose::Application as u32 {
        calls.wrong_purpose.store(true, Ordering::SeqCst);
        return -i64::from(libc::EINVAL);
    }
    let args = unsafe { args.cast::<[u64; 6]>().read() };
    let result =
        unsafe { libc::syscall(number, args[0], args[1], args[2], args[3], args[4], args[5]) };
    if result == -1 {
        -i64::from(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        )
    } else {
        result
    }
}

fn assert_error<T>(result: Result<T, Error>, expected: &str) {
    match result {
        Err(error) => assert!(error.to_string().contains(expected), "{error}"),
        Ok(_) => panic!("accepted an invalid primary binding; expected {expected}"),
    }
}

fn assert_refused_before_io(
    binding: &PrimaryBinding<Global>,
    call: CallContext,
    port: PrimaryPort<'_, Global>,
    calls: &Invocations,
    expected: &str,
) {
    let before = calls.count.load(Ordering::SeqCst);
    assert_error(binding.borrow(call), expected);
    let mut operation_entered = false;
    assert_error(
        port.exchange(|client| {
            operation_entered = true;
            client.try_send_rpc(Wire(FORBIDDEN_REQUEST))
        }),
        expected,
    );
    assert!(
        !operation_entered,
        "a refused port entered its RPC operation"
    );
    assert_eq!(calls.count.load(Ordering::SeqCst), before);
}

// This cleanup is solely for an isolated test process. The production owner
// deliberately keeps its numeric descriptor occupied until image/process exit.
struct PrimaryCleanup {
    call: CallContext,
    fd: i32,
}

impl Drop for PrimaryCleanup {
    fn drop(&mut self) {
        GUEST_CLIENT.primary.store(0, Ordering::Release);
        match GUEST_CLIENT.connection.try_lock() {
            Ok(mut slot) => *slot = None,
            Err(TryLockError::Poisoned(error)) => *error.into_inner() = None,
            // A failed assertion while holding the test guard unwinds that
            // guard first. An unexpected remaining owner is not disturbed.
            Err(TryLockError::WouldBlock) => return,
        }
        close_guest(self.call.context, self.call.invoke, self.fd);
    }
}

fn exercise_primary() {
    assert!(GUEST_CLIENT.connection.lock().unwrap().is_none());
    assert_eq!(GUEST_CLIENT.primary.load(Ordering::Acquire), 0);
    let calls = Invocations::default();
    let pid = i32::try_from(std::process::id()).unwrap();
    let root = TaskIdentity {
        pid: Tid::from_raw(pid),
        tid: Tid::from_raw(pid),
    };
    let call = CallContext {
        context: &calls as *const Invocations as usize,
        invoke: invoke_linux,
        identity: root,
    };
    let helper = CallContext {
        identity: TaskIdentity {
            pid: root.pid,
            tid: Tid::from_raw(pid.checked_add(1).unwrap()),
        },
        ..call
    };
    let other_process = CallContext {
        identity: TaskIdentity {
            pid: helper.identity.tid,
            tid: helper.identity.tid,
        },
        ..call
    };

    // These are supplied non-root identity controls, not proof that an actual
    // helper cannot forge an adapter's CallContext. Native admission owns that.
    for wrong in [helper, other_process] {
        assert_error(
            PrimaryBinding::<Global>::connect(wrong),
            "caller Tool requires the initial physical root",
        );
        assert_eq!(calls.count.load(Ordering::SeqCst), 0);
    }

    // Real connection establishment, high-FD relocation, initial config, and
    // Arc provenance publication; no handcrafted GuestConnection or response.
    let binding = PrimaryBinding::<Global>::connect(call).unwrap();
    let fd = GUEST_CLIENT.descriptor(std::process::id()).unwrap();
    let _cleanup = PrimaryCleanup { call, fd };
    assert!(fd >= 1024);
    assert_eq!(CallbackPorts::<Global>::config(&binding), &CONFIG);
    {
        let slot = GUEST_CLIENT.connection.lock().unwrap();
        let connection = slot.as_ref().unwrap();
        assert!(connection.usable);
        assert_eq!(connection.pid, std::process::id());
        assert_eq!(connection.fd, fd);
        assert!(Arc::ptr_eq(&connection.identity, &binding.identity));
    }
    let port = binding.borrow(call).unwrap();
    let before = calls.count.load(Ordering::SeqCst);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(runtime.block_on(port.send_rpc(Wire(7))), Wire(7 ^ CONFIG));
    assert!(calls.count.load(Ordering::SeqCst) > before);

    for wrong in [helper, other_process] {
        let before = calls.count.load(Ordering::SeqCst);
        assert_error(
            binding.borrow(wrong),
            "caller primary belongs to a different physical task",
        );
        assert_eq!(calls.count.load(Ordering::SeqCst), before);
    }

    // Contention must refuse; the general DBT client's independent-connection
    // fallback must not be used by this strict caller-owned port.
    {
        let _held = GUEST_CLIENT.connection.lock().unwrap();
        assert_refused_before_io(
            &binding,
            call,
            port,
            &calls,
            "caller primary connection is busy or poisoned",
        );
    }

    // Preserve PID/fd/cache while changing only the connection generation.
    let original_identity = {
        let mut slot = GUEST_CLIENT.connection.lock().unwrap();
        std::mem::replace(&mut slot.as_mut().unwrap().identity, Arc::new(()))
    };
    assert_refused_before_io(
        &binding,
        call,
        port,
        &calls,
        "caller primary connection identity changed or retired",
    );
    GUEST_CLIENT
        .connection
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .identity = original_identity;

    GUEST_CLIENT
        .connection
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .usable = false;
    assert_refused_before_io(
        &binding,
        call,
        port,
        &calls,
        "caller primary connection identity changed or retired",
    );
    GUEST_CLIENT
        .connection
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .usable = true;

    // A still-live fd/Arc without the protected numeric owner is not enough.
    let original_fact = GUEST_CLIENT.primary.swap(0, Ordering::AcqRel);
    assert_refused_before_io(
        &binding,
        call,
        port,
        &calls,
        "caller primary connection identity changed or retired",
    );
    GUEST_CLIENT.primary.store(original_fact, Ordering::Release);

    let original_connection = GUEST_CLIENT.connection.lock().unwrap().take().unwrap();
    assert_refused_before_io(
        &binding,
        call,
        port,
        &calls,
        "caller primary connection identity changed or retired",
    );
    *GUEST_CLIENT.connection.lock().unwrap() = Some(original_connection);

    // Refusals did not reconnect or poison the retained valid owner.
    assert!(!GUEST_CLIENT.connection.is_poisoned());
    assert_eq!(GUEST_CLIENT.descriptor(std::process::id()), Some(fd));
    let port = binding.borrow(call).unwrap();
    assert_eq!(port.try_send_rpc_sync(Wire(8)), Ok(Wire(8 ^ CONFIG)));
    assert_eq!(SERIALIZED.load(Ordering::SeqCst), 2);
    assert_eq!(DESERIALIZED.load(Ordering::SeqCst), 2);
    assert!(!calls.wrong_purpose.load(Ordering::SeqCst));
}

#[test]
fn primary_binding_and_ports_refuse_changed_provenance_before_invoker_io() {
    if let Some(child) = std::env::var_os(CHILD) {
        assert_eq!(child, NAME);
        exercise_primary();
        println!("caller-primary-complete:{NAME}");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let dirfd = std::fs::File::open(directory.path()).unwrap();
    let path = PathBuf::from(format!(
        "/proc/{}/fd/{}/rpc",
        std::process::id(),
        dirfd.as_raw_fd()
    ));
    let global = Arc::new(Global::default());
    let server_global = Arc::clone(&global);
    let server_path = path.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let server = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let server = reverie_rpc_transport::RpcServer::bind(
                    &server_path, server_global, CONFIG,
                )
                .unwrap();
                ready_tx.send(()).unwrap();
                tokio::select! {
                    result = server.serve() => panic!("RPC server ended before the child: {result:?}"),
                    _ = stop_rx => {},
                }
            });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let full_name = format!("{module}::{NAME}");
    let result = (|| {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &full_name, "--nocapture", "--test-threads=1"])
            .env(CHILD, NAME)
            .env(RPC_SOCKET_ENV, &path)
            .env("RUST_BACKTRACE", "0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let child_pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut exceeded = false;
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                exceeded = true;
                child.kill()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok::<_, io::Error>((child_pid, exceeded, child.wait_with_output()?))
    })();
    let _ = stop_tx.send(());
    server.join().unwrap();
    let (pid, exceeded, output) = result.unwrap();
    assert!(
        !exceeded,
        "primary port blocked instead of refusing: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
    let marker = format!("caller-primary-complete:{NAME}");
    assert!(
        output
            .stdout
            .windows(marker.len())
            .any(|bytes| bytes == marker.as_bytes()),
        "selected child did not reach its assertions: {output:?}"
    );
    assert_eq!(
        *global.requests.lock().unwrap(),
        [(pid as i32, 7), (pid as i32, 8)],
        "a refused exchange reached the coordinator or the valid path stopped working"
    );
}
