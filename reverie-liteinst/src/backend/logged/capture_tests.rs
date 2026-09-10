use std::io;
use std::io::Write;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use reverie_rpc_transport::guest_log::CaptureDestination;
use reverie_rpc_transport::guest_log::CaptureLimits;
use reverie_rpc_transport::guest_log::CaptureOptions;
use reverie_rpc_transport::guest_log::CaptureTimeouts;
use reverie_rpc_transport::guest_log::DestinationProgress;
use reverie_rpc_transport::guest_log::HostProducer;
use reverie_rpc_transport::guest_log::prepared_capture;

use super::*;

#[derive(Default)]
struct Destination {
    bytes: Arc<Mutex<Vec<u8>>>,
    progress: DestinationProgress,
}
impl Write for Destination {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        self.progress.acknowledged_data_bytes += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl CaptureDestination for Destination {
    fn progress(&self) -> DestinationProgress {
        self.progress
    }
}
fn options() -> CaptureOptions {
    CaptureOptions {
        limits: CaptureLimits {
            producers: 4,
            slots_per_producer: 4,
            max_record_bytes: 1024,
            host_pending_bytes: 8192,
            guest_pending_bytes: 8192,
            pending_records: 8,
            diagnostic_bytes: 1024,
        },
        timeouts: CaptureTimeouts {
            startup: Duration::from_secs(2),
            blocked_publication: Duration::from_secs(2),
            final_drain: Duration::from_millis(20),
        },
    }
}

#[test]
fn owned_cleanup_unwind_survives_full_capture_issue_budget() {
    struct PanickingOwner;
    impl Drop for PanickingOwner {
        fn drop(&mut self) {
            panic!("owner destructor panic after primary failure");
        }
    }
    let (mut capture, sink, host) = prepared_capture(options(), Destination::default()).unwrap();
    let handle = capture.handle();
    for index in 0..32 {
        handle.issue(IssueKind::Child, format!("existing issue {index}"));
    }
    assert_eq!(handle.snapshot().issues.len(), 32);
    let prepared =
        owned::PreparedCommand::new(std::process::Command::new("/bin/true"), PanickingOwner)
            .with_spawn_check(|_, _| Err(io::Error::other("original primary failure").into()));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(future).unwrap_err();
    assert!(error.to_string().contains("original primary failure"));
    assert_eq!(error.logs.snapshot().issues.len(), 32);
    let state = observer.try_snapshot().unwrap();
    assert!(!state.spawned);
    assert!(
        state
            .first_error
            .unwrap()
            .display
            .contains("original primary failure")
    );
    let cleanup = state.cleanup_issue.unwrap();
    assert_eq!(cleanup.kind, IssueKind::Cleanup);
    assert_eq!(cleanup.message, "owned launch cleanup unwound");
    drop(host);
    let report = capture.finish_until(Instant::now() + Duration::from_secs(2));
    assert!(report.omitted_issues > 0);
    assert!(!report.qualifies());
}

#[test]
fn prepared_unpolled_both_adapters_keep_host_cleanup_outside_runtime() {
    for inherited in [false, true] {
        let output = Destination::default();
        let bytes = output.bytes.clone();
        let (mut owner, sink, host) = prepared_capture(options(), output).unwrap();
        let handle = owner.handle();
        if inherited {
            drop(
                LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<()>(
                    Command::new("/bin/true"),
                    (),
                    "/bin/true",
                    Vec::new(),
                    sink,
                ),
            );
        } else {
            drop(
                LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(
                    Command::new("/bin/true"),
                    (),
                    "/bin/true",
                    Vec::new(),
                    sink,
                ),
            );
        }
        host.write_record(b"cleanup after unpolled run").unwrap();
        let report = owner.finish_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(handle.snapshot().run, RunState::Cancelled);
        assert_eq!(report.guest.phase, Phase::Incomplete);
        assert!(!report.qualifies());
        assert_eq!(&*bytes.lock().unwrap(), b"cleanup after unpolled run");
    }
}

#[test]
fn prepared_prepare_error_both_adapters_preserves_cause_and_cleanup() {
    for inherited in [false, true] {
        let output = Destination::default();
        let bytes = output.bytes.clone();
        let (mut owner, sink, host) = prepared_capture(options(), output).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            if inherited {
                LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<()>(
                    Command::new("/bin/true"),
                    (),
                    "/missing-common-capture-preload",
                    Vec::new(),
                    sink,
                )
                .await
            } else {
                LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(
                    Command::new("/bin/true"),
                    (),
                    "/missing-common-capture-preload",
                    Vec::new(),
                    sink,
                )
                .await
            }
        });
        let error = result.unwrap_err();
        assert!(error.to_string().contains("No such file"));
        assert!(error.logs.capture_snapshot().is_some());
        drop(runtime);
        host.write_record(b"cleanup after error").unwrap();
        let report = owner.finish_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(report.guest.run, RunState::Failed);
        assert!(!report.guest.root_reaped);
        assert_eq!(&*bytes.lock().unwrap(), b"cleanup after error");
    }
}

