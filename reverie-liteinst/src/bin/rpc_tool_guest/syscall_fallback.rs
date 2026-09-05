use core::arch::global_asm;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use std::path::Path;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Getpid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

static SITE: AtomicUsize = AtomicUsize::new(0);
static NATIVE_PID: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct FallbackTool;

#[reverie::tool]
impl Tool for FallbackTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = u32;

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid, Sysno::getuid, Sysno::getgid, Sysno::getppid]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (number, args) = syscall.into_parts();
        assert_eq!(
            [
                args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5
            ],
            [11, 22, 33, 44, 55, 66],
        );
        let registers = guest.regs().await;
        assert_eq!(registers.rip, SITE.load(Ordering::Relaxed) as u64);
        assert_ne!(registers.rsp, 0);
        let nested = unsafe { fallback_test_call(SITE.load(Ordering::Relaxed), libc::SYS_getpid) };
        assert_eq!(nested, NATIVE_PID.load(Ordering::Relaxed) as i64);
        let (total, senders) = guest.send_rpc(1).await;
        super::LAST_TOTAL.store(total, Ordering::Relaxed);
        super::LAST_SENDERS.store(senders, Ordering::Relaxed);
        unsafe { *libc::__errno_location() = libc::EINVAL };
        unsafe {
            core::arch::asm!("pxor xmm0, xmm0", out("xmm0") _, options(nostack, preserves_flags))
        };
        match number {
            Sysno::getpid => Ok(424_242),
            Sysno::getuid => Err(Errno::EPERM.into()),
            Sysno::getgid => guest.tail_inject(Getpid::new()).await,
            Sysno::getppid => {
                *guest.thread_state_mut() += 1;
                if *guest.thread_state() == 1 {
                    Err(Errno::ERESTARTSYS.into())
                } else {
                    Ok(777_777)
                }
            }
            _ => unreachable!(),
        }
    }
}

pub(super) fn run(path: &Path) {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let site = unsafe { mapping.cast::<u8>().add(page - 3) };
    unsafe { std::ptr::copy_nonoverlapping([0x0f, 0x05, 0xc3].as_ptr(), site, 3) };
    assert_eq!(
        unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) },
        0
    );
    let native_pid = unsafe { libc::getpid() };
    assert_ne!(native_pid, 424_242);
    SITE.store(site as usize, Ordering::Relaxed);
    NATIVE_PID.store(native_pid as usize, Ordering::Relaxed);
    unsafe { reverie_liteinst::install_tool::<FallbackTool>(path) }.unwrap();
    unsafe { *libc::__errno_location() = libc::E2BIG };
    for _ in 0..3 {
        assert_eq!(
            unsafe { fallback_test_call(site as usize, libc::SYS_getpid) },
            424_242
        );
    }
    assert_eq!(
        unsafe { fallback_test_call(site as usize, libc::SYS_getuid) },
        -i64::from(libc::EPERM)
    );
    assert_eq!(
        unsafe { fallback_test_call(site as usize, libc::SYS_getgid) },
        i64::from(native_pid)
    );
    assert_eq!(
        unsafe { fallback_test_call(site as usize, libc::SYS_getppid) },
        777_777
    );
    assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(site, 3) },
        [0x0f, 0x05, 0xc3]
    );
    assert_eq!(
        reverie_liteinst::reverie_liteinst_site_hook_count(site as u64),
        0
    );
    assert_eq!(
        reverie_liteinst::reverie_liteinst_site_trap_count(site as u64),
        6
    );
    assert_eq!(super::LAST_TOTAL.load(Ordering::Relaxed), 7);
    assert_eq!(super::LAST_SENDERS.load(Ordering::Relaxed), 1);
    println!("fallback: calls=6 rpc=7 hooks=0 bytes=unchanged abi=preserved");
}

unsafe extern "C" {
    pub(super) fn fallback_test_call(site: usize, number: i64) -> i64;
}

global_asm!(
    r#"
    .text
    .global fallback_test_call
    .hidden fallback_test_call
    .type fallback_test_call,@function
fallback_test_call:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    sub rsp, 40
    mov r12, rdi
    mov [rsp + 24], r12
    mov rax, rsi
    mov rbx, 71
    mov rbp, 72
    mov r13, 73
    mov r14, 74
    mov r15, 75
    mov rdi, 11
    mov rsi, 22
    mov rdx, 33
    mov r10, 44
    mov r8, 55
    mov r9, 66
    mov qword ptr [rsp - 16], 123456
    pcmpeqd xmm0, xmm0
    stc
    pushfq
    pop qword ptr [rsp]
    call r12
    mov [rsp + 16], r11
    pushfq
    pop r11
    cmp r11, [rsp]
    jne 9f
    cmp r11, [rsp + 16]
    jne 9f
    lea r11, [r12 + 2]
    cmp rcx, r11
    jne 9f
    cmp qword ptr [rsp - 16], 123456
    jne 9f
    cmp r12, [rsp + 24]
    jne 9f
    cmp rbx, 71
    jne 9f
    cmp rbp, 72
    jne 9f
    cmp r13, 73
    jne 9f
    cmp r14, 74
    jne 9f
    cmp r15, 75
    jne 9f
    cmp rdi, 11
    jne 9f
    cmp rsi, 22
    jne 9f
    cmp rdx, 33
    jne 9f
    cmp r10, 44
    jne 9f
    cmp r8, 55
    jne 9f
    cmp r9, 66
    jne 9f
    pmovmskb r11d, xmm0
    cmp r11d, 65535
    jne 9f
    add rsp, 40
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
9:
    ud2
    .size fallback_test_call, .-fallback_test_call
"#
);
