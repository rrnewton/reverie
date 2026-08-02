use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::COORDINATOR_ENV;
use reverie_liteinst::LiteinstBackend;

#[derive(Default)]
struct ClockGlobal {
    calls: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for ClockGlobal {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, (): ()) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct ClockTool;

#[reverie::tool]
impl Tool for ClockTool {
    type GlobalState = ClockGlobal;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::clock_gettime].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::clock_gettime);
        guest.send_rpc(()).await;
        Ok(guest.inject(syscall).await?)
    }
}

fn preload_path() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let parent = executable
        .parent()
        .ok_or_else(|| io::Error::other("lifecycle guest has no parent directory"))?;
    [
        parent.join("libreverie_liteinst.so"),
        parent.join("deps/libreverie_liteinst.so"),
        parent
            .parent()
            .unwrap_or(parent)
            .join("libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing LiteInst preload library"))
}

fn install<T: Tool + 'static>() -> io::Result<()> {
    let socket = std::env::var_os(COORDINATOR_ENV)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, COORDINATOR_ENV))?;
    // SAFETY: each guest mode installs exactly once before creating a thread.
    unsafe {
        std::env::remove_var(COORDINATOR_ENV);
        reverie_liteinst::install_tool::<T>(socket)
    }
}

fn write_marker_after_root_exit(path: &Path) -> ! {
    let path = CString::new(path.as_os_str().as_bytes()).expect("marker path contains NUL");
    let child = unsafe { libc::fork() };
    if child == -1 {
        unsafe { libc::_exit(90) }
    }
    if child == 0 {
        unsafe { libc::usleep(200_000) };
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd == -1 {
            unsafe { libc::_exit(91) }
        }
        let marker = b"descendant-finished\n";
        let written = unsafe { libc::write(fd, marker.as_ptr().cast(), marker.len()) };
        let _ = unsafe { libc::close(fd) };
        unsafe {
            libc::_exit(if written == marker.len() as isize {
                0
            } else {
                92
            })
        }
    }
    unsafe { libc::_exit(23) }
}

fn guest_root_exit(marker: &Path) -> io::Result<()> {
    install::<()>()?;
    write_marker_after_root_exit(marker)
}

fn guest_clock() -> io::Result<()> {
    install::<ClockTool>()?;
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut timestamp) } == 0 {
        return Err(io::Error::other(format!(
            "vDSO clock leaked host time: {}.{}",
            timestamp.tv_sec, timestamp.tv_nsec
        )));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
        println!("vdso-clock-failed-closed");
        Ok(())
    } else {
        Err(error)
    }
}

fn guest_signal() -> io::Result<()> {
    install::<()>()?;
    unsafe {
        core::arch::asm!("ud2", options(noreturn));
    }
}

async fn root_exit_harness() -> Result<(), Error> {
    let directory = tempfile::tempdir()?;
    let marker = directory.path().join("descendant.marker");
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("guest-root-exit").arg(&marker);
    let (status, ()) =
        LiteinstBackend::run_with_preload::<()>(command, (), preload_path()?).await?;
    if status != ExitStatus::Exited(23) {
        return Err(io::Error::other(format!("unexpected root status: {status:?}")).into());
    }
    let contents = std::fs::read(&marker)?;
    if contents != b"descendant-finished\n" {
        return Err(io::Error::other(format!("unexpected descendant marker: {contents:?}")).into());
    }
    Ok(())
}

async fn clock_harness() -> Result<(), Error> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("guest-clock");
    let (output, global) =
        LiteinstBackend::run_with_output_and_preload::<ClockTool>(command, (), preload_path()?)
            .await?;
    if !output.status.success() {
        return Err(io::Error::other(format!("clock guest failed: {output:?}")).into());
    }
    if output.stdout != b"vdso-clock-failed-closed\n" {
        return Err(io::Error::other(format!("unexpected clock output: {output:?}")).into());
    }
    if global.calls.load(Ordering::Relaxed) != 0 {
        return Err(io::Error::other("unpatchable vDSO call unexpectedly reached the Tool").into());
    }
    Ok(())
}

async fn signal_harness() -> Result<(), Error> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("guest-signal");
    let (status, ()) =
        LiteinstBackend::run_with_preload::<()>(command, (), preload_path()?).await?;
    if status.signal() != Some(libc::SIGILL) {
        return Err(io::Error::other(format!("unexpected signal status: {status:?}")).into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let mode = args
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing lifecycle mode"))?;
    match mode.to_str() {
        Some("guest-root-exit") => {
            let marker = args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "missing descendant marker path",
                )
            })?;
            guest_root_exit(Path::new(&marker))?;
        }
        Some("guest-clock") => guest_clock()?,
        Some("guest-signal") => guest_signal()?,
        Some("root-exit-harness" | "clock-harness" | "signal-harness") => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                match mode.to_str() {
                    Some("root-exit-harness") => root_exit_harness().await,
                    Some("clock-harness") => clock_harness().await,
                    Some("signal-harness") => signal_harness().await,
                    _ => unreachable!(),
                }
            })?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown lifecycle mode {mode:?}"),
            )
            .into());
        }
    }
    Ok(())
}
