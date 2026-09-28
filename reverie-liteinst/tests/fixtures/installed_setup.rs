//! Real mapped configuration peers in a fresh controlled native host.
use std::io;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::OnceLock;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
use reverie_liteinst::rpc::CoordinatorRpc;
use reverie_liteinst::rpc::InstalledCoordinator;
use reverie_liteinst::rpc::InstalledSetupListener;
use reverie_rpc_transport::guest_log as g;
use reverie_rpc_transport::mapped::MappedStream;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

static MODE: OnceLock<String> = OnceLock::new();
static DECODES: AtomicUsize = AtomicUsize::new(0);
static CLONES: AtomicUsize = AtomicUsize::new(0);
static SAVED_PRODUCERS: OnceLock<(
    reverie_liteinst::MappedLogProducer,
    reverie_liteinst::MappedLogProducer,
)> = OnceLock::new();
const DECODE_PANIC: &str = "installed setup intentional decode panic";
const CLONE_PANIC: &str = "installed setup intentional clone panic";
#[derive(Default)]
struct Config(u64);
impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        super::audit();
        DECODES.fetch_add(1, Ordering::Relaxed);
        if MODE.get().is_some_and(|mode| mode.starts_with("panic-")) {
            assert!(
                SAVED_PRODUCERS
                    .set(reverie_liteinst::mapped_log_producers().unwrap())
                    .is_ok()
            );
        }
        if MODE.get().is_some_and(|mode| mode == "panic-decode") {
            std::panic::panic_any(DECODE_PANIC);
        }
        if MODE.get().is_some_and(|mode| mode == "slow-decode") {
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(Self(value))
    }
}
impl Clone for Config {
    fn clone(&self) -> Self {
        super::audit();
        CLONES.fetch_add(1, Ordering::Relaxed);
        if MODE.get().is_some_and(|mode| mode == "panic-clone") {
            std::panic::panic_any(CLONE_PANIC);
        }
        if MODE.get().is_some_and(|mode| mode == "slow-clone") {
            std::thread::sleep(Duration::from_millis(250));
        }
        Self(self.0)
    }
}
#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = Config;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut result = (bytes.len() as u32).to_be_bytes().to_vec();
    result.extend_from_slice(bytes);
    result
}
fn retire_observation(
    stream: &mut MappedStream,
    deadline: Instant,
) -> (io::ErrorKind, io::ErrorKind) {
    let read = stream
        .read_exact_until(&mut [0], deadline)
        .unwrap_err()
        .kind();
    let write = stream.write_all_until(b"x", deadline).unwrap_err().kind();
    (read, write)
}
pub fn client(directory: &Path, mode: &str) {
    let live = mode.ends_with("-alive");
    let mode = mode.strip_suffix("-alive").unwrap_or(mode);
    MODE.set(mode.to_owned()).unwrap();
    let endpoint =
        InstalledCoordinator::from_bytes(&std::fs::read(directory.join("setup.identity")).unwrap())
            .unwrap();
    let start = Instant::now();
    let construct = || unsafe {
        CoordinatorRpc::<Global>::connect_installed(
            endpoint,
            Duration::from_millis(200),
            Duration::from_millis(200),
        )
    };
    let description = if mode.starts_with("panic-") {
        let expected = if mode == "panic-decode" {
            DECODE_PANIC
        } else {
            assert_eq!(mode, "panic-clone");
            CLONE_PANIC
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(construct));
        let Err(payload) = result else {
            panic!("constructor did not preserve its configuration panic")
        };
        assert_eq!(payload.downcast_ref::<&str>(), Some(&expected));
        let &(public, private) = SAVED_PRODUCERS.get().unwrap();
        for producer in [public, private] {
            assert!(matches!(
                producer.write_record(b"must not publish after setup failure\n"),
                Err(g::PublishError::Stopped)
            ));
        }
        format!("panicked: {expected}; copied producers stopped")
    } else {
        match construct() {
            Ok(rpc) => {
                assert_eq!(mode, "complete");
                assert_eq!(rpc.config().0, 0x12345678);
                drop(rpc);
                "complete".to_owned()
            }
            Err(error) => {
                assert_ne!(mode, "complete");
                format!("{:?}: {error}", error.kind())
            }
        }
    };
    let elapsed = start.elapsed();
    super::audit();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    assert!(
        !maps.contains("memfd:reverie-rpc"),
        "fresh main/statistics mappings survived transaction disposal"
    );
    std::fs::write(directory.join("client-maps.txt"), maps).unwrap();
    std::fs::write(
        directory.join("client-result.txt"),
        format!(
            "{description}; elapsed={elapsed:?}; decodes={}; clones={}\n",
            DECODES.load(Ordering::Relaxed),
            CLONES.load(Ordering::Relaxed)
        ),
    )
    .unwrap();
    assert!(
        elapsed < Duration::from_millis(500),
        "configuration ignored original setup deadline: {elapsed:?}"
    );
    if mode == "statistics-silent" {
        assert!(
            elapsed < Duration::from_millis(300),
            "statistics reset the setup deadline after the delayed main frame: {elapsed:?}"
        );
    }
    let expected_decode = usize::from(matches!(
        mode,
        "complete"
            | "statistics-silent"
            | "slow-decode"
            | "slow-clone"
            | "panic-decode"
            | "panic-clone"
    ));
    assert_eq!(DECODES.load(Ordering::Relaxed), expected_decode);
    assert_eq!(
        CLONES.load(Ordering::Relaxed),
        usize::from(matches!(mode, "complete" | "slow-clone" | "panic-clone"))
    );
    if mode.ends_with("silent") || mode.ends_with("stall") || mode.starts_with("slow-") {
        assert!(description.contains("timed out"), "{description}");
    } else if mode == "clean-eof" {
        assert_eq!(description, "Other: peer closed the connection");
    } else if mode != "complete" && !mode.starts_with("panic-") {
        assert!(
            description.contains("unexpected end of file")
                || description.contains("failed to fill whole buffer"),
            "{description}"
        );
    }
    if live {
        let available = reverie_liteinst::mapped_log_producers().is_some();
        std::fs::write(
            directory.join("client-alive.txt"),
            format!("producers_available={available}\n"),
        )
        .unwrap();
        while !directory.join("host-observed.txt").is_file() {
            assert!(
                start.elapsed() < Duration::from_millis(500),
                "host did not observe constructor failure while caller remained alive"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !available,
            "failed installation still advertised active producers"
        );
    }
}
pub fn host(directory: &Path, mode: &str) {
    let requested_mode = mode;
    let live = mode.ends_with("-alive");
    let mode = mode.strip_suffix("-alive").unwrap_or(mode);
    let mut owner = super::ownership::ControlledHost::enter().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let limits = g::ordered::Limits {
            producers: 4,
            slots: 4,
            max_record_bytes: 1024,
            host_pending_bytes: 4096,
            guest_pending_bytes: 4096,
            pending_records: 8,
        };
        let (public, public_fd) = unsafe { g::ordered::Buffer::create(limits) }.unwrap();
        let (private, private_fd) = unsafe { g::ordered::Buffer::create(limits) }.unwrap();
        let socket_directory = tempfile::Builder::new()
            .prefix("li-setup-")
            .tempdir()
            .unwrap();
        let mut listener = unsafe {
            InstalledSetupListener::bind(
                socket_directory.path().join("s"),
                public_fd,
                private_fd,
                true,
                libc::geteuid(),
                libc::getegid(),
            )
        }
        .unwrap();
        std::fs::write(
            directory.join("setup.identity"),
            listener.coordinator().to_bytes(),
        )
        .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("setup-client")
            .arg(directory)
            .arg(requested_mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        owner.spawn_root(&mut command).unwrap();
        owner.close_root_stdin();
        owner.close_admission();
        let mut set = unsafe { listener.accept(4096, deadline) }.unwrap();
        let payload = reverie_rpc_transport::codec::encode(&Config(0x12345678)).unwrap();
        let main = frame(&payload);
        match mode {
            "complete" | "slow-decode" | "slow-clone" | "panic-decode" | "panic-clone" => {
                set.main.write_all_until(&main, deadline).unwrap();
                let statistics = frame(&reverie_rpc_transport::codec::encode(&()).unwrap());
                set.statistics
                    .as_mut()
                    .unwrap()
                    .write_all_until(&statistics, deadline)
                    .unwrap();
            }
            "statistics-silent" => {
                std::thread::sleep(Duration::from_millis(150));
                set.main.write_all_until(&main, deadline).unwrap();
            }
            "main-silent" => {}
            "clean-eof" => set.main.close_write(),
            "header-stall" | "header-eof" => {
                set.main.write_all_until(&main[..3], deadline).unwrap();
                if mode.ends_with("eof") {
                    set.main.close_write();
                }
            }
            "body-stall" | "body-eof" => {
                set.main
                    .write_all_until(&main[..main.len() - 1], deadline)
                    .unwrap();
                if mode.ends_with("eof") {
                    set.main.close_write();
                }
            }
            _ => panic!("unknown setup case"),
        }
        let observation_deadline = Instant::now() + Duration::from_millis(600);
        if live {
            while !directory.join("client-alive.txt").is_file() {
                owner.poll(deadline).unwrap();
                assert!(
                    Instant::now() < observation_deadline,
                    "live caller marker missing"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            owner.poll(deadline).unwrap();
            let snapshot = owner.snapshot();
            let mut public_collector = public.collector().unwrap();
            let mut private_collector = private.collector().unwrap();
            assert!(public_collector.poll().unwrap().is_none());
            assert!(private_collector.poll().unwrap().is_none());
            let failures = [public.guest_failed(), private.guest_failed()];
            let complete = [
                public_collector.guest_complete(),
                private_collector.guest_complete(),
            ];
            let streams = [
                public_collector.diagnostics(4096).0,
                private_collector.diagnostics(4096).0,
            ];
            std::fs::write(directory.join("live-capture-state.txt"), format!("failures={failures:?}; complete={complete:?}; streams={streams:?}\n{snapshot:#?}\n")).unwrap();
            assert_eq!(
                snapshot.root_status, None,
                "caller exited before capture failure was observed"
            );
            assert!(!snapshot.complete && !snapshot.echild);
            assert_eq!(
                failures,
                [true, true],
                "configuration failure left a capture healthy"
            );
            assert_eq!(complete, [false, false]);
            assert!(
                streams.iter().flatten().all(|stream| !stream.finished),
                "failure fabricated FINISH"
            );
            assert_eq!(
                std::fs::read_to_string(directory.join("client-alive.txt")).unwrap(),
                "producers_available=false\n"
            );
            std::fs::write(
                directory.join("host-observed.txt"),
                b"observed while alive\n",
            )
            .unwrap();
        }
        while !owner.snapshot().complete && Instant::now() < observation_deadline {
            owner.poll(deadline).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        let snapshot = owner.snapshot();
        std::fs::write(
            directory.join("setup-processes.txt"),
            format!("{snapshot:#?}\n"),
        )
        .unwrap();
        assert!(
            snapshot.complete,
            "client still blocked after its original setup deadline"
        );
        assert_eq!(snapshot.root_status, Some(0));
        assert!(snapshot.signals.is_empty());
        assert!(snapshot.echild);
        if mode.starts_with("panic-") {
            let expected = if mode == "panic-decode" {
                DECODE_PANIC
            } else {
                CLONE_PANIC
            };
            let stderr = std::str::from_utf8(&snapshot.stderr).unwrap();
            assert!(snapshot.stdout.is_empty());
            assert_eq!(stderr.matches(expected).count(), 1, "{snapshot:?}");
            assert_eq!(stderr.matches("panicked at").count(), 1, "{snapshot:?}");
            std::fs::write(
                directory.join("configuration-panic.stderr"),
                &snapshot.stderr,
            )
            .unwrap();
        } else {
            assert!(
                snapshot.stdout.is_empty() && snapshot.stderr.is_empty(),
                "{snapshot:?}"
            );
        }
        if mode == "complete" {
            assert!(!public.guest_failed() && !private.guest_failed());
        }
        let main = retire_observation(&mut set.main, deadline);
        let stats = retire_observation(set.statistics.as_mut().unwrap(), deadline);
        std::fs::write(
            directory.join("peer-retirement.txt"),
            format!("main={main:?}; statistics={stats:?}\n"),
        )
        .unwrap();
        assert_eq!(
            main,
            (io::ErrorKind::UnexpectedEof, io::ErrorKind::BrokenPipe)
        );
        assert_eq!(
            stats,
            (io::ErrorKind::UnexpectedEof, io::ErrorKind::BrokenPipe)
        );
    }));
    if let Err(payload) = result {
        owner.cancel();
        let cleanup = owner.drain_until(deadline);
        std::fs::write(
            directory.join("setup-failed-cleanup.txt"),
            format!("{cleanup:?}\n{:?}\n", owner.snapshot()),
        )
        .unwrap();
        std::panic::resume_unwind(payload);
    }
    println!("installed setup complete: {requested_mode}");
}
