//! PROTOTYPE 3c (throwaway; branch signals/proto-3c, nothing lands).

use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

use crate::trap::raw_syscall6;

const fn bit(signal: i32) -> u64 {
    1 << (signal - 1)
}

/// Every blockable signal except the runtime-reserved ones (SIGSYS, SIGSEGV,
/// SIGTRAP); SIGKILL and SIGSTOP cannot be blocked anyway.
pub const RUNTIME_MASK: u64 = !(bit(libc::SIGSYS)
    | bit(libc::SIGSEGV)
    | bit(libc::SIGTRAP)
    | bit(libc::SIGKILL)
    | bit(libc::SIGSTOP));

static SIGRETURNS: AtomicU64 = AtomicU64::new(0);

/// Counts a guest rt_sigreturn completed at the restorer, and says so on
/// stderr (prototype instrumentation).
pub fn note_sigreturn(stack_pointer: u64) {
    let count = SIGRETURNS.fetch_add(1, Ordering::Relaxed) + 1;
    let mut line = *b"proto3c: guest rt_sigreturn #0000 at the restorer\n";
    let digits = &mut line[28..32];
    let mut n = count;
    for slot in digits.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let _ = stack_pointer;
    unsafe {
        raw_syscall6(
            libc::SYS_write,
            [2, line.as_ptr() as u64, line.len() as u64, 0, 0, 0],
        )
    };
}

fn self_copy(write: bool, local: *mut u8, remote: u64, len: usize) -> bool {
    let l = libc::iovec { iov_base: local.cast(), iov_len: len };
    let r = libc::iovec { iov_base: remote as usize as *mut libc::c_void, iov_len: len };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let number = if write { libc::SYS_process_vm_writev } else { libc::SYS_process_vm_readv };
    let done = unsafe {
        raw_syscall6(
            number,
            [pid as u64, (&raw const l) as u64, 1, (&raw const r) as u64, 1, 0],
        )
    };
    done == len as i64
}

const UNBLOCKABLE: u64 = bit(libc::SIGKILL) | bit(libc::SIGSTOP);

/// `rt_sigprocmask` and `rt_sigpending` on the guest's mask (Linux's
/// sigprocmask rules: kernel/signal.c v6.12 `sigprocmask`, `do_sigpending`).
/// In the real design these rules are Detcore's; the backend only sets the
/// mask its completion installs.
///
/// # Safety
/// Called while forwarding the guest's own call, on its thread.
pub unsafe fn mask_call(number: i64, args: [u64; 6]) -> Option<i64> {
    let einval = -i64::from(libc::EINVAL);
    let efault = -i64::from(libc::EFAULT);
    match number {
        libc::SYS_rt_sigprocmask => {
            let old = super::continuation::proto_guest_mask()?;
            if args[3] != 8 {
                return Some(einval);
            }
            if args[1] != 0 {
                let mut set = 0u64;
                if !self_copy(false, (&raw mut set).cast(), args[1], 8) {
                    return Some(efault);
                }
                let new = match args[0] as i32 {
                    libc::SIG_BLOCK => old | set,
                    libc::SIG_UNBLOCK => old & !set,
                    libc::SIG_SETMASK => set,
                    _ => return Some(einval),
                } & !UNBLOCKABLE;
                super::continuation::proto_set_guest_mask(new);
            }
            if args[2] != 0 {
                let mut out = old;
                if !self_copy(true, (&raw mut out).cast(), args[2], 8) {
                    return Some(efault);
                }
            }
            Some(0)
        }
        libc::SYS_rt_sigpending => {
            let mask = super::continuation::proto_guest_mask()?;
            if args[1] > 8 {
                return Some(einval);
            }
            // The kernel's answer is pending & blocked; blocked is the runtime
            // mask here, which covers every signal the guest can have pending
            // except the reserved ones.
            let mut physical = 0u64;
            let r = unsafe {
                raw_syscall6(libc::SYS_rt_sigpending, [(&raw mut physical) as u64, 8, 0, 0, 0, 0])
            };
            if r != 0 {
                return Some(r);
            }
            let mut out = physical & mask;
            if !self_copy(true, (&raw mut out).cast(), args[0], args[1] as usize) {
                return Some(efault);
            }
            Some(0)
        }
        _ => None,
    }
}

