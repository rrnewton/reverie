/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Actual configured int3 interception, without a LiteInst preload or rewriter.

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;

use reverie::Guest;
use reverie::process::Stdio;
use reverie::syscalls::Addr;
use reverie::syscalls::Syscall;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn unbound_cleanup_retention_survives_attachment_and_tls_storage_refusal() {
    const NAME: &str = "tracer::injected_error_tests::unbound_cleanup_retention_survives_attachment_and_tls_storage_refusal";
    const ROLE: &str = "REVERIE_UNBOUND_TLS_CHILD";
    const DEADLINE: &str = "REVERIE_UNBOUND_TLS_DEADLINE";
    fn now_ns() -> u64 {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    }
    if std::env::var(ROLE).as_deref() != Ok(NAME) {
        let deadline = now_ns() + 5_000_000_000;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ROLE, NAME)
            .env(DEADLINE, deadline.to_string())
            .spawn()
            .unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated TLS resource fixture failed: {status}"
                );
                assert!(now_ns() < deadline, "original five-second deadline expired");
                return;
            }
            if now_ns() >= deadline {
                let signal = child.kill();
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                let rescue = loop {
                    let status = child.try_wait().unwrap();
                    if status.is_some() || Instant::now() >= rescue_deadline {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                panic!(
                    "original TLS fixture deadline failed; separate rescue only: {signal:?}, {rescue:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(std::env::args().any(|arg| arg == NAME));
    assert!(std::env::args().any(|arg| arg == "--exact"));
    assert!(OrdinaryAdmission::acquire().is_ok());
    struct Resource {
        drops: Arc<std::sync::atomic::AtomicUsize>,
        _original: Box<u64>,
    }
    impl PtraceCleanupResource for Resource {
        fn cleanup(&mut self) -> Result<(), Error> {
            panic!("unbound resource acquired cleanup authority")
        }
    }
    impl Drop for Resource {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn resource(drops: &Arc<std::sync::atomic::AtomicUsize>) -> Resource {
        Resource {
            drops: drops.clone(),
            _original: Box::new(71),
        }
    }
    fn storage_marker() -> CleanupUnconfirmed {
        // White-box marker for the entry-before-lookup boundary, with an actual
        // live process identity. No fabricated traced tree or cleanup success.
        CleanupUnconfirmed {
            id: u64::MAX,
            thread: std::thread::current().id(),
            primary: Arc::new(anyhow::Error::new(AfterEffect(Box::new(71))).into()),
            origin: reverie::BackendFailure {
                pid: Pid::from_raw(unsafe { libc::getpid() }),
                tid: Pid::from_raw(unsafe { libc::gettid() }),
                phase: "test attachment storage refusal",
            },
            owner_identity: Arc::new(Ok(CleanupOwnerIdentity::capture().unwrap())),
        }
    }
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let marker = storage_marker();
    CLEANUP_QUARANTINE.with(|owners| {
        let _borrow = owners.borrow_mut();
        assert!(matches!(
            marker.retain_cleanup_resource::<(), ExitStatus, _>(resource(&drops)),
            Err(CleanupLookupError::StorageUnavailable)
        ));
        quarantine_cleanup_resource(resource(&drops));
    });
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(*UNCONFIRMED_QUARANTINES.lock().unwrap(), 2);
    struct Teardown {
        marker: CleanupUnconfirmed,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Drop for Teardown {
        fn drop(&mut self) {
            assert!(
                CLEANUP_QUARANTINE.try_with(|_| ()).is_err(),
                "registry was not actually torn down"
            );
            assert!(matches!(
                self.marker
                    .retain_cleanup_resource::<(), ExitStatus, _>(resource(&self.drops)),
                Err(CleanupLookupError::StorageUnavailable)
            ));
            quarantine_cleanup_resource(resource(&self.drops));
        }
    }
    thread_local! {
        static TEARDOWN: std::cell::RefCell<Option<Teardown>> = const { std::cell::RefCell::new(None) };
    }
    let thread_drops = drops.clone();
    std::thread::spawn(move || {
        TEARDOWN.with(|slot| {
            *slot.borrow_mut() = Some(Teardown {
                marker: storage_marker(),
                drops: thread_drops,
            })
        });
        // Initialized last, destroyed first: exercise real unavailable TLS,
        // not a returned-error seam or a borrowed-registry approximation.
        CLEANUP_QUARANTINE.with(|_| ());
    })
    .join()
    .unwrap();
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "storage refusal destroyed an original guard"
    );
    assert_eq!(*UNCONFIRMED_QUARANTINES.lock().unwrap(), 4);
    let refused = spawn_fn::<(), _>(|| panic!("refused admission executed guest work")).await;
    assert!(
        matches!(refused, Err(Error::Tool(error)) if error.downcast_ref::<CleanupAdmissionRefused>().is_some())
    );
    assert!(now_ns() < std::env::var(DEADLINE).unwrap().parse::<u64>().unwrap());
    // These four unbound allocations intentionally remain until isolated process
    // exit. They are not called reaped, rescued, or recoverable product owners.
}

const ENTRY: u64 = 0x401000;
const DATA: u64 = 0x402000;
const MARKER: u64 = 0x5452415054455354;
const EFFECT: &[u8] = b"committed-effect";
const AFTER: &[u8] = b"guest-continued";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
enum Mode {
    #[default]
    Success,
    Errno,
    ToolError,
    ChildToolError,
}

#[derive(Default)]
struct Log {
    events: Arc<StdMutex<Vec<&'static str>>>,
    effect_tid: Arc<AtomicI32>,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = Mode;
    type Request = ();
    type Response = ();

    async fn receive_rpc(&self, from: Pid, _: ()) {
        self.effect_tid.store(from.as_raw(), Ordering::SeqCst);
        self.events.lock().unwrap().push("effect");
    }

    fn report_backend_failure(&self, _: reverie::BackendFailure) {
        self.events.lock().unwrap().push("failed");
    }
}

#[derive(Debug, thiserror::Error)]
#[error("typed injected failure after effect {0}")]
struct AfterEffect(Box<u64>);

#[derive(Default)]
struct TrapTool;

#[reverie::tool]
impl Tool for TrapTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_: &Mode) -> Subscription {
        let mut events = Subscription::none();
        events.syscalls([Sysno::getpid]);
        events
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        assert!(matches!(call, Syscall::Getpid(_)));
        // A real injected write completes before the callback returns its error.
        assert_eq!(
            guest
                .inject(
                    reverie::syscalls::Write::default()
                        .with_fd(1)
                        .with_buf(Addr::from_raw(DATA as usize + 256))
                        .with_len(EFFECT.len()),
                )
                .await?,
            EFFECT.len() as i64,
        );
        guest.send_rpc(()).await;
        match guest.config() {
            Mode::Success => Ok(71),
            Mode::Errno => Err(Errno::EBADF.into()),
            Mode::ToolError | Mode::ChildToolError => {
                Err(anyhow::Error::new(AfterEffect(Box::new(71))).into())
            }
        }
    }
}

struct Fixture {
    path: PathBuf,
    trap_rip: u64,
}

impl Fixture {
    fn new(child: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-injected-error-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let mut code = Vec::new();
        if child {
            // The parent blocks in wait4. If a failed child were detached or
            // encoded EIO, it and then its parent would both emit AFTER.
            code.extend_from_slice(&[0xb8, 57, 0, 0, 0, 0x0f, 0x05]); // fork
            code.extend_from_slice(&[0x48, 0x85, 0xc0, 0x0f, 0x84]); // test rax; je child
            let branch = code.len();
            code.extend_from_slice(&[0; 4]);
            code.extend_from_slice(&[0x48, 0x89, 0xc7, 0x31, 0xf6, 0x31, 0xd2, 0x45, 0x31, 0xd2]);
            code.extend_from_slice(&[0xb8, 61, 0, 0, 0, 0x0f, 0x05]); // wait4
            write(&mut code, DATA + 320, AFTER.len());
            code.extend_from_slice(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
            let delta = i32::try_from(code.len() - (branch + 4)).unwrap();
            code[branch..branch + 4].copy_from_slice(&delta.to_le_bytes());
        }
        // Save the actual stack in the logical frame, then invoke the configured
        // marker/RDI/frame/int3 ABI. This is not a mocked handler call.
        code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, frame
        code.extend_from_slice(&DATA.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x89, 0xa7, 128, 0, 0, 0]); // mov [rdi+128],rsp
        code.extend_from_slice(&[0x48, 0xb8]); // movabs rax, marker
        code.extend_from_slice(&MARKER.to_le_bytes());
        code.push(0xcc);
        let trap_rip = ENTRY + code.len() as u64;
        fn write(code: &mut Vec<u8>, buf: u64, len: usize) {
            code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0xbe]);
            code.extend_from_slice(&(buf as u32).to_le_bytes());
            code.push(0xba);
            code.extend_from_slice(&(len as u32).to_le_bytes());
            code.extend_from_slice(&[0x0f, 0x05]);
        }
        write(&mut code, DATA + 120, 8); // exact returned frame RAX
        write(&mut code, DATA + 320, AFTER.len());
        code.extend_from_slice(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
        let mut elf = vec![0u8; 0x3000];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[24..32].copy_from_slice(&ENTRY.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&2u16.to_le_bytes());
        for (header, flags, offset, address, length) in [
            (64, 5u32, 0u64, 0x400000u64, 0x1000 + code.len() as u64),
            (120, 6u32, 0x2000u64, DATA, 0x1000),
        ] {
            elf[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
            elf[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
            elf[header + 16..header + 24].copy_from_slice(&address.to_le_bytes());
            elf[header + 24..header + 32].copy_from_slice(&address.to_le_bytes());
            elf[header + 32..header + 40].copy_from_slice(&length.to_le_bytes());
            elf[header + 40..header + 48].copy_from_slice(&length.to_le_bytes());
            elf[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        }
        elf[0x1000..0x1000 + code.len()].copy_from_slice(&code);
        elf[0x2078..0x2080].copy_from_slice(&(libc::SYS_getpid as u64).to_le_bytes());
        elf[0x2088..0x2090].copy_from_slice(&ENTRY.to_le_bytes());
        elf[0x2100..0x2100 + EFFECT.len()].copy_from_slice(EFFECT);
        elf[0x2140..0x2140 + AFTER.len()].copy_from_slice(AFTER);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(&elf).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .unwrap();
        Self { path, trap_rip }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.path).unwrap();
    }
}

async fn run(mode: Mode, legacy_guard: bool) {
    run_with_transient_refusal(mode, legacy_guard, None).await;
}

async fn run_with_transient_refusal(
    mode: Mode,
    legacy_guard: bool,
    refusal: Option<Arc<AtomicBool>>,
) {
    let fixture = Fixture::new(matches!(mode, Mode::ChildToolError));
    let mut command = Command::new(&fixture.path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut builder = TracerBuilder::<TrapTool>::new(command)
        .config(mode)
        .injected_syscall_trap(MARKER, fixture.trap_rip);
    if legacy_guard {
        // Exercise the production legacy cleanup owner, using the maintained
        // activation bypass. This does not validate a real LiteInst preload.
        builder = builder
            .liteinst_runtime("/not/used.so", 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test();
    }
    if let Some(refusal) = refusal {
        builder = builder.fail_liteinst_discovery_once_for_test(refusal);
    }
    let mut tracer = builder.spawn().await.unwrap();
    let root = tracer.guest_pid();
    let mut stdout = tracer.stdout.take().unwrap();
    let mut stderr = tracer.stderr.take().unwrap();
    // Keep diagnostics without retaining the GlobalTool itself: completion
    // must be allowed to consume its sole Arc after all tasks actually join.
    let observations = Arc::downgrade(&tracer.gref);
    let events = tracer.gref.events.clone();
    let effect_tid = tracer.gref.effect_tid.clone();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let (result, out_result, err_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            tracer.wait(),
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err)
        )
    })
    .await
    .expect("configured injected syscall did not finish within five seconds");
    out_result.unwrap();
    err_result.unwrap();
    assert!(err.is_empty(), "unexpected guest stderr: {err:?}");
    assert!(
        !PathBuf::from(format!("/proc/{root}")).exists(),
        "root was not actually reaped"
    );
    assert!(
        observations.upgrade().is_none(),
        "Tool state still has a backend owner"
    );
    let effect_tid = effect_tid.load(Ordering::SeqCst);
    assert!(effect_tid > 0, "actual callback effect was never observed");
    assert!(
        !PathBuf::from(format!("/proc/{effect_tid}")).exists(),
        "effect task was not reaped"
    );
    if matches!(mode, Mode::ChildToolError) {
        assert_ne!(root.as_raw(), effect_tid);
    }
    match mode {
        Mode::ToolError | Mode::ChildToolError => {
            assert_eq!(*events.lock().unwrap(), ["effect", "failed"]);
            let Error::Tool(error) = result.err().expect("tool failure became a guest result")
            else {
                panic!("typed Tool error was lost");
            };
            assert_eq!(
                *error
                    .downcast_ref::<AfterEffect>()
                    .expect("original typed payload")
                    .0,
                71
            );
            assert_eq!(
                out, EFFECT,
                "guest continued or result frame was exposed after failure"
            );
        }
        Mode::Success | Mode::Errno => {
            let (status, log) = result.unwrap();
            assert_eq!(status, ExitStatus::Exited(0));
            assert_eq!(*log.events.lock().unwrap(), ["effect"]);
            let value: i64 = if matches!(mode, Mode::Success) {
                71
            } else {
                -(libc::EBADF as i64)
            };
            let expected = [EFFECT, value.to_le_bytes().as_slice(), AFTER].concat();
            assert_eq!(out, expected);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn injected_success_preserves_result_and_continuation() {
    run(Mode::Success, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn injected_errno_preserves_result_and_continuation() {
    run(Mode::Errno, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn injected_tool_error_after_effect_is_terminal() {
    run(Mode::ToolError, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn injected_child_tool_error_does_not_resume_child_or_parent() {
    run(Mode::ChildToolError, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_guard_preserves_injected_success_and_errno() {
    run(Mode::Success, true).await;
    run(Mode::Errno, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_guard_reaps_injected_root_and_child_tool_failures() {
    run(Mode::ToolError, true).await;
    run(Mode::ChildToolError, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_guard_refusal_retains_original_owner_and_typed_cause() {
    const NAME: &str =
        "tracer::injected_error_tests::legacy_guard_refusal_retains_original_owner_and_typed_cause";
    const ROLE: &str = "REVERIE_INJECTED_REFUSAL_CHILD";
    const DEADLINE: &str = "REVERIE_INJECTED_REFUSAL_DEADLINE_NS";
    fn monotonic_ns() -> u64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) },
            0
        );
        time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
    }
    if std::env::var(ROLE).as_deref() != Ok(NAME) {
        // Quarantine intentionally closes process-wide admission. Isolate it
        // from parallel libtest workers without widening the five-second bound.
        let deadline = monotonic_ns() + 5_000_000_000;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ROLE, NAME)
            .env(DEADLINE, deadline.to_string())
            .spawn()
            .unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated refusal fixture failed: {status}"
                );
                assert!(
                    monotonic_ns() < deadline,
                    "original five-second deadline expired"
                );
                return;
            }
            if monotonic_ns() >= deadline {
                let signal = child.kill();
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                let reaped = loop {
                    let status = child.try_wait().unwrap();
                    if status.is_some() || Instant::now() >= rescue_deadline {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                panic!(
                    "original five-second deadline failed; separate subprocess rescue: signal={signal:?}, reaped={reaped:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(std::env::args().any(|arg| arg == "--exact"));
    assert!(std::env::args().any(|arg| arg == NAME));
    let deadline_ns = std::env::var(DEADLINE).unwrap().parse::<u64>().unwrap();
    let deadline = tokio::time::Instant::now()
        + Duration::from_nanos(deadline_ns.saturating_sub(monotonic_ns()));
    let fixture = Fixture::new(true);
    let mut command = Command::new(&fixture.path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let refused = Arc::new(AtomicBool::new(true));
    let mut tracer = TracerBuilder::<TrapTool>::new(command)
        .config(Mode::ChildToolError)
        .injected_syscall_trap(MARKER, fixture.trap_rip)
        .liteinst_runtime("/not/used.so", 1, 2, 3, 4)
        .activate_liteinst_without_handshake_for_test()
        .spawn()
        .await
        .unwrap();
    tracer
        .liteinst_cleanup
        .as_mut()
        .unwrap()
        .fail_discovery_while = Some(refused.clone());
    let root = tracer.guest_pid();
    let global = Arc::downgrade(&tracer.gref);
    let events = tracer.gref.events.clone();
    let effect_tid = tracer.gref.effect_tid.clone();
    let result = tokio::time::timeout_at(deadline, tracer.wait_with_output())
        .await
        .expect("injected cleanup refusal exceeded five seconds");
    let error = match result {
        Err(Error::Tool(error)) => error,
        _ => panic!("missing typed refusal"),
    };
    let diagnostic = error
        .downcast_ref::<InjectedCleanupUnconfirmed>()
        .expect("cleanup-unconfirmed type");
    let Error::Tool(original) = diagnostic.failure().primary() else {
        panic!("original Tool cause lost")
    };
    assert_eq!(
        *original
            .downcast_ref::<AfterEffect>()
            .expect("typed original")
            .0,
        71
    );
    assert!(
        refused.load(Ordering::SeqCst),
        "persistent refusal was lifted before the product bound"
    );
    assert!(diagnostic.failure().secondary().iter().any(|failure| {
        failure.origin().phase == "injected tracee cleanup confirmation"
            && matches!(failure.error(), Error::Io(error) if error.raw_os_error() == Some(libc::EIO))
    }));
    assert!(
        diagnostic.failure().secondary().len() > 1,
        "persistent refusal was not retried"
    );
    assert_eq!(*events.lock().unwrap(), ["effect", "failed"]);
    assert!(
        global.upgrade().is_some(),
        "refusal dropped the original global owner"
    );
    assert!(
        matches!(TracerBuilder::<()>::new(Command::new("/bin/true")).spawn().await,
        Err(Error::Tool(error)) if error.downcast_ref::<CleanupAdmissionRefused>().is_some())
    );
    let id = diagnostic.id;
    CLEANUP_QUARANTINE.with(|owners| {
        let owners = owners.borrow();
        let owner = owners
            .get(&id)
            .unwrap()
            .downcast_ref::<LegacyInjectedOwner<Log>>()
            .unwrap();
        assert!(
            Arc::ptr_eq(owner.failure.as_ref().unwrap(), &diagnostic.failure),
            "quarantined owner must retain the exact original failure/prefix allocation"
        );
    });
    // Discard the public marker while the owner is still quarantined. Its
    // complete prefix and original typed cause must survive independently.
    drop(error);
    refused.store(false, Ordering::SeqCst);
    // Test-only rescue removes the exact private retained owner. This is not a
    // production resume API, and no rescue observation certifies product cleanup.
    let mut owner = CLEANUP_QUARANTINE.with(|owners| {
        let owner = std::mem::ManuallyDrop::into_inner(owners.borrow_mut().remove(&id).unwrap());
        *owner
            .downcast::<LegacyInjectedOwner<Log>>()
            .unwrap_or_else(|_| panic!("wrong retained owner type"))
    });
    assert_eq!(owner.tracer.guest_pid(), root);
    assert!(Arc::ptr_eq(&owner.tracer.gref, &global.upgrade().unwrap()));
    owner
        .tracer
        .liteinst_cleanup
        .as_mut()
        .unwrap()
        .terminate_and_confirm()
        .expect("separate test rescue");
    // Restore the exact captured prefix, then read from the original retained
    // reader after rescue. No after-error guest bytes may have appeared, even
    // when the failure was published before the capture driver read EFFECT.
    let retained = owner.failure.as_ref().expect("original retained failure");
    let Error::Tool(original) = retained.primary() else {
        panic!("retained Tool cause lost")
    };
    assert_eq!(
        *original
            .downcast_ref::<AfterEffect>()
            .expect("retained typed original")
            .0,
        71
    );
    let prefix = retained.captured_prefix().expect("capture was requested");
    owner
        .stdout
        .restore_prefix(Some(prefix.stdout().to_vec()))
        .unwrap();
    owner
        .stderr
        .restore_prefix(Some(prefix.stderr().to_vec()))
        .unwrap();
    tokio::time::timeout_at(
        deadline,
        future::poll_fn(|cx| {
            for drain in [&mut owner.stdout, &mut owner.stderr] {
                match drain.poll(cx) {
                    std::task::Poll::Ready(crate::capture::DrainEvent::Error(error)) => {
                        panic!("rescue reader: {error}")
                    }
                    std::task::Poll::Ready(crate::capture::DrainEvent::Progress) => {
                        cx.waker().wake_by_ref()
                    }
                    _ => {}
                }
            }
            if owner.stdout.is_finished() && owner.stderr.is_finished() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }),
    )
    .await
    .expect("original readers did not finish within the same five-second bound");
    assert_eq!(owner.stdout.take_prefix().unwrap().unwrap(), EFFECT);
    assert!(owner.stderr.take_prefix().unwrap().unwrap().is_empty());
    let permit = owner.permit.take().unwrap();
    drop(owner);
    permit.complete();
    assert!(global.upgrade().is_none());
    assert!(!PathBuf::from(format!("/proc/{root}")).exists());
    assert!(!PathBuf::from(format!("/proc/{}", effect_tid.load(Ordering::SeqCst))).exists());
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_guard_capture_and_discard_preserve_success() {
    for discard in [false, true] {
        let fixture = Fixture::new(false);
        let mut command = Command::new(&fixture.path);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let tracer = TracerBuilder::<TrapTool>::new(command)
            .config(Mode::Success)
            .injected_syscall_trap(MARKER, fixture.trap_rip)
            .liteinst_runtime("/not/used.so", 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .spawn()
            .await
            .unwrap();
        let root = tracer.guest_pid();
        tokio::time::timeout(Duration::from_secs(5), async {
            if discard {
                let (status, log) = tracer.wait_discarding_output().await.unwrap();
                assert_eq!(status, ExitStatus::Exited(0));
                assert_eq!(*log.events.lock().unwrap(), ["effect"]);
            } else {
                let (output, log) = tracer.wait_with_output().await.unwrap();
                assert_eq!(output.status, ExitStatus::Exited(0));
                assert_eq!(
                    output.stdout,
                    [EFFECT, 71i64.to_le_bytes().as_slice(), AFTER].concat()
                );
                assert!(output.stderr.is_empty());
                assert_eq!(*log.events.lock().unwrap(), ["effect"]);
            }
        })
        .await
        .expect("legacy capture/discard exceeded five seconds");
        assert!(!PathBuf::from(format!("/proc/{root}")).exists());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_fatal_external_reaper_refusal_keeps_terminal_observation_and_owner() {
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    const NAME: &str = "tracer::injected_error_tests::legacy_fatal_external_reaper_refusal_keeps_terminal_observation_and_owner";
    const ROLE: &str = "REVERIE_INJECTED_EXTERNAL_REAPER_ROLE";
    const DEADLINE: &str = "REVERIE_INJECTED_EXTERNAL_REAPER_DEADLINE";
    fn now_ns() -> u64 {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    }
    fn remaining(deadline: u64) -> Duration {
        let ns = deadline
            .checked_sub(now_ns())
            .expect("single five-second fixture deadline expired");
        assert_ne!(ns, 0);
        Duration::from_nanos(ns)
    }
    fn command(role: &str, deadline: u64) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ROLE, role)
            .env(DEADLINE, deadline.to_string());
        command
    }
    async fn wait_child(child: &mut std::process::Child, deadline: u64) {
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated external-reaper fixture failed: {status}"
                );
                remaining(deadline);
                return;
            }
            if now_ns() >= deadline {
                let signal = child.kill();
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                let reaped = loop {
                    let status = child.try_wait().unwrap();
                    if status.is_some() || Instant::now() >= rescue_deadline {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                panic!(
                    "single five-second deadline failed; separate subprocess rescue: signal={signal:?}, reaped={reaped:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    let role = std::env::var(ROLE).unwrap_or_default();
    if role.is_empty() {
        let deadline = now_ns() + 5_000_000_000;
        let mut child = command("natural-parent", deadline).spawn().unwrap();
        wait_child(&mut child, deadline).await;
        return;
    }
    assert!(std::env::args().any(|arg| arg == "--exact"));
    assert!(std::env::args().any(|arg| arg == NAME));
    let deadline = std::env::var(DEADLINE).unwrap().parse::<u64>().unwrap();
    if role == "natural-parent" {
        // Only this isolated supervisor becomes a subreaper. The ptracer is a
        // separate descendant and has no natural-parent wait authority here.
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
        let (mut channel, child_channel) = UnixStream::pair().unwrap();
        channel.set_read_timeout(Some(remaining(deadline))).unwrap();
        channel
            .set_write_timeout(Some(remaining(deadline)))
            .unwrap();
        let mut child = command("ptracer", deadline)
            .stdin(std::process::Stdio::from(OwnedFd::from(child_channel)))
            .spawn()
            .unwrap();
        // Deliberately do not perform a natural wait until the backend has
        // returned its typed timeout. This is an actual retained zombie, not a
        // fake procfs answer or a live process relabelled as terminal.
        let mut words = [0u64; 3];
        for word in &mut words {
            let mut bytes = [0; 8];
            channel.read_exact(&mut bytes).unwrap();
            *word = u64::from_ne_bytes(bytes);
        }
        let pid = Pid::from_raw(i32::try_from(words[0]).unwrap());
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
        assert!(
            raw >= 0,
            "open exact adopted child pidfd: {}",
            std::io::Error::last_os_error()
        );
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let snapshot = tracee_snapshot(pid).unwrap();
        assert_eq!(snapshot.start_time, words[1]);
        assert_eq!(
            fs::metadata(format!("/proc/{pid}")).unwrap().ino(),
            words[2]
        );
        assert_eq!(snapshot.ppid.as_raw(), std::process::id() as i32);
        assert_eq!(snapshot.tracer_pid.as_raw(), 0);
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    pidfd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(unsafe { info.si_pid() }, pid.as_raw());
        assert_eq!(info.si_code, libc::CLD_KILLED);
        assert_eq!(unsafe { info.si_status() }, libc::SIGKILL);
        assert!(PathBuf::from(format!("/proc/{pid}")).exists());
        // Separate test rescue by the proven natural parent, using its pidfd.
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    pidfd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG,
                )
            },
            0
        );
        assert_eq!(unsafe { info.si_pid() }, pid.as_raw());
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        channel.write_all(&[1]).unwrap();
        wait_child(&mut child, deadline).await;
        return;
    }
    assert_eq!(role, "ptracer");
    let raw = unsafe { libc::dup(libc::STDIN_FILENO) };
    assert!(raw >= 0);
    let mut channel = UnixStream::from(unsafe { OwnedFd::from_raw_fd(raw) });
    channel.set_read_timeout(Some(remaining(deadline))).unwrap();
    channel
        .set_write_timeout(Some(remaining(deadline)))
        .unwrap();
    let fixture = Fixture::new(true);
    let mut guest = Command::new(&fixture.path);
    guest
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let tracer = TracerBuilder::<TrapTool>::new(guest)
        .config(Mode::ChildToolError)
        .injected_syscall_trap(MARKER, fixture.trap_rip)
        .liteinst_runtime("/not/used.so", 1, 2, 3, 4)
        .activate_liteinst_without_handshake_for_test()
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let global = Arc::downgrade(&tracer.gref);
    let events = tracer.gref.events.clone();
    let effect_tid = tracer.gref.effect_tid.clone();
    let before = Instant::now();
    let result = tokio::time::timeout(remaining(deadline), tracer.wait_with_output())
        .await
        .unwrap();
    assert!(
        before.elapsed() >= Duration::from_secs(2),
        "external wait was not withheld through the cleanup bound"
    );
    remaining(deadline);
    let Err(Error::Tool(error)) = result else {
        panic!("terminal observation falsely confirmed")
    };
    let diagnostic = error
        .downcast_ref::<InjectedCleanupUnconfirmed>()
        .expect("typed cleanup refusal");
    assert!(
        matches!(diagnostic.failure().primary(), Error::Tool(original) if original.downcast_ref::<AfterEffect>().is_some())
    );
    assert!(diagnostic.failure().secondary().iter().any(|failure| failure.origin().phase == "injected tracee cleanup confirmation" && matches!(failure.error(), Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut)));
    assert_eq!(*events.lock().unwrap(), ["effect", "failed"]);
    assert!(global.upgrade().is_some());
    assert!(!PathBuf::from(format!("/proc/{root}")).exists());
    let pid = Pid::from_raw(effect_tid.load(Ordering::SeqCst));
    let id = diagnostic.id;
    let identity_words = CLEANUP_QUARANTINE.with(|owners| {
        let owners = owners.borrow();
        let owner = owners
            .get(&id)
            .unwrap()
            .downcast_ref::<LegacyInjectedOwner<Log>>()
            .unwrap();
        assert!(Arc::ptr_eq(
            owner.failure.as_ref().unwrap(),
            &diagnostic.failure
        ));
        let guard = owner.tracer.liteinst_cleanup.as_ref().unwrap();
        assert!(guard.retained_descendants.is_empty());
        assert!(guard.retained_terminal_descendants.is_empty());
        let observations = guard.fatal_terminal_observations.as_ref().unwrap();
        assert_eq!(observations.len(), 1);
        let identity = observations
            .get(&pid)
            .expect("exact terminal generation retained");
        assert!(identity.observe_same_process().unwrap());
        assert!(
            !terminal_descendant_remains_owned(identity),
            "observation acquired ownership"
        );
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        assert!(status.lines().any(|line| line.starts_with("State:\tZ")));
        [
            pid.as_raw() as u64,
            identity.snapshot.start_time,
            identity.proc_inode,
        ]
    });
    assert!(
        matches!(TracerBuilder::<()>::new(Command::new("/bin/true")).spawn().await, Err(Error::Tool(error)) if error.downcast_ref::<CleanupAdmissionRefused>().is_some())
    );
    // Marker disposal cannot discard the captured prefix or original cause.
    drop(error);
    for word in identity_words {
        channel.write_all(&word.to_ne_bytes()).unwrap();
    }
    let mut ack = [0];
    channel.read_exact(&mut ack).unwrap();
    assert_eq!(ack, [1]);
    let mut owner = CLEANUP_QUARANTINE.with(|owners| {
        let owner = std::mem::ManuallyDrop::into_inner(owners.borrow_mut().remove(&id).unwrap());
        *owner
            .downcast::<LegacyInjectedOwner<Log>>()
            .unwrap_or_else(|_| panic!("wrong retained owner"))
    });
    owner
        .tracer
        .liteinst_cleanup
        .as_mut()
        .unwrap()
        .terminate_and_confirm()
        .expect("separate test rescue after natural-parent reap");
    let retained = owner.failure.as_ref().unwrap();
    assert!(
        matches!(retained.primary(), Error::Tool(original) if original.downcast_ref::<AfterEffect>().is_some())
    );
    let prefix = retained.captured_prefix().unwrap();
    owner
        .stdout
        .restore_prefix(Some(prefix.stdout().to_vec()))
        .unwrap();
    owner
        .stderr
        .restore_prefix(Some(prefix.stderr().to_vec()))
        .unwrap();
    tokio::time::timeout(
        remaining(deadline),
        future::poll_fn(|cx| {
            for drain in [&mut owner.stdout, &mut owner.stderr] {
                match drain.poll(cx) {
                    std::task::Poll::Ready(crate::capture::DrainEvent::Error(error)) => {
                        panic!("retained reader: {error}")
                    }
                    std::task::Poll::Ready(crate::capture::DrainEvent::Progress) => {
                        cx.waker().wake_by_ref()
                    }
                    _ => {}
                }
            }
            if owner.stdout.is_finished() && owner.stderr.is_finished() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(owner.stdout.take_prefix().unwrap().unwrap(), EFFECT);
    assert!(owner.stderr.take_prefix().unwrap().unwrap().is_empty());
    let permit = owner.permit.take().unwrap();
    drop(owner);
    permit.complete();
    assert!(global.upgrade().is_none());
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    remaining(deadline);
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_transient_cleanup_refusal_recovers_with_admission_open() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let refusal = Arc::new(AtomicBool::new(true));
        run_with_transient_refusal(Mode::ChildToolError, true, Some(refusal.clone())).await;
        assert!(
            !refusal.load(Ordering::SeqCst),
            "one-shot refusal was not exercised"
        );
        let tracer = TracerBuilder::<()>::new(Command::new("/bin/true"))
            .spawn()
            .await
            .expect("confirmed transient cleanup must leave admission open");
        assert_eq!(tracer.wait().await.unwrap().0, ExitStatus::Exited(0));
    })
    .await
    .expect("transient cleanup and new admission exceeded five seconds");
}

#[tokio::test(flavor = "current_thread")]
async fn static_injected_cleanup_resource_follows_original_pending_owner() {
    const NAME: &str = "tracer::injected_error_tests::static_injected_cleanup_resource_follows_original_pending_owner";
    const ROLE: &str = "REVERIE_STATIC_RESOURCE_CHILD";
    const DEADLINE: &str = "REVERIE_STATIC_RESOURCE_DEADLINE";
    fn now_ns() -> u64 {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    }
    if std::env::var(ROLE).as_deref() != Ok(NAME) {
        let deadline = now_ns() + 5_000_000_000;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ROLE, NAME)
            .env(DEADLINE, deadline.to_string())
            .spawn()
            .unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated resource fixture failed: {status}"
                );
                assert!(now_ns() < deadline, "original five-second deadline expired");
                return;
            }
            if now_ns() >= deadline {
                let signal = child.kill();
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                let rescue = loop {
                    let status = child.try_wait().unwrap();
                    if status.is_some() || Instant::now() >= rescue_deadline {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                panic!(
                    "five-second resource fixture deadline failed; separate rescue: {signal:?}, {rescue:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    assert!(std::env::args().any(|arg| arg == NAME));
    assert!(std::env::args().any(|arg| arg == "--exact"));
    let deadline = tokio::time::Instant::now()
        + Duration::from_nanos(
            std::env::var(DEADLINE)
                .unwrap()
                .parse::<u64>()
                .unwrap()
                .saturating_sub(now_ns()),
        );

    struct Resource {
        attempts: Arc<std::sync::atomic::AtomicUsize>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
        root: Pid,
        effect: Arc<AtomicI32>,
    }
    impl PtraceCleanupResource for Resource {
        fn cleanup(&mut self) -> Result<(), Error> {
            assert!(!PathBuf::from(format!("/proc/{}", self.root)).exists());
            assert!(
                !PathBuf::from(format!("/proc/{}", self.effect.load(Ordering::SeqCst))).exists()
            );
            assert!(
                OrdinaryAdmission::acquire().is_err(),
                "admission reopened before resource cleanup"
            );
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(std::io::Error::from_raw_os_error(libc::EIO).into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Resource {
        fn drop(&mut self) {
            assert!(
                OrdinaryAdmission::acquire().is_err(),
                "resource dropped after admission reopened"
            );
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    async fn check<R: 'static>(error: Error, root: Pid, effect: Arc<AtomicI32>, capture: bool) {
        let Error::Tool(error) = error else {
            panic!("static cleanup marker lost")
        };
        let marker = error
            .downcast_ref::<CleanupUnconfirmed>()
            .expect("existing public recovery route");
        assert!(
            matches!(marker.primary(), Error::Tool(error) if error.downcast_ref::<AfterEffect>().is_some())
        );
        assert!(matches!(
            marker.take_cleanup::<(), R>(),
            Err(CleanupLookupError::WrongType)
        ));
        // Verify an actual original pidfd plus active namespace, and its refusal
        // before registry access for a foreign numeric process identity.
        marker.verify_owner().unwrap();
        let mut foreign = CleanupOwnerIdentity::capture().unwrap();
        foreign.pid = if foreign.pid == 1 { 2 } else { 1 };
        assert!(matches!(
            foreign.verify(),
            Err(CleanupLookupError::WrongProcess)
        ));
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let refused_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let refused_drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let unbound = if !capture {
            assert!(matches!(
                marker.retain_cleanup_resource::<(), R, _>(Resource {
                    attempts: refused_attempts.clone(),
                    drops: refused_drops.clone(),
                    root,
                    effect: effect.clone(),
                }),
                Err(CleanupLookupError::WrongType)
            ));
            assert_eq!(
                refused_drops.load(Ordering::SeqCst),
                0,
                "lookup refusal dropped exact resource"
            );
            Some(CLEANUP_QUARANTINE.with(|owners| {
                *owners
                    .borrow()
                    .iter()
                    .find(|(_, owner)| owner.is::<UnboundCleanupResource>())
                    .expect("separate retained unbound owner")
                    .0
            }))
        } else {
            None
        };
        marker
            .retain_cleanup_resource::<Log, R, _>(Resource {
                attempts: attempts.clone(),
                drops: drops.clone(),
                root,
                effect,
            })
            .unwrap();
        let id = marker.id;
        let pending = if capture {
            // Marker disposal while still quarantined must retain the guard.
            // Removal below is explicitly white-box test rescue, not a promised
            // recovery API for a caller which discarded its diagnostic.
            drop(error);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            CLEANUP_QUARANTINE.with(|owners| {
                let owner =
                    std::mem::ManuallyDrop::into_inner(owners.borrow_mut().remove(&id).unwrap());
                *owner
                    .downcast::<PendingPtraceCleanup<Log, R>>()
                    .unwrap_or_else(|_| panic!("wrong original owner"))
            })
        } else {
            let owner = marker.take_cleanup::<Log, R>().unwrap();
            drop(error);
            owner
        };
        assert_eq!(pending.driver.resources.len(), 1);
        let ToolRunOutcome::CleanupPending(pending) = pending.resume_cleanup().await else {
            panic!("actual resource cleanup error was hidden")
        };
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(pending.failure().secondary().iter().any(|failure| failure.origin().phase == "ptrace retained cleanup resource" && matches!(failure.error(), Error::Io(error) if error.raw_os_error() == Some(libc::EIO))));
        if capture {
            assert_eq!(
                pending.failure().captured_prefix().unwrap().stdout(),
                EFFECT
            );
            assert!(
                pending
                    .failure()
                    .captured_prefix()
                    .unwrap()
                    .stderr()
                    .is_empty()
            );
        } else {
            assert!(pending.failure().captured_prefix().is_none());
        }
        let error = pending.quarantine();
        let Error::Tool(error) = error else {
            unreachable!()
        };
        let marker = error.downcast_ref::<CleanupUnconfirmed>().unwrap();
        assert_eq!(
            marker.recovery_key(),
            id,
            "repeat quarantine changed original owner key"
        );
        let pending = marker.take_cleanup::<Log, R>().unwrap();
        assert_eq!(
            pending.driver.resources.len(),
            1,
            "repeat quarantine duplicated/lost guard"
        );
        drop(error);
        let ToolRunOutcome::Complete(completion) = pending.resume_cleanup().await else {
            panic!("same retained resource did not recover")
        };
        let failure = completion
            .result
            .err()
            .expect("original Tool failure became success");
        assert!(
            matches!(failure.primary(), Error::Tool(error) if error.downcast_ref::<AfterEffect>().is_some())
        );
        assert!(
            failure
                .secondary()
                .iter()
                .any(|failure| failure.origin().phase == "ptrace retained cleanup resource")
        );
        if capture {
            assert_eq!(failure.captured_prefix().unwrap().stdout(), EFFECT);
        } else {
            assert!(failure.captured_prefix().is_none());
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        if let Some(id) = unbound {
            assert!(
                OrdinaryAdmission::acquire().is_err(),
                "original completion reopened admission over an unbound guard"
            );
            assert_eq!(refused_drops.load(Ordering::SeqCst), 0);
            assert_eq!(
                refused_attempts.load(Ordering::SeqCst),
                0,
                "unbound guard acquired cleanup authority"
            );
            // Explicit white-box test rescue only; production deliberately has
            // no lookup/recovery API for an unbound attachment.
            let owner = CLEANUP_QUARANTINE.with(|owners| {
                let owner =
                    std::mem::ManuallyDrop::into_inner(owners.borrow_mut().remove(&id).unwrap());
                *owner
                    .downcast::<UnboundCleanupResource>()
                    .unwrap_or_else(|_| panic!("wrong unbound owner"))
            });
            let UnboundCleanupResource {
                _resource: resource,
                _permit: permit,
            } = owner;
            drop(resource);
            permit.complete();
            assert_eq!(refused_drops.load(Ordering::SeqCst), 1);
        }
        assert!(OrdinaryAdmission::acquire().is_ok());
    }

    tokio::time::timeout_at(deadline, async {
        for capture in [true, false] {
            let control = Arc::new(crate::task::FatalFreezeControl::default());
            crate::task::FATAL_FREEZE_CONTROL.with(|slot| *slot.borrow_mut() = Some(control));
            let fixture = Fixture::new(true);
            let mut command = Command::new(&fixture.path);
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            let tracer = TracerBuilder::<TrapTool>::new(command)
                .config(Mode::ChildToolError)
                .injected_syscall_trap(MARKER, fixture.trap_rip)
                .spawn()
                .await
                .unwrap();
            assert!(
                tracer.termination_handle().is_none(),
                "normal public supervisor support changed"
            );
            let root = tracer.guest_pid();
            let effect = tracer.gref.effect_tid.clone();
            let global = Arc::downgrade(&tracer.gref);
            if capture {
                let error = tracer
                    .wait_with_output()
                    .await
                    .err()
                    .expect("missing failed captured cleanup");
                crate::task::FATAL_FREEZE_CONTROL.with(|slot| *slot.borrow_mut() = None);
                check::<Output>(error, root, effect, true).await;
            } else {
                let error = tracer
                    .wait_discarding_output()
                    .await
                    .err()
                    .expect("missing failed discarded cleanup");
                crate::task::FATAL_FREEZE_CONTROL.with(|slot| *slot.borrow_mut() = None);
                check::<ExitStatus>(error, root, effect, false).await;
            }
            assert!(global.upgrade().is_none());
        }
    })
    .await
    .expect("static retained resource cleanup exceeded original five-second deadline");
}
