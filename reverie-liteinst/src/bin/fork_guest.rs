use std::process;

const CHILD_MESSAGE: &[u8] = b"fork child reached guest code\n";

fn main() {
    let mode = std::env::args_os().nth(1);
    if mode.as_deref() == Some(std::ffi::OsStr::new("--unsafe-clone")) {
        probe_unsafe_clone();
        return;
    }
    if mode.as_deref() == Some(std::ffi::OsStr::new("--unsafe-process")) {
        probe_unsafe_process_creation();
        return;
    }
    let raw_fork = mode.as_deref() == Some(std::ffi::OsStr::new("--raw-fork"));
    let child = unsafe {
        if raw_fork {
            libc::syscall(libc::SYS_fork) as libc::pid_t
        } else {
            libc::fork()
        }
    };
    if child < 0 {
        eprintln!("fork failed: {}", std::io::Error::last_os_error());
        process::exit(1);
    }

    if child == 0 {
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                CHILD_MESSAGE.as_ptr().cast(),
                CHILD_MESSAGE.len(),
            );
            libc::_exit(0);
        }
    }

    let mut status = 0;
    if unsafe { libc::waitpid(child, &mut status, 0) } != child {
        eprintln!("waitpid failed: {}", std::io::Error::last_os_error());
        process::exit(1);
    }
    if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        eprintln!("child status was {status}");
        process::exit(1);
    }

    println!("fork parent observed child {child}");
}

fn probe_unsafe_clone() {
    let flags = libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD;
    let result = unsafe { libc::syscall(libc::SYS_clone, flags, 0, 0, 0, 0) };
    if result == 0 {
        unsafe {
            libc::_exit(90);
        }
    }
    if result >= 0 {
        eprintln!("unsafe clone unexpectedly created child {result}");
        process::exit(1);
    }
    println!(
        "unsafe clone rejected: {}",
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    );
}

/// Issue raw `vfork` and eight `clone3` shapes through libc's `syscall()`
/// site. Each must be refused with `ENOTSUP` before the kernel sees it: no task
/// may be created, the kernel's own `EINVAL`/`EPERM` for malformed
/// `clone_args` must not surface, and the parent's stack and TLS canaries must
/// be unchanged afterwards.
fn probe_unsafe_process_creation() {
    std::thread_local! {
        static TLS_CANARY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    const CANARY: u64 = 0x4c49_5445_464f_524b;
    // `struct clone_args` through `cgroup`: eleven u64 fields.
    const CLONE_ARGS_SIZE_VER2: usize = 88;
    let mut stack_canary = CANARY;
    let stack_canary_address = &raw mut stack_canary;
    TLS_CANARY.set(CANARY);
    // The first call traps and installs the hook on libc's `syscall()` site, so
    // every probe below reaches the dispatcher through that installed hook.
    let pid = unsafe { libc::syscall(libc::SYS_getpid) };
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    assert!(
        pid > 0 && tid > 0,
        "parent identity must be available before the refusal probes"
    );
    let child_stack = [0_u64; 1024];
    let child_stack_base = child_stack.as_ptr() as u64;
    let child_stack_size = std::mem::size_of_val(&child_stack) as u64;
    let mut results = Vec::new();
    for (name, number, flags, stack, tls, size) in [
        ("vfork", libc::SYS_vfork, 0, 0, 0, CLONE_ARGS_SIZE_VER2),
        ("clone3", libc::SYS_clone3, 0, 0, 0, CLONE_ARGS_SIZE_VER2),
        (
            "clone3-shared",
            libc::SYS_clone3,
            (libc::CLONE_VM | libc::CLONE_VFORK) as u64,
            0,
            0,
            CLONE_ARGS_SIZE_VER2,
        ),
        (
            "clone3-thread",
            libc::SYS_clone3,
            (libc::CLONE_VM | libc::CLONE_SIGHAND | libc::CLONE_THREAD) as u64,
            0,
            0,
            CLONE_ARGS_SIZE_VER2,
        ),
        (
            "clone3-stack",
            libc::SYS_clone3,
            0,
            child_stack_base,
            0,
            CLONE_ARGS_SIZE_VER2,
        ),
        (
            "clone3-tls",
            libc::SYS_clone3,
            libc::CLONE_SETTLS as u64,
            0,
            u64::MAX,
            CLONE_ARGS_SIZE_VER2,
        ),
        (
            "clone3-flags",
            libc::SYS_clone3,
            1_u64 << 63,
            0,
            0,
            CLONE_ARGS_SIZE_VER2,
        ),
        ("clone3-size", libc::SYS_clone3, 0, 0, 0, 1),
    ] {
        let mut clone_args = [0_u64; CLONE_ARGS_SIZE_VER2 / 8];
        clone_args[0] = flags;
        clone_args[4] = libc::SIGCHLD as u64;
        clone_args[5] = stack;
        clone_args[6] = if stack == 0 { 0 } else { child_stack_size };
        clone_args[7] = tls;
        let result = unsafe { libc::syscall(number, clone_args.as_ptr(), size) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if result == 0 {
            // A faulty admission created a child. A child sharing the
            // parent's memory makes these writes visible to the parent.
            TLS_CANARY.set(0);
            unsafe {
                core::ptr::write_volatile(stack_canary_address, 0);
                libc::_exit(90);
            }
        }
        if result > 0 {
            // The refusal failed and created a task. Kill it before reaping it
            // rather than wait for it to exit: a child started on its own stack
            // inside the forwarding code can spin forever, which would hang
            // this probe instead of failing it. SIGKILL cannot be blocked, so
            // the wait below is bounded.
            let child = result as libc::pid_t;
            assert_eq!(
                unsafe { libc::kill(child, libc::SIGKILL) },
                0,
                "failed to kill unexpected {name} child {child}"
            );
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(child, &mut status, 0) };
                if waited == child {
                    break;
                }
                assert!(
                    waited == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR),
                    "failed to reap unexpected {name} child {child}"
                );
            }
        }
        assert_eq!(result, -1, "{name} created task {result}");
        assert_eq!(
            errno,
            libc::ENOTSUP,
            "{name} was not refused before forwarding"
        );
        assert_eq!(
            unsafe { core::ptr::read_volatile(stack_canary_address) },
            CANARY,
            "{name} changed the parent's stack"
        );
        assert_eq!(TLS_CANARY.get(), CANARY, "{name} changed the parent's TLS");
        assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, pid);
        assert_eq!(unsafe { libc::syscall(libc::SYS_gettid) }, tid);
        results.push(format!("{name}={errno}"));
    }
    // Later hooked syscalls must still return into this parent's frames.
    for _ in 0..4 {
        assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, pid);
    }
    println!(
        "unsafe process creation rejected: {} parent-canaries=unchanged",
        results.join(" ")
    );
}
