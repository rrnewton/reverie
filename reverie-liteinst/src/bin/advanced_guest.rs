use std::process;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;

const WORKERS: usize = 4;
const THREAD_ITERATIONS: usize = 2_000;
const SIGNAL_ITERATIONS: usize = 2_000;
const FORK_ITERATIONS: usize = 16;
const THREAD_CHURN: usize = 1_200;

static SIGNALS: AtomicUsize = AtomicUsize::new(0);
static SIGNAL_ACK_FD: AtomicI32 = AtomicI32::new(-1);
static RAW_THREAD_DONE: AtomicI32 = AtomicI32::new(0);

unsafe extern "C" fn signal_handler(_signal: libc::c_int) {
    let _ = unsafe { libc::syscall(libc::SYS_gettid) };
    SIGNALS.fetch_add(1, Ordering::Relaxed);
    let acknowledgment = b's';
    let written = unsafe {
        libc::syscall(
            libc::SYS_write,
            SIGNAL_ACK_FD.load(Ordering::Relaxed),
            &acknowledgment,
            1,
        )
    };
    if written != 1 {
        unsafe { libc::_exit(3) };
    }
}

fn install_signal_handler() {
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_flags = libc::SA_RESTART;
    action.sa_sigaction = signal_handler as *const () as usize;
    assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
        0
    );

    let mut blocked: libc::sigset_t = unsafe { core::mem::zeroed() };
    assert_eq!(unsafe { libc::sigemptyset(&mut blocked) }, 0);
    assert_eq!(unsafe { libc::sigaddset(&mut blocked, libc::SIGUSR1) }, 0);
    assert_eq!(
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()) },
        0
    );
}

fn syscall_burst(iterations: usize, mut state: u64) {
    for _ in 0..iterations {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let _ = unsafe { libc::syscall(libc::SYS_getpid) };
        if state & 7 == 0 {
            unsafe {
                libc::sched_yield();
            }
        }
    }
}

fn run_threads(seed: u64) {
    let barrier = Arc::new(Barrier::new(WORKERS + 1));
    let workers: Vec<_> = (0..WORKERS)
        .map(|worker| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                syscall_burst(THREAD_ITERATIONS, seed ^ (worker as u64 + 1));
            })
        })
        .collect();
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
}

fn run_thread_churn() {
    for _ in 0..THREAD_CHURN {
        thread::spawn(|| {
            let _ = unsafe { libc::syscall(libc::SYS_getpid) };
        })
        .join()
        .unwrap();
    }
}

extern "C" fn raw_thread_main(_argument: *mut libc::c_void) -> libc::c_int {
    let _ = unsafe { libc::syscall(libc::SYS_getpid) };
    RAW_THREAD_DONE.store(1, Ordering::Release);
    unsafe {
        libc::syscall(libc::SYS_exit, 0);
    }
    unreachable!()
}

fn run_raw_thread() {
    RAW_THREAD_DONE.store(0, Ordering::Relaxed);
    let mut stack = vec![0_u8; 1024 * 1024];
    let stack_top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast();
    let flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM;
    let tid = unsafe {
        libc::clone(
            raw_thread_main,
            stack_top,
            flags,
            std::ptr::null_mut::<libc::c_void>(),
        )
    };
    assert!(tid > 0, "clone failed: {}", std::io::Error::last_os_error());
    while RAW_THREAD_DONE.load(Ordering::Acquire) == 0 {
        unsafe {
            libc::sched_yield();
        }
    }
    std::mem::forget(stack);
}

fn spawn_signal_sender(pid: libc::pid_t, tid: libc::pid_t) -> libc::pid_t {
    let mut acknowledgments = [0; 2];
    assert_eq!(unsafe { libc::pipe(acknowledgments.as_mut_ptr()) }, 0);
    SIGNAL_ACK_FD.store(acknowledgments[1], Ordering::Relaxed);

    let child = unsafe { libc::fork() };
    if child == 0 {
        unsafe {
            libc::close(acknowledgments[1]);
        }
        for _ in 0..SIGNAL_ITERATIONS {
            let mut acknowledgment = 0_u8;
            if unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR1) } != 0
                || unsafe { libc::read(acknowledgments[0], &mut acknowledgment as *mut u8 as _, 1) }
                    != 1
            {
                unsafe { libc::_exit(2) };
            }
        }
        unsafe {
            libc::_exit(0);
        }
    }
    assert!(
        child > 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        libc::close(acknowledgments[0]);
    }
    child
}

