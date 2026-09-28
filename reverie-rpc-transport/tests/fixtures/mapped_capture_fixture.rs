//! Fresh native process controls for direct mapped capture and physical waits.

use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use reverie_rpc_transport::guest_log as g;

#[derive(Clone, Default)]
struct Destination(Arc<Mutex<Vec<u8>>>);

impl Write for Destination {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl g::CaptureDestination for Destination {
    fn progress(&self) -> g::DestinationProgress {
        g::DestinationProgress {
            acknowledged_data_bytes: self.0.lock().unwrap().len() as u64,
            ..Default::default()
        }
    }
}

fn options() -> g::CaptureOptions {
    g::CaptureOptions {
        limits: g::CaptureLimits {
            producers: 4,
            slots_per_producer: 8,
            max_record_bytes: 8192,
            host_pending_bytes: 16384,
            guest_pending_bytes: 16384,
            pending_records: 16,
            diagnostic_bytes: 32768,
        },
        timeouts: g::CaptureTimeouts {
            startup: Duration::from_secs(2),
            blocked_publication: Duration::from_secs(2),
            final_drain: Duration::from_millis(200),
        },
    }
}

fn guest_record(role: usize) -> Vec<u8> {
    let mut bytes = vec![b'B' + role as u8; 5001];
    bytes[17] = 0;
    bytes[4999] = 0xff;
    bytes
}

fn guest(mode: &str, fds: [i32; 2]) {
    let buffers = fds.map(|fd| {
        let buffer = unsafe { g::ordered::Buffer::import(OwnedFd::from_raw_fd(fd)) }.unwrap();
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        buffer
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    for (role, buffer) in buffers.into_iter().enumerate() {
        let mut writer = unsafe { buffer.activate(1, i64::from(std::process::id())) }.unwrap();
        let wait = |_: &g::SharedBuffer, _: u32| {
            assert!(Instant::now() < deadline, "native mapped writer stalled");
            std::thread::yield_now();
            Ok(())
        };
        writer.write_record(&guest_record(role), wait).unwrap();
        if mode != "missing-private-finish" || role == 0 {
            writer.finish(wait).unwrap();
        }
    }
}

fn host(mode: &str) {
    assert_eq!(unsafe { libc::getpid() }, unsafe { libc::gettid() });
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
    let (mut processes, observer) = unsafe { g::MappedProcessOwner::new() };
    let public_bytes = Destination::default();
    let private_bytes = Destination::default();
    let (mut public, public_fd, public_writer) =
        unsafe { g::prepared_mapped_capture(options(), public_bytes.clone(), observer.clone()) }
            .unwrap();
    let (mut private, private_fd, private_writer) =
        unsafe { g::prepared_mapped_capture(options(), private_bytes.clone(), observer.clone()) }
            .unwrap();
    public_writer.write_record(b"public host A\n").unwrap();
    private_writer.write_record(b"private host A\n").unwrap();
    assert!(!public.snapshot().qualifies());
    assert!(!private.snapshot().qualifies());

    // The main host is the sole waiter. No P_ALL wait runs inside spawn's
    // in-progress interval or its pre_exec/exec error handling.
    let fds = [public_fd.as_raw_fd(), private_fd.as_raw_fd()];
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--guest", mode, &fds[0].to_string(), &fds[1].to_string()]);
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let root = child.id() as i32;
    drop(public_fd);
    drop(private_fd);
    assert!(
        !observer.snapshot().all_reaped,
        "setup closure became lifetime completion"
    );
    processes.close_admission();
    let status = child.wait().unwrap();
    assert!(status.success(), "native writer failed: {status}");
    unsafe { processes.root_reaped(root, status.into_raw()) }.unwrap();
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::__WALL,
            )
        },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    if mode == "abandoned-owner" {
        drop(processes);
    } else {
        unsafe { processes.all_reaped() }.unwrap();
        drop(processes);
    }
    public_writer.write_record(b"public cleanup C\n").unwrap();
    private_writer.write_record(b"private cleanup C\n").unwrap();
    public.handle().run_state(g::RunState::Succeeded);
    private.handle().run_state(g::RunState::Succeeded);
    let deadline = Instant::now() + Duration::from_secs(2);
    public.request_close_until(deadline);
    private.request_close_until(deadline);
    let public = public.finish_until(deadline);
    let private = private.finish_until(deadline);
    assert!(!public.capture.guest.peer_closed);
    assert!(!private.capture.guest.peer_closed);
    assert!(
        !public.capture.qualifies(),
        "mapped lifetime qualified a socket report"
    );
    assert!(
        !private.capture.qualifies(),
        "mapped lifetime qualified a socket report"
    );
    assert_eq!(public.qualifies(), mode != "abandoned-owner", "{public:?}");
    assert_eq!(private.qualifies(), mode == "complete", "{private:?}");
    for (role, (destination, first, last)) in [
        (
            public_bytes,
            b"public host A\n".as_slice(),
            b"public cleanup C\n".as_slice(),
        ),
        (
            private_bytes,
            b"private host A\n".as_slice(),
            b"private cleanup C\n".as_slice(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let expected = [first, &guest_record(role), last].concat();
        assert_eq!(*destination.0.lock().unwrap(), expected);
    }
    assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--guest") {
        guest(
            &args[1],
            [args[2].parse().unwrap(), args[3].parse().unwrap()],
        );
    } else {
        let mode = &args[0];
        assert!(matches!(
            mode.as_str(),
            "complete" | "missing-private-finish" | "abandoned-owner"
        ));
        host(mode);
        println!("mapped capture complete: {mode}");
    }
}
