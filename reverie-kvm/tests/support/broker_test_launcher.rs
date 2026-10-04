// Ordinary-main launcher for the seven explicitly broker-routed libtest routes.
// This private proposal depends on the API contracts listed in API-CONTRACT.md.
// No constructor, libtest worker or lazy backend path may assert startup authority.
use std::ffi::CString;
use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;

use reverie_kvm::native_exit_broker::BrokerOwner;
use reverie_kvm::native_exit_broker::StartupAuthority;

#[path = "broker_test_protocol.rs"]
mod broker_test_protocol;

#[path = "broker_native_cases.rs"]
mod broker_native_cases;

fn main() {
    // SAFETY: this standalone executable's first main-body action precedes all
    // threads, logging, test/native children and guest resources. Its controlled
    // startup must satisfy StartupAuthority's full inherited-fd/reaper contract;
    // it is not a general embedding or injected-constructor entry point.
    let authority = unsafe { StartupAuthority::assert_exclusive_early_launch() };
    let mut owner = match BrokerOwner::bootstrap(authority) {
        Ok(owner) => owner,
        Err(failure) => {
            eprintln!("BROKER_LAUNCH_SETUP_FAILURE: {}", failure.cause);
            if failure.child.is_some() {
                retain_for_outer_guard(failure);
            }
            std::process::exit(125);
        }
    };
    if let Some(case) = std::env::args().nth(1).filter(|arg| arg == "--native-case") {
        let _ = case;
        let args: Vec<_> = std::env::args().skip(2).collect();
        if args.len() != 1 {
            eprintln!("exactly one native case required");
            std::process::exit(125);
        }
        let result = match args[0].as_str() {
            "single" | "shared-arc" | "nested-scm" => {
                broker_native_cases::run_queued_case(&args[0], &owner.client())
            }
            "chunk-resource-abort" => broker_native_cases::run_chunk_abort_case(&owner),
            _ => {
                eprintln!("unknown native case");
                std::process::exit(125);
            }
        };
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(failure) => {
                eprintln!("NATIVE_BROKER_CASE_FAILURE: {}", failure.message);
                // Until explicit failure drain/reap is implemented, this is an
                // infrastructure failure. It cannot qualify a causal mutant.
                retain_for_outer_guard((owner, failure));
            }
        };
        if let Err(error) = settle_broker(&mut owner) {
            eprintln!("NATIVE_BROKER_CASE_SETTLEMENT_FAILURE: {error}");
            retain_for_outer_guard(owner);
        }
        println!("NATIVE_BROKER_CASE_PASS: {receipt:?}");
        return;
    }
    let outcome = launch_selected(&owner);
    let shutdown = settle_broker(&mut owner);
    if let Err(error) = shutdown {
        eprintln!("BROKER_LAUNCH_INFRASTRUCTURE_FAILURE: {error}");
        // No blocking destructor and no dropped/unconfirmed native wait owner.
        // The existing enclosing timeout owns the entire non-escaped group.
        retain_for_outer_guard(owner);
    }
    let status = match outcome {
        Ok(status) => status,
        Err(error) => {
            eprintln!("BROKER_LAUNCH_SETUP_FAILURE: {error}");
            std::process::exit(125);
        }
    };
    drop(owner); // Already actually reaped. No guest socket owners live here.
    mirror_status(status);
}

