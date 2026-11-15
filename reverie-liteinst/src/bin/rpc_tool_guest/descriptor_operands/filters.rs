//! Guest seccomp filters on the call the runtime copies a guest operand with,
//! `process_vm_writev` from this process to itself, and a supervisor that
//! answers it for the cell that fabricates a copy. Each filter is installed
//! by the guest, through the run's syscall site, so the runtime's admission
//! of guest filters decides whether the cell can run.

use super::Site;
use super::errno_or_ret;
use super::report;

/// `AUDIT_ARCH_X86_64`.
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const SECCOMP_SET_MODE_FILTER: u64 = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: u64 = 1 << 3;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;
/// `SECCOMP_IOCTL_NOTIF_RECV`, `_IOWR('!', 0, struct seccomp_notif)`.
const SECCOMP_IOCTL_NOTIF_RECV: u64 = 0xc050_2100;
/// `SECCOMP_IOCTL_NOTIF_SEND`, `_IOWR('!', 1, struct seccomp_notif_resp)`.
const SECCOMP_IOCTL_NOTIF_SEND: u64 = 0xc018_2101;

/// `struct seccomp_data`.
#[repr(C)]
#[derive(Default)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

/// `struct seccomp_notif`, which `SECCOMP_IOCTL_NOTIF_RECV` requires zeroed.
#[repr(C)]
#[derive(Default)]
struct Notification {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}

/// `struct seccomp_notif_resp`.
#[repr(C)]
#[derive(Default)]
struct Response {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

const _: () = assert!(core::mem::size_of::<Notification>() == 80);
const _: () = assert!(core::mem::size_of::<Response>() == 24);

/// One classic BPF instruction.
const fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

/// Installs, as the guest, a filter that answers `process_vm_writev` with
/// `action` and allows every other call, with seccomp `flags`. Returns what
/// the install returned (the listener descriptor with
/// SECCOMP_FILTER_FLAG_NEW_LISTENER), or why the filter could not be
/// installed: guest filters not admitted in this configuration, or not
/// available on this host.
fn install(site: &Site, action: u32, flags: u64) -> Result<i64, String> {
    let no_new_privs = [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0];
    let refused = site.call(libc::SYS_prctl, no_new_privs).result;
    if refused != 0 {
        let answer = errno_or_ret(refused);
        return Err(format!("the guest's PR_SET_NO_NEW_PRIVS answered {answer}"));
    }
    let program = [
        // BPF_LD | BPF_W | BPF_ABS: the architecture. Any but x86-64 is allowed.
        instruction(0x20, 0, 0, 4),
        // BPF_JMP | BPF_JEQ | BPF_K.
        instruction(0x15, 0, 3, AUDIT_ARCH_X86_64),
        // The syscall number.
        instruction(0x20, 0, 0, 0),
        instruction(0x15, 0, 1, libc::SYS_process_vm_writev as u32),
        // BPF_RET | BPF_K.
        instruction(0x06, 0, 0, action),
        instruction(0x06, 0, 0, SECCOMP_RET_ALLOW),
    ];
    let fprog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_ptr().cast_mut(),
    };
    let program_address = (&raw const fprog) as u64;
    let arguments = [SECCOMP_SET_MODE_FILTER, flags, program_address, 0, 0, 0];
    let installed = site.call(libc::SYS_seccomp, arguments).result;
    if installed < 0 {
        let errno = -installed;
        return Err(format!(
            "guest seccomp filters not admitted: the guest's filter install answered errno={errno}"
        ));
    }
    Ok(installed)
}

/// A filter failing every `process_vm_writev` with EPERM, so that every copy
/// the runtime makes of a guest operand fails.
pub(super) fn deny_copies(site: &Site) -> Result<(), String> {
    let action = SECCOMP_RET_ERRNO | libc::EPERM as u32;
    install(site, action, 0).map(|_| ())
}

/// A filter sending every `process_vm_writev` to a supervisor; returns the
/// listener descriptor the supervisor answers on.
pub(super) fn notify_copies(site: &Site) -> Result<i32, String> {
    let flags = SECCOMP_FILTER_FLAG_NEW_LISTENER;
    let listener = install(site, SECCOMP_RET_USER_NOTIF, flags)?;
    Ok(listener as i32)
}

/// The supervisor, in a child of `target` that inherited the filter and its
/// `listener`: of `target`'s `process_vm_writev` calls it lets the first run,
/// answers the second with success and a count of `fabricated` bytes without
/// copying anything, and lets every later one run, until its own receive
/// fails (its target gone) or it is killed. Every call reaches the listener
/// through `site`, as the guest's own calls. It makes no `process_vm_writev`
/// of its own, which would wait for itself.
pub(super) fn supervise(
    site: &Site,
    listener: i32,
    target: u32,
    fabricated: i64,
    label: &str,
) -> ! {
    let listener = listener as u64;
    let mut seen = 0_u32;
    loop {
        let mut notification = Notification::default();
        let address = (&raw mut notification) as u64;
        let receive = [listener, SECCOMP_IOCTL_NOTIF_RECV, address, 0, 0, 0];
        if site.call(libc::SYS_ioctl, receive).result != 0 {
            unsafe { libc::_exit(0) };
        }
        let ours = notification.pid == target
            && i64::from(notification.data.nr) == libc::SYS_process_vm_writev;
        if ours {
            seen += 1;
            // The runtime's copy reads one guest range into one buffer.
            let counts = (notification.data.args[2], notification.data.args[4]);
            if counts != (1, 1) {
                let reason = format!("supervisor: copy {seen} has iovec counts {counts:?}");
                report("fail", "E12", label, &reason);
            }
        }
        let response = if ours && seen == 2 {
            Response {
                id: notification.id,
                val: fabricated,
                error: 0,
                flags: 0,
            }
        } else {
            Response {
                id: notification.id,
                flags: SECCOMP_USER_NOTIF_FLAG_CONTINUE,
                ..Response::default()
            }
        };
        let address = (&raw const response) as u64;
        let send = [listener, SECCOMP_IOCTL_NOTIF_SEND, address, 0, 0, 0];
        site.call(libc::SYS_ioctl, send);
    }
}
