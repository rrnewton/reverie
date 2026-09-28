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
    // Warm exactly libc's variable syscall site before raw fork. The fork then
    // returns through an already installed callback in both COW processes.
    let parent_before = if raw_fork {
        let pid = unsafe { libc::syscall(libc::SYS_getpid) };
        assert!(pid > 0);
        Some(pid)
    } else {
        None
    };
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

    if let Some(parent_before) = parent_before {
        for _ in 0..4 {
            assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, parent_before);
        }
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

fn probe_unsafe_process_creation() {
    std::thread_local! {
        static PARENT_MARKER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    const MARKER: u64 = 0x4c49_5445_464f_524b;
    let mut stack_marker = MARKER;
    let stack_address = &raw mut stack_marker;
    PARENT_MARKER.set(MARKER);
    let pid = unsafe { libc::syscall(libc::SYS_getpid) };
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    assert!(
        pid > 0 && tid > 0,
        "parent identity must be available before refusal probes"
    );
    let mut results = Vec::new();
    // The current Linux clone_args layout has eleven u64 fields through cgroup. Refusal must
    // precede reading this ABI or creating any task, for every flag shape.
    let child_stack = [0_u64; 1024];
    let stack_base = child_stack.as_ptr() as u64;
    for (name, number, flags, stack, tls, size) in [
        ("vfork", libc::SYS_vfork, 0_u64, 0, 0, 88),
        ("clone3", libc::SYS_clone3, 0, 0, 0, 88),
        (
            "clone3-shared",
            libc::SYS_clone3,
            (libc::CLONE_VM | libc::CLONE_VFORK) as u64,
            0,
            0,
            88,
        ),
        (
            "clone3-thread",
            libc::SYS_clone3,
            (libc::CLONE_VM | libc::CLONE_SIGHAND | libc::CLONE_THREAD) as u64,
            0,
            0,
            88,
        ),
        ("clone3-stack", libc::SYS_clone3, 0, stack_base, 0, 88),
        (
            "clone3-tls",
            libc::SYS_clone3,
            libc::CLONE_SETTLS as u64,
            0,
            u64::MAX,
            88,
        ),
        ("clone3-flags", libc::SYS_clone3, 1_u64 << 63, 0, 0, 88),
        ("clone3-size", libc::SYS_clone3, 0, 0, 0, 1),
    ] {
        let mut args = [0_u64; 11];
        args[0] = flags;
        args[4] = libc::SIGCHLD as u64;
        args[5] = stack;
        args[6] = if stack == 0 {
            0
        } else {
            std::mem::size_of_val(&child_stack) as u64
        };
        args[7] = tls;
        let result = unsafe { libc::syscall(number, args.as_ptr(), size as usize) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if result == 0 {
            // A faulty admission created a child. A shared child makes these
            // visible to the parent; the entire fixture must then fail.
            PARENT_MARKER.set(0);
            unsafe {
                core::ptr::write_volatile(stack_address, 0);
                libc::_exit(90);
            }
        }
        if result > 0 {
            // Own and reap any accidentally created child before failing.
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(result as libc::pid_t, &mut status, 0) };
                if waited == result as libc::pid_t {
                    break;
                }
                assert!(
                    waited == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR),
                    "failed to reap unexpected {name} child {result}"
                );
            }
        }
        assert_eq!(result, -1, "{name} created task {result}");
        assert_eq!(
            errno,
            libc::ENOTSUP,
            "{name} was not refused before forwarding"
        );
        assert_eq!(unsafe { core::ptr::read_volatile(stack_address) }, MARKER);
        assert_eq!(PARENT_MARKER.get(), MARKER);
        assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, pid);
        assert_eq!(unsafe { libc::syscall(libc::SYS_gettid) }, tid);
        results.push(format!("{name}={errno}"));
    }
    // Repeated callbacks after all refusals must still return to this parent.
    for _ in 0..4 {
        assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, pid);
    }
    println!(
        "unsafe process creation rejected: {} parent-canaries=unchanged",
        results.join(" ")
    );
}
