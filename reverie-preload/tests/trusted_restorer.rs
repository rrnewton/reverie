use reverie_preload::trap::raw_syscall6;
use reverie_preload::trap::trusted_gate;
use reverie_preload::trap::trusted_sigreturn_restorer;

#[repr(C)]
struct KernelAction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

unsafe extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, frame: *mut libc::c_void) {
    if signal != libc::SIGSYS
        || info.is_null()
        || frame.is_null()
        || unsafe { (*info).si_code } != 2
    {
        unsafe { raw_syscall6(libc::SYS_exit_group, [2, 0, 0, 0, 0, 0]) };
        return;
    }
    unsafe {
        (*frame.cast::<libc::ucontext_t>()).uc_mcontext.gregs[libc::REG_RAX as usize] = 424242
    };
}

fn raw(number: i64, args: [u64; 6]) -> i64 {
    unsafe { raw_syscall6(number, args) }
}

fn child_probe() -> ! {
    let mut instructions = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: libc::SYS_ptrace as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_KILL_PROCESS,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };
    let action = KernelAction {
        handler: handler as *const () as usize,
        flags: libc::SA_SIGINFO as u64 | 0x04000000,
        restorer: trusted_sigreturn_restorer as *const () as usize,
        mask: 0,
    };
    let gate = trusted_gate();
    let unblocked = 1u64 << (libc::SIGSYS - 1);
    let mut okay = raw(
        libc::SYS_prctl,
        [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
    ) == 0
        && raw(
            libc::SYS_prctl,
            [
                libc::PR_SET_SECCOMP as u64,
                2,
                (&raw const program) as u64,
                0,
                0,
                0,
            ],
        ) == 0
        && raw(
            libc::SYS_rt_sigaction,
            [libc::SIGSYS as u64, (&raw const action) as u64, 0, 8, 0, 0],
        ) == 0
        && raw(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_UNBLOCK as u64,
                (&raw const unblocked) as u64,
                0,
                8,
                0,
                0,
            ],
        ) == 0
        && raw(
            libc::SYS_prctl,
            [
                59,
                1,
                gate.syscall_ip,
                gate.return_ip - gate.syscall_ip + 1,
                0,
                0,
            ],
        ) == 0;
    if okay {
        let first = unsafe { libc::syscall(libc::SYS_getpid) };
        let native = raw(libc::SYS_getpid, [0; 6]);
        let second = unsafe { libc::syscall(libc::SYS_getpid) };
        okay = first == 424242 && second == 424242 && native > 0 && native != 424242;
    }
    raw(libc::SYS_exit_group, [u64::from(!okay), 0, 0, 0, 0, 0]);
    unreachable!()
}

#[test]
fn native_sud_restorer_returns_twice_through_existing_gate() {
    let gate = trusted_gate();
    assert_eq!(gate.return_ip, gate.syscall_ip + 2);
    let instruction = unsafe { std::slice::from_raw_parts(gate.syscall_ip as *const u8, 3) };
    assert_eq!(instruction, &[0x0f, 0x05, 0xc3]);
    let restorer = trusted_sigreturn_restorer as *const () as usize;
    let prefix = unsafe { std::slice::from_raw_parts(restorer as *const u8, 7) };
    assert_eq!(prefix, &[0x48, 0xc7, 0xc0, 15, 0, 0, 0]);
    let jump = unsafe { (restorer as *const u8).add(7).read() };
    let destination = match jump {
        0xeb => restorer
            .wrapping_add(9)
            .wrapping_add_signed(unsafe { ((restorer + 8) as *const i8).read() } as isize),
        0xe9 => restorer.wrapping_add(12).wrapping_add_signed(unsafe {
            ((restorer + 8) as *const i32).read_unaligned()
        } as isize),
        _ => panic!("restorer must tail-jump, not call or alter RSP: opcode {jump:#x}"),
    };
    assert_eq!(destination, gate.syscall_ip as usize);
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe { libc::alarm(5) };
        child_probe();
    }
    let mut status = -1;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert_eq!(status, 0, "native SUD restorer probe wait status {status}");
}