thread_local! {
    /// The runtime's alternate stack while it is switched off.
    static PARKED_STACK: core::cell::Cell<Option<(u64, u64)>> = const { core::cell::Cell::new(None) };
}

/// Switches the runtime's alternate stack off when a signal is pending and
/// unblocked under `guest_mask` (so the kernel's guest frames at the
/// completion save SS_DISABLE, as Linux does for a thread with no alternate
/// stack, and SA_ONSTACK handlers run on the current stack), and back on
/// otherwise. Must run on a stack other than the alternate one: Linux refuses
/// the change while the thread is on it (kernel/signal.c do_sigaltstack).
pub fn switch_alternate_stack(guest_mask: u64) {
    let mut physical = 0u64;
    let r = unsafe {
        raw_syscall6(libc::SYS_rt_sigpending, [(&raw mut physical) as u64, 8, 0, 0, 0, 0])
    };
    let deliverable = r == 0 && physical & !guest_mask != 0;
    match (deliverable, PARKED_STACK.get()) {
        (true, None) => {
            let mut old = libc::stack_t { ss_sp: core::ptr::null_mut(), ss_flags: 0, ss_size: 0 };
            let off = libc::stack_t { ss_sp: core::ptr::null_mut(), ss_flags: libc::SS_DISABLE, ss_size: 0 };
            let r = unsafe {
                raw_syscall6(
                    libc::SYS_sigaltstack,
                    [(&raw const off) as u64, (&raw mut old) as u64, 0, 0, 0, 0],
                )
            };
            if r == 0 && old.ss_flags & libc::SS_DISABLE == 0 {
                PARKED_STACK.set(Some((old.ss_sp as u64, old.ss_size as u64)));
            }
        }
        (false, Some((base, size))) => {
            let on = libc::stack_t { ss_sp: base as usize as *mut libc::c_void, ss_flags: 0, ss_size: size as usize };
            let r = unsafe {
                raw_syscall6(libc::SYS_sigaltstack, [(&raw const on) as u64, 0, 0, 0, 0, 0])
            };
            if r == 0 {
                PARKED_STACK.set(None);
            }
        }
        _ => {}
    }
}

/// The deliveries the kernel will make at a return under `mask`, in Linux's
/// order (kernel/signal.c v6.12 `next_signal` 200-240: synchronous signals
/// first, then the lowest number; `dequeue_signal` 626-639: private before
/// shared), applying each handler's mask as the exit-to-user loop does before
/// the first handler runs. Standard signals only. In the real design this is
/// Detcore's.
pub struct Prediction {
    /// Caught signals, in delivery order.
    pub caught: [i32; 64],
    pub len: usize,
    /// A default-fatal delivery that ends the sequence, if any.
    pub fatal: Option<i32>,
}

const SYNCHRONOUS: u64 = bit(libc::SIGSEGV)
    | bit(libc::SIGBUS)
    | bit(libc::SIGILL)
    | bit(libc::SIGTRAP)
    | bit(libc::SIGFPE)
    | bit(libc::SIGSYS);
const DEFAULT_IGNORED: u64 =
    bit(libc::SIGCHLD) | bit(libc::SIGURG) | bit(libc::SIGWINCH) | bit(libc::SIGCONT);
const DEFAULT_STOP: u64 =
    bit(libc::SIGSTOP) | bit(libc::SIGTSTP) | bit(libc::SIGTTIN) | bit(libc::SIGTTOU);

fn next_signal(pending: u64, mask: u64) -> Option<i32> {
    let mut x = pending & !mask & 0xffff_ffff;
    if x == 0 {
        return None;
    }
    if x & SYNCHRONOUS != 0 {
        x &= SYNCHRONOUS;
    }
    Some(x.trailing_zeros() as i32 + 1)
}