static INIT_COUNT: AtomicUsize = AtomicUsize::new(0);
static PRODUCER: Mutex<Option<HostProducer>> = Mutex::new(None);

struct InitGuard;
impl Drop for InitGuard {
    fn drop(&mut self) {
        PRODUCER
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .write_record(b"init drop\n")
            .unwrap();
    }
}
#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = ();
    type Response = ();
    type Config = ();
    async fn init_global_state(_: &()) -> Self {
        PRODUCER
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .write_record(b"init\n")
            .unwrap();
        let _guard = InitGuard;
        INIT_COUNT.fetch_add(1, Ordering::Release);
        std::future::pending().await
    }
    async fn receive_rpc(&self, _: reverie::Tid, _: ()) {}
}
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct InitTool;
#[reverie::tool]
impl Tool for InitTool {
    type GlobalState = Global;
    type ThreadState = ();
}

#[test]
fn prepared_pending_init_both_adapters_retains_source_emission_and_drop() {
    for (index, inherited) in [false, true].into_iter().enumerate() {
        let output = Destination::default();
        let bytes = output.bytes.clone();
        let (mut owner, sink, host) = prepared_capture(options(), output).unwrap();
        *PRODUCER.lock().unwrap() = Some(host.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut future: std::pin::Pin<Box<dyn Future<Output = _>>> = if inherited {
                Box::pin(LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<InitTool>(Command::new("/bin/true"), (), "/bin/true", Vec::new(), sink))
            } else {
                Box::pin(LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<InitTool>(Command::new("/bin/true"), (), "/bin/true", Vec::new(), sink))
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::select! {
                    _ = &mut future => panic!("pending init returned"),
                    _ = async { while INIT_COUNT.load(Ordering::Acquire) <= index { tokio::task::yield_now().await; } } => {},
                }
            }).await.unwrap();
            drop(future);
        });
        drop(runtime);
        host.write_record(b"after runtime\n").unwrap();
        let report = owner.finish_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(report.guest.phase, Phase::Incomplete);
        assert!(!report.guest.root_reaped);
        assert_eq!(&*bytes.lock().unwrap(), b"init\ninit drop\nafter runtime\n");
        PRODUCER.lock().unwrap().take();
    }
}

#[test]
fn prepared_v4_bootstrap_identity_and_mapping_are_distinct() {
    use std::os::fd::IntoRawFd;
    let (mut owner, mut sink, _host) = prepared_capture(options(), Destination::default()).unwrap();
    let endpoint = sink.take_prepared_endpoint().unwrap().unwrap();
    let fd = endpoint.into_raw_fd();
    let packet =
        create_versioned_log_bootstrap(Path::new("/tmp/rpc"), b"sealed-tool", Some(fd), true, true)
            .unwrap();
    let mut magic = [0; 16];
    assert_eq!(
        unsafe { libc::pread(packet.as_raw_fd(), magic.as_mut_ptr().cast(), 16, 0) },
        16
    );
    assert_eq!(&magic, ORDERED_LOG_BOOTSTRAP_MAGIC);
    assert_ne!(&magic, BUFFERED_LOG_BOOTSTRAP_MAGIC);
    let decoded = read_preload_bootstrap(packet.as_raw_fd()).unwrap().unwrap();
    assert_eq!(decoded.tool_data, b"sealed-tool");
    assert_eq!(decoded.coordinator, Path::new("/tmp/rpc"));
    assert!(decoded.log.is_some());
    assert!(create_versioned_log_bootstrap(Path::new("/tmp/rpc"), b"", None, true, true).is_err());
    drop(decoded);
    assert!(
        !owner
            .finish_until(Instant::now() + Duration::from_secs(2))
            .qualifies()
    );
}
