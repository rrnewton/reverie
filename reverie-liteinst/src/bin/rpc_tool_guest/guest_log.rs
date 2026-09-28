//! Real typed Tool fixtures for the local diagnostic packet transport.
//! The executable constructor consumes the sealed bootstrap before threads;
//! Tool installation stays after native pkey controls in the existing fixture.

use std::ffi::CStr;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::GuestLogWriter;
use reverie_liteinst::LiteinstBackend;
use reverie_liteinst::PreloadBootstrap;

const CHILD_ENV: &str = "LITEINST_DIAGNOSTIC_LOG_CHILD";
const TOOL_DATA: &[u8] = b"diagnostic fixture\0\xff";
const BOOTSTRAP_RECORD: &[u8] = b"bootstrap ready\n";
const TOOL_RECORD: &[u8] = b"Tool syscall\n";
const CLEANUP_RECORD: &[u8] = b"Tool cleanup\n";
const OUT: &[u8] = b"injected stdout\n";
const ERR: &[u8] = b"tail-injected stderr\n";
const PIPE_BYTES: usize = 1024 * 1024;
const PIPE_EVIDENCE_ENV: &str = "LITEINST_LOG_PIPE_EVIDENCE";
static WRITER: OnceLock<Mutex<GuestLogWriter>> = OnceLock::new();
static BOOTSTRAP: OnceLock<PreloadBootstrap> = OnceLock::new();
static LOG_FD: AtomicI32 = AtomicI32::new(-1);

#[used]
#[unsafe(link_section = ".init_array")]
static INITIALIZE: unsafe extern "C" fn() = initialize;

unsafe extern "C" fn initialize() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let bootstrap = unsafe { reverie_liteinst::take_preload_bootstrap_with_log() }
        .unwrap()
        .unwrap();
    assert_eq!(bootstrap.bootstrap.tool_data, TOOL_DATA);
    let log = bootstrap.log.expect("V2 diagnostic endpoint");
    // Discover the private integer using ordinary descriptor metadata, as an
    // application can. Access protection must hold even when it knows the fd.
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let entry = entry.unwrap();
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let mut kind = 0i32;
        let mut size = std::mem::size_of_val(&kind) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &mut size,
            )
        } == 0
            && kind == libc::SOCK_SEQPACKET
        {
            descriptors.push(fd);
        }
    }
    assert_eq!(descriptors.len(), 1);
    LOG_FD.store(descriptors[0], Ordering::Relaxed);
    assert!(BOOTSTRAP.set(bootstrap.bootstrap).is_ok());
    assert!(
        WRITER
            .set(Mutex::new(unsafe { log.install() }.unwrap()))
            .is_ok()
    );
    emit(BOOTSTRAP_RECORD);
}

pub(super) fn emit(bytes: &[u8]) {
    if let Some(writer) = WRITER.get() {
        writer.lock().unwrap().write_all(bytes).unwrap();
    }
}

pub(super) fn syscall_record() {
    emit(TOOL_RECORD);
}
pub(super) fn cleanup_record() {
    emit(CLEANUP_RECORD);
}

#[derive(Default)]
struct ProtectedTool;

fn write_syscall(fd: i32, bytes: &[u8]) -> Syscall {
    Syscall::from_raw(
        Sysno::write,
        SyscallArgs::new(fd as usize, bytes.as_ptr() as usize, bytes.len(), 0, 0, 0),
    )
}

#[reverie::tool]
impl Tool for ProtectedTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid, Sysno::getuid, Sysno::getppid, Sysno::getgid]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        syscall_record();
        let _ = guest.send_rpc(1).await;
        match syscall.number() {
            Sysno::getpid => Ok(guest
                .inject(write_syscall(LOG_FD.load(Ordering::Relaxed), b"forbidden"))
                .await?),
            Sysno::getuid => {
                guest
                    .tail_inject(write_syscall(LOG_FD.load(Ordering::Relaxed), b"forbidden"))
                    .await
            }
            Sysno::getppid => Ok(guest
                .inject(write_syscall(libc::STDOUT_FILENO, OUT))
                .await?),
            Sysno::getgid => {
                guest
                    .tail_inject(write_syscall(libc::STDERR_FILENO, ERR))
                    .await
            }
            _ => unreachable!(),
        }
    }

    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        _: Pid,
        _: &G,
        _: ExitStatus,
    ) -> Result<(), Error> {
        cleanup_record();
        Ok(())
    }
}