fn read_pending() -> Option<(u64, u64)> {
    let path = b"/proc/thread-self/status\0";
    let fd = unsafe {
        raw_syscall6(
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, path.as_ptr() as u64, libc::O_RDONLY as u64 | libc::O_CLOEXEC as u64, 0, 0, 0],
        )
    };
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { raw_syscall6(libc::SYS_read, [fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0]) };
    unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
    if n <= 0 {
        return None;
    }
    let text = &buf[..n as usize];
    let field = |name: &[u8]| -> Option<u64> {
        let at = text.windows(name.len()).position(|w| w == name)? + name.len();
        let mut value = 0u64;
        for &c in text[at..].iter().skip_while(|c| **c == b'\t' || **c == b' ') {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                _ => break,
            };
            value = value << 4 | u64::from(d);
        }
        Some(value)
    };
    Some((field(b"SigPnd:")?, field(b"ShdPnd:")?))
}

pub fn predict(guest_mask: u64) -> Prediction {
    let mut prediction = Prediction { caught: [0; 64], len: 0, fatal: None };
    let Some((mut private, mut shared)) = read_pending() else {
        return prediction;
    };
    let mut mask = guest_mask & !UNBLOCKABLE;
    let mut reset = 0u64;
    for _ in 0..64 {
        let (sig, from_private) = match next_signal(private, mask) {
            Some(sig) => (sig, true),
            None => match next_signal(shared, mask) {
                Some(sig) => (sig, false),
                None => break,
            },
        };
        let b = bit(sig);
        if from_private { private &= !b } else { shared &= !b }
        let mut action = crate::signal::KernelSigaction { handler: 0, flags: 0, restorer: 0, mask: 0 };
        let _ = unsafe { crate::signal::raw_sigaction(sig, None, Some(&mut action)) };
        let handler = if reset & b != 0 { libc::SIG_DFL as u64 } else { action.handler };
        if handler == libc::SIG_IGN as u64 {
            continue;
        }
        if handler == libc::SIG_DFL as u64 {
            if DEFAULT_IGNORED & b != 0 {
                continue;
            }
            if DEFAULT_STOP & b == 0 {
                prediction.fatal = Some(sig);
            }
            break;
        }
        prediction.caught[prediction.len] = sig;
        prediction.len += 1;
        if action.flags & libc::SA_RESETHAND as u64 != 0 {
            reset |= b;
        }
        mask |= action.mask | if action.flags & libc::SA_NODEFER as u64 != 0 { 0 } else { b };
        mask &= !UNBLOCKABLE;
    }
    prediction
}

/// Prototype instrumentation: the final prediction at a return, on stderr.
pub fn report_final(prediction: &Prediction) {
    if prediction.len == 0 && prediction.fatal.is_none() {
        return;
    }
    let mut line = [0u8; 160];
    let mut n = 0;
    for &b in b"proto3c: predicted at this return:" {
        line[n] = b;
        n += 1;
    }
    for &sig in &prediction.caught[..prediction.len] {
        line[n] = b' ';
        n += 1;
        if sig >= 10 {
            line[n] = b'0' + (sig / 10) as u8;
            n += 1;
        }
        line[n] = b'0' + (sig % 10) as u8;
        n += 1;
    }
    if let Some(sig) = prediction.fatal {
        for &b in b" fatal:" {
            line[n] = b;
            n += 1;
        }
        if sig >= 10 {
            line[n] = b'0' + (sig / 10) as u8;
            n += 1;
        }
        line[n] = b'0' + (sig % 10) as u8;
        n += 1;
    }
    line[n] = b'\n';
    n += 1;
    unsafe { raw_syscall6(libc::SYS_write, [2, line.as_ptr() as u64, n as u64, 0, 0, 0]) };
}

/// Dequeues one pending instance of `sig` (a Tool suppressed its delivery),
/// private first, as the kernel would take it.
pub fn dequeue(sig: i32) {
    let set = bit(sig);
    let zero = [0i64; 2];
    unsafe {
        raw_syscall6(
            libc::SYS_rt_sigtimedwait,
            [(&raw const set) as u64, 0, zero.as_ptr() as u64, 8, 0, 0],
        )
    };
}