fn launch_selected(owner: &BrokerOwner) -> Result<i32, String> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() != 6 || args[1] != "--exact" || args[3] != "--nocapture" || args[4] != "--logfile"
    {
        return Err("expected exact existing libtest argument shape".into());
    }
    let test = args[2].to_str().ok_or("test name must be UTF-8")?;
    if !broker_test_protocol::selected(test) {
        return Err("test is not an explicitly broker-enrolled selector".into());
    }
    if !std::path::Path::new(&args[0]).is_absolute()
        || !std::path::Path::new(&args[5]).is_absolute()
    {
        return Err("libtest executable/result path must be absolute".into());
    }
    let exported = match owner.export_client_for_exec() {
        Ok(exported) => exported,
        Err(failure) => {
            eprintln!("BROKER_LAUNCH_SETUP_FAILURE: {}", failure.cause);
            // Keep channel and any unexpected SCM_RIGHTS owner, including a
            // received prefix. Formatting an error must not drop those owners.
            retain_for_outer_guard(failure);
        }
    };
    let (channel, nonce) = exported.into_parts();
    let fd = channel.as_raw_fd();
    if fd < 3 {
        return Err("exported client must not alias stdio".into());
    }
    let encoded: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut environment: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            key != broker_test_protocol::CHANNEL_ENV
                && key != broker_test_protocol::NONCE_ENV
                && key != broker_test_protocol::CHILD_ENV
        })
        .collect();
    environment.push((
        broker_test_protocol::CHANNEL_ENV.into(),
        fd.to_string().into(),
    ));
    environment.push((broker_test_protocol::NONCE_ENV.into(), encoded.into()));
    environment.push((broker_test_protocol::CHILD_ENV.into(), test.into()));
    let argv: Vec<CString> = args
        .iter()
        .map(|arg| CString::new(arg.as_bytes()).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let env: Vec<CString> = environment
        .into_iter()
        .map(|(key, value)| {
            let mut bytes = key.as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            CString::new(bytes).map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let mut env_ptrs: Vec<*const libc::c_char> = env.iter().map(|s| s.as_ptr()).collect();
    env_ptrs.push(std::ptr::null());

    let all = u64::MAX;
    let mut old = 0u64;
    let blocked = unsafe {
        raw(
            libc::SYS_rt_sigprocmask,
            libc::SIG_SETMASK as usize,
            (&all as *const u64) as usize,
            (&mut old as *mut u64) as usize,
            8,
            0,
            0,
        )
    };
    if blocked < 0 {
        return Err(format!("block clone mask: {}", -blocked));
    }
    // flags0 means a separate native process with exit_signal0. It is not a
    // thread, vfork, shared fd table or CLONE_PARENT child. Parent never execs.
    let pid = unsafe { raw(libc::SYS_clone, 0, 0, 0, 0, 0, 0) };
    if pid == 0 {
        // No allocator, Rust Drop, libc TLS errno, panic or callback in child.
        let inherited = unsafe {
            raw(
                libc::SYS_fcntl,
                fd as usize,
                libc::F_SETFD as usize,
                0,
                0,
                0,
                0,
            )
        };
        if inherited < 0 {
            unsafe { exit_raw(125) }
        }
        let restored = unsafe {
            raw(
                libc::SYS_rt_sigprocmask,
                libc::SIG_SETMASK as usize,
                (&old as *const u64) as usize,
                0,
                8,
                0,
                0,
            )
        };
        if restored < 0 {
            unsafe { exit_raw(125) }
        }
        unsafe {
            raw(
                libc::SYS_execve,
                argv[0].as_ptr() as usize,
                argv_ptrs.as_ptr() as usize,
                env_ptrs.as_ptr() as usize,
                0,
                0,
                0,
            )
        };
        unsafe { exit_raw(125) }
    }
    let restored = unsafe {
        raw(
            libc::SYS_rt_sigprocmask,
            libc::SIG_SETMASK as usize,
            (&old as *const u64) as usize,
            0,
            8,
            0,
            0,
        )
    };
    if restored < 0 {
        eprintln!(
            "BROKER_LAUNCH_INFRASTRUCTURE_FAILURE: parent mask restore {}",
            -restored
        );
        // Even a failed clone does not permit continuing under the wrong mask.
        retain_for_outer_guard((channel, pid));
    }
    if pid < 0 {
        return Err(format!("clone exact libtest child: {}", -pid));
    }
    drop(channel); // Child has its inherited dedicated endpoint; owner stays here.
    let mut status = 0i32;
    loop {
        let waited = unsafe {
            raw(
                libc::SYS_wait4,
                pid as usize,
                (&mut status as *mut i32) as usize,
                libc::__WCLONE as usize,
                0,
                0,
                0,
            )
        };
        if waited == pid {
            return Ok(status);
        }
        if waited == -(libc::EINTR as i64) {
            continue;
        }
        eprintln!("BROKER_LAUNCH_INFRASTRUCTURE_FAILURE: exact child wait {waited}");
        retain_for_outer_guard((pid, status));
    }
}

fn settle_broker(owner: &mut BrokerOwner) -> Result<(), String> {
    loop {
        if owner.request_shutdown().map_err(|e| e.to_string())? {
            break;
        }
        wait_interest(owner.shutdown_poll_interest())?;
    }
    loop {
        if let Some(status) = owner.try_wait().map_err(|e| e.to_string())? {
            return if status == 0 {
                Ok(())
            } else {
                Err(format!("broker native status {status}"))
            };
        }
        let interest = owner
            .exit_poll_interest()
            .ok_or("unreaped owner has no pidfd")?;
        wait_interest(interest)?;
    }
}

fn wait_interest(mut fd: libc::pollfd) -> Result<(), String> {
    loop {
        let result = unsafe { libc::poll(&mut fd, 1, -1) };
        if result > 0 && fd.revents & libc::POLLNVAL == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if result < 0 && error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(format!(
            "broker progress poll: {result}, revents={}, {error}",
            fd.revents
        ));
    }
}

// This is a terminal infrastructure failure, never an accepted guest/test result.
// Retain owned state without unwinding until the unchanged outer30+kill2 guard
// retires this launcher and its non-escaped children. No new private timeout.
fn retain_for_outer_guard<T>(state: T) -> ! {
    loop {
        std::hint::black_box(&state);
        unsafe {
            libc::pause();
        }
    }
}

fn mirror_status(status: i32) -> ! {
    if libc::WIFEXITED(status) {
        std::process::exit(libc::WEXITSTATUS(status));
    }
    if libc::WIFSIGNALED(status) {
        let signal = libc::WTERMSIG(status);
        // Only this isolated launcher changes its own final disposition/mask;
        // the caller's ambient SIGCHLD state was never changed.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            raw(
                libc::SYS_tgkill,
                libc::getpid() as usize,
                raw(libc::SYS_gettid, 0, 0, 0, 0, 0, 0) as usize,
                signal as usize,
                0,
                0,
                0,
            );
        }
        std::process::exit(128 + signal);
    }
    eprintln!("BROKER_LAUNCH_INFRASTRUCTURE_FAILURE: nonterminal wait status {status}");
    std::process::exit(125);
}

unsafe fn exit_raw(code: usize) -> ! {
    unsafe {
        raw(libc::SYS_exit_group, code, 0, 0, 0, 0, 0);
    }
    loop {
        std::hint::spin_loop();
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn raw(
    number: libc::c_long,
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    e: usize,
    f: usize,
) -> i64 {
    let result: i64;
    unsafe {
        std::arch::asm!("syscall", inlateout("rax") number => result,
            in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d,
            in("r8") e, in("r9") f, lateout("rcx") _, lateout("r11") _,
            options(nostack));
    }
    result
}