pub(super) fn child(case: &str) {
    let path = &BOOTSTRAP.get().expect("constructor bootstrap").coordinator;
    match case {
        "pkey" => super::syscall_fallback_guest::run_pkey(path),
        "fork" => super::syscall_fallback_guest::run_fork(path, false),
        "one-blocking-worker" => {
            unsafe { reverie_liteinst::install_tool::<ProtectedTool>(path) }.unwrap();
            let capacities = [libc::STDOUT_FILENO, libc::STDERR_FILENO].map(|fd| {
                let capacity = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
                assert!(capacity > 0 && (capacity as usize) < PIPE_BYTES);
                capacity
            });
            std::fs::write(
                std::env::var_os(PIPE_EVIDENCE_ENV).expect("pipe evidence path"),
                format!(
                    "stdout={} stderr={} payload={PIPE_BYTES}\n",
                    capacities[0], capacities[1]
                ),
            )
            .unwrap();
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_getppid) },
                OUT.len() as i64
            );
            assert_eq!(unsafe { libc::syscall(libc::SYS_getgid) }, ERR.len() as i64);
            std::io::stdout()
                .write_all(&vec![b'O'; PIPE_BYTES])
                .unwrap();
            std::io::stderr()
                .write_all(&vec![b'E'; PIPE_BYTES])
                .unwrap();
        }
        "descriptors" | "missing-finish" => {
            unsafe { reverie_liteinst::install_tool::<ProtectedTool>(path) }.unwrap();
            let fd = LOG_FD.load(Ordering::Relaxed);
            for number in [libc::SYS_write, libc::SYS_getpid, libc::SYS_getuid] {
                assert_eq!(
                    unsafe { libc::syscall(number, fd, b"forbidden".as_ptr(), 9) },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EBADF)
                );
            }
            assert_eq!(unsafe { libc::syscall(libc::SYS_close, fd) }, 0);
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_getppid) },
                OUT.len() as i64
            );
            assert_eq!(unsafe { libc::syscall(libc::SYS_getgid) }, ERR.len() as i64);
            if case == "missing-finish" {
                // Deliberately bypass the Tool exit callback. An actual closed
                // endpoint and successful child exit must not fabricate FINISH.
                unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_exit_group, [0; 6]) };
                unreachable!();
            }
        }
        _ => panic!("unknown diagnostic case"),
    }
}

fn loader_library() -> PathBuf {
    // The Tool and bootstrap constructor are compiled into this executable so
    // native controls run before Tool installation. An already loaded libc is
    // the inert preload argument for this logged-launch API fixture.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe { libc::dladdr(libc::getpid as *const () as *const _, &mut info) },
        0
    );
    PathBuf::from(unsafe { CStr::from_ptr(info.dli_fname) }.to_str().unwrap())
}

pub(super) fn host(directory: &Path, case: &str) {
    if case == "pkey" && core::arch::x86_64::__cpuid_count(7, 0).ecx & (1 << 4) == 0 {
        println!("diagnostic pkey: OSPKE unavailable");
        std::process::exit(77);
    }
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.enable_all();
    if case == "one-blocking-worker" {
        builder.max_blocking_threads(1);
    }
    let runtime = builder.build().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    let mut command = reverie::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("diagnostic-log-child")
        .arg(case)
        .env(CHILD_ENV, "1");
    command
        .env_remove("REVERIE_LITEINST_TOOL")
        .env_remove("REVERIE_LITEINST_HOST_RUNTIME");
    if case == "one-blocking-worker" {
        command.env(PIPE_EVIDENCE_ENV, directory.join("pipe-capacities.txt"));
    }
    let (output, global, log) = runtime
        .block_on(LiteinstBackend::run_with_output_and_preload_data_and_log::<
            super::CounterTool,
        >(
            command, (), loader_library(), TOOL_DATA, 1024 * 1024
        ))
        .unwrap();
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("stdout.bin"), &output.stdout).unwrap();
    std::fs::write(directory.join("stderr.bin"), &output.stderr).unwrap();
    std::fs::write(directory.join("log.bin"), &log.bytes).unwrap();
    std::fs::write(
        directory.join("error.txt"),
        log.error.as_deref().unwrap_or(""),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let (calls, cleanups) = match case {
        "pkey" => (2, 1),
        "fork" => (1, 2),
        "descriptors" => (4, 1),
        "missing-finish" => (4, 0),
        "one-blocking-worker" => (2, 1),
        _ => unreachable!(),
    };
    assert_eq!(global.calls.load(Ordering::Relaxed), calls);
    assert_eq!(global.senders.lock().unwrap().len(), 1);
    let expected = [
        BOOTSTRAP_RECORD.to_vec(),
        TOOL_RECORD.repeat(calls as usize),
        CLEANUP_RECORD.repeat(cleanups),
    ]
    .concat();
    assert_eq!(log.bytes, expected);
    if case == "missing-finish" {
        assert!(
            log.error
                .as_deref()
                .unwrap()
                .contains("missing process completion")
        );
    } else {
        assert!(log.error.is_none(), "{:?}", log.error);
    }
    println!(
        "diagnostic {case}: rpc={calls} stdout={} stderr={} log={} missing-finish={}",
        output.stdout.len(),
        output.stderr.len(),
        log.bytes.len(),
        case == "missing-finish"
    );
}
