//! Single-threaded exec boundary for the deferred API. The integration parent
//! owns its process group and the unchanged 50-second lifecycle deadline.
use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use reverie_rpc_transport::guest_log as g;

struct Destination {
    bytes: Arc<Mutex<Vec<u8>>>,
    count: u64,
}
impl Write for Destination {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        self.count += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl g::CaptureDestination for Destination {
    fn progress(&self) -> g::DestinationProgress {
        g::DestinationProgress {
            acknowledged_data_bytes: self.count,
            ..Default::default()
        }
    }
}
fn identity() -> (u32, String, Vec<u32>) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let start = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned();
    let mut tids: Vec<u32> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    tids.sort();
    (std::process::id(), start, tids)
}
fn wait(_: &g::SharedBuffer, _: u32) -> Result<(), g::PublishError> {
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}
pub fn dispatch(args: &[String]) -> bool {
    if args.get(1).map(String::as_str) == Some("--deferred-guest") {
        let fd: i32 = args[2].parse().unwrap();
        let before = identity();
        assert_eq!(before.2, vec![before.0]);
        let buffer = unsafe { g::ordered::Buffer::receive(fd) }.unwrap();
        let mut guest = unsafe { buffer.activate(1, i64::from(std::process::id())) }.unwrap();
        assert_eq!(
            guest.write_record(b"guest original\n", wait).unwrap().order,
            3
        );
        guest.finish(wait).unwrap();
        println!(
            "guest actual FINISH pid={} start={} tasks={:?}",
            before.0, before.1, before.2
        );
        return true;
    }
    if args.get(1).map(String::as_str) != Some("--deferred-capture") {
        return false;
    }
    let options = g::CaptureOptions {
        limits: g::CaptureLimits {
            producers: 4,
            slots_per_producer: 16,
            max_record_bytes: 4096,
            host_pending_bytes: 8192,
            guest_pending_bytes: 8192,
            pending_records: 8,
            diagnostic_bytes: 128,
        },
        timeouts: g::CaptureTimeouts {
            startup: Duration::from_secs(2),
            blocked_publication: Duration::from_secs(2),
            final_drain: Duration::from_secs(2),
        },
    };
    let before = identity();
    assert_eq!(before.2, vec![before.0]);
    let (mut owner, mut sink, host) = unsafe { g::prepare_capture_unstarted(options) }.unwrap();
    let after = identity();
    assert_eq!(before, after);
    println!(
        "zero-task preparation pid={} start={} before={:?} after={:?}",
        before.0, before.1, before.2, after.2
    );
    let prefix = [
        format!("seed={}\n", 100),
        format!("original host value={}\n", 7),
    ];
    assert_eq!(host.write_record(prefix[0].as_bytes()).unwrap().order, 1);
    assert_eq!(host.write_record(prefix[1].as_bytes()).unwrap().order, 2);
    assert_eq!(
        owner
            .handle()
            .capture_snapshot()
            .unwrap()
            .publication
            .progress
            .acknowledged_data_bytes,
        0
    );
    let socket = sink.take_prepared_endpoint().unwrap().unwrap();
    let fd = socket.as_raw_fd();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--deferred-guest", &fd.to_string()]);
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let child_pid = child.id();
    drop(socket);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    owner
        .start_workers(Destination {
            bytes: bytes.clone(),
            count: 0,
        })
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    owner.handle().root_reaped();
    owner.handle().run_state(g::RunState::Succeeded);
    let report = owner.finish_until(Instant::now() + Duration::from_secs(3));
    assert!(report.qualifies(), "{report:?}");
    let expected = format!("{}{}guest original\n", prefix[0], prefix[1]);
    assert_eq!(*bytes.lock().unwrap(), expected.as_bytes());
    assert_eq!(
        report.publication.progress.acknowledged_data_bytes,
        expected.len() as u64
    );
    println!("deferred capture qualified after actual reap pid={child_pid}: {report:?}");
    true
}