/// The `uc_sigmask` of the frame an x86-64 rt_sigreturn at `sp` reads: the
/// frame is at `sp - 8` and its ucontext at `sp`; uc_sigmask is at offset 296
/// (uc_flags 8, uc_link 8, uc_stack 24, uc_mcontext 256). `None` when the
/// guest's own rights cannot read it.
pub fn read_frame_mask(sp: u64) -> Option<u64> {
    let mut mask = 0u64;
    self_copy(false, (&raw mut mask).cast(), sp + 296, 8).then_some(mask)
}

/// Prototype instrumentation: a final read reached after more than one pass
/// of turns (a signal arrived while an earlier turn was taken).
/// PROTOTYPE 3c (P11): one line per committed announcement.
pub fn note_commit(sig: i32) {
    let mut line = *b"proto3c: commit 00\n";
    line[16] = b'0' + (sig / 10) as u8;
    line[17] = b'0' + (sig % 10) as u8;
    unsafe { raw_syscall6(libc::SYS_write, [2, line.as_ptr() as u64, line.len() as u64, 0, 0, 0]) };
}

/// PROTOTYPE 3c (P11): one line per withdrawn announcement.
pub fn note_withdraw(sig: i32) {
    let mut line = *b"proto3c: withdraw 00\n";
    line[18] = b'0' + (sig / 10) as u8;
    line[19] = b'0' + (sig % 10) as u8;
    unsafe { raw_syscall6(libc::SYS_write, [2, line.as_ptr() as u64, line.len() as u64, 0, 0, 0]) };
}

pub fn note_passes(passes: usize) {
    let mut line = *b"proto3c: final read after 0 passes of turns\n";
    line[26] = b'0' + passes.min(9) as u8;
    unsafe { raw_syscall6(libc::SYS_write, [2, line.as_ptr() as u64, line.len() as u64, 0, 0, 0]) };
}

/// Whether a caught signal is delivered at this return (the prediction holds a
/// caught one), on the running continuation's thread.
pub fn caught_deliverable() -> bool {
    super::continuation::proto_guest_mask().is_some_and(|mask| predict(mask).len > 0)
}

/// Linux's restart table (arch/x86/kernel/signal.c v6.12 `handle_signal`,
/// 260-282) for a call that ends with `result` because the first predicted
/// delivery is a caught signal: -ERESTARTSYS restarts with SA_RESTART, else
/// -EINTR. -ERESTARTNOHAND and -ERESTART_RESTARTBLOCK give -EINTR;
/// -ERESTARTNOINTR always restarts. In the real design this is Detcore's.
pub fn restart_table(result: i64) -> i64 {
    let eintr = -i64::from(libc::EINTR);
    let Some(mask) = super::continuation::proto_guest_mask() else {
        return eintr;
    };
    let prediction = predict(mask);
    if prediction.len == 0 {
        return eintr;
    }
    let mut action = crate::signal::KernelSigaction { handler: 0, flags: 0, restorer: 0, mask: 0 };
    let _ = unsafe { crate::signal::raw_sigaction(prediction.caught[0], None, Some(&mut action)) };
    let restart = match -result {
        512 => action.flags & libc::SA_RESTART as u64 != 0, // ERESTARTSYS
        513 => true,                                      // ERESTARTNOINTR
        _ => false,                                       // ERESTARTNOHAND, ERESTART_RESTARTBLOCK
    };
    if restart {
        super::continuation::proto_set_restart();
        // The result is replaced by the entry registers at the completion.
        result
    } else {
        eintr
    }
}

/// Prototype instrumentation: one fixed line on stderr.
pub fn note_text(text: &[u8]) {
    unsafe { raw_syscall6(libc::SYS_write, [2, text.as_ptr() as u64, text.len() as u64, 0, 0, 0]) };
}