fn wait_child(child: libc::pid_t) {
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status), "child status {status}");
    assert_eq!(libc::WEXITSTATUS(status), 0, "child status {status}");
}

fn run_signal_storm() {
    SIGNALS.store(0, Ordering::Relaxed);
    install_signal_handler();
    let pid = unsafe { libc::getpid() };
    let tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };
    let sender = spawn_signal_sender(pid, tid);

    wait_for_signals();
    wait_child(sender);
    assert_eq!(SIGNALS.load(Ordering::Relaxed), SIGNAL_ITERATIONS);
    assert_eq!(
        unsafe { libc::close(SIGNAL_ACK_FD.swap(-1, Ordering::Relaxed)) },
        0
    );
    unblock_signal();
}

fn wait_for_signals() {
    let mut unblocked: libc::sigset_t = unsafe { core::mem::zeroed() };
    assert_eq!(unsafe { libc::sigemptyset(&mut unblocked) }, 0);
    for expected in 1..=SIGNAL_ITERATIONS {
        while SIGNALS.load(Ordering::Relaxed) < expected {
            let result =
                unsafe { libc::ppoll(std::ptr::null_mut(), 0, std::ptr::null(), &unblocked) };
            assert_eq!(result, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR)
            );
        }
    }
}

fn unblock_signal() {
    let mut blocked: libc::sigset_t = unsafe { core::mem::zeroed() };
    assert_eq!(unsafe { libc::sigemptyset(&mut blocked) }, 0);
    assert_eq!(unsafe { libc::sigaddset(&mut blocked, libc::SIGUSR1) }, 0);
    assert_eq!(
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &blocked, std::ptr::null_mut()) },
        0
    );
}

fn run_fork_stress() {
    for _ in 0..FORK_ITERATIONS {
        let child = unsafe { libc::fork() };
        if child == 0 {
            let _ = unsafe { libc::syscall(libc::SYS_getpid) };
            unsafe {
                libc::_exit(0);
            }
        }
        assert!(
            child > 0,
            "fork failed: {}",
            std::io::Error::last_os_error()
        );
        wait_child(child);
    }
}

fn run_forked_thread_churn() {
    run_thread_churn();
    let child = unsafe { libc::fork() };
    if child == 0 {
        run_thread_churn();
        unsafe {
            libc::_exit(0);
        }
    }
    assert!(
        child > 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    wait_child(child);
}

fn run_chaos(seed: u64) {
    SIGNALS.store(0, Ordering::Relaxed);
    install_signal_handler();
    let pid = unsafe { libc::getpid() };
    let tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };
    let sender = spawn_signal_sender(pid, tid);

    let barrier = Arc::new(Barrier::new(WORKERS + 1));
    let workers: Vec<_> = (0..WORKERS)
        .map(|worker| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                syscall_burst(
                    THREAD_ITERATIONS,
                    seed.rotate_left(worker as u32) ^ (worker as u64 + 1),
                );
            })
        })
        .collect();
    barrier.wait();
    run_fork_stress();
    for worker in workers {
        worker.join().unwrap();
    }
    wait_for_signals();
    wait_child(sender);
    assert_eq!(SIGNALS.load(Ordering::Relaxed), SIGNAL_ITERATIONS);
    assert_eq!(
        unsafe { libc::close(SIGNAL_ACK_FD.swap(-1, Ordering::Relaxed)) },
        0
    );
    unblock_signal();
}

fn main() {
    let mut arguments = std::env::args();
    let _program = arguments.next();
    match arguments.next().as_deref() {
        Some("threads") => {
            run_threads(1);
            println!("threads-ok");
        }
        Some("raw-thread") => {
            run_raw_thread();
            println!("raw-thread-ok");
        }
        Some("thread-churn") => {
            run_thread_churn();
            println!("thread-churn-ok");
        }
        Some("signals") => {
            run_signal_storm();
            println!("signals-ok");
        }
        Some("fork") => {
            run_fork_stress();
            println!("fork-ok");
        }
        Some("fork-churn") => {
            run_forked_thread_churn();
            println!("fork-churn-ok");
        }
        Some("chaos") => {
            let seed = arguments
                .next()
                .and_then(|seed| seed.parse::<u64>().ok())
                .unwrap_or_else(|| {
                    eprintln!("chaos mode requires a u64 seed");
                    process::exit(2);
                });
            run_chaos(seed);
            println!("chaos-ok");
        }
        _ => {
            eprintln!(
                "usage: reverie-liteinst-advanced-guest threads|raw-thread|thread-churn|signals|fork|fork-churn|chaos SEED"
            );
            process::exit(2);
        }
    }
}
