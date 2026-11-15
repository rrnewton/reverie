//! Cells E15 and E16, in the group EXCLUDED-DECLARED: io_uring state made
//! before activation is not covered. The program runs its own code first,
//! as a program calling the public installer itself may: it makes and maps
//! a submission-polling ring natively. Only then does it call
//! `install_tool`, which places the coordinator connection at a reserved
//! number R. After activation a new ring is refused (E15, ENOSYS on both
//! routes), but the old ring's polling thread shares the descriptor table:
//! one IORING_OP_STATX entry naming R, published through the mapped ring
//! with no system call, completes on R's socket (E16). These cells pin the
//! declared exclusion; they are never protected passes.
//!
//! E16 gives NO RESULT, never a pass, when there is not exactly one socket
//! at or above the runtime's floor (this mode configures no Tool output
//! socket, and the guest itself holds none), when the polling thread was
//! asleep as the entry was published (IORING_SQ_NEED_WAKEUP: only the
//! refused `io_uring_enter` could wake it), or when no completion arrives
//! within 5 s.

use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use super::Route;
use super::Site;
use super::activate;
use super::descriptors_from;
use super::io_uring;
use super::report;
use super::ring_setup_cell;
use super::runtime_floor;

/// The polling thread's idle time before it sleeps: long enough that it is
/// awake when the entry is published, after installation.
const IDLE_MS: u32 = 60_000;
const COMPLETION_LIMIT: Duration = Duration::from_secs(5);
const STATX_TAG: u64 = 0x7374_6174;

/// The inode a `socket:[N]` link names.
fn socket_inode(target: &str) -> Option<u64> {
    let inode = target.strip_prefix("socket:[")?.strip_suffix(']')?;
    inode.parse().ok()
}

pub(super) fn run(path: &Path) {
    // 1. Before the runtime, natively: the ring, mapped, and the routes'
    // sites.
    let ring = match io_uring::PollingRing::create(4, IDLE_MS) {
        Ok(ring) => ring,
        Err(reason) => {
            for (id, route) in [("E15", "hook"), ("E15", "trap"), ("E16", "ring")] {
                report("refused", id, route, &reason);
            }
            return;
        }
    };
    let hook = Site::new(Route::Hook);
    let trap = Site::new(Route::Trap);
    // 2. Activation: the explicit installer, after the program's own code.
    activate(path, "ring");
    // 3. E15: a new ring is refused on both routes. The first call at the
    // hook route's site patches it.
    hook.call(libc::SYS_getppid, [0; 6]);
    ring_setup_cell("E15", &hook);
    ring_setup_cell("E15", &trap);
    // 4. R through /proc/self/fd: the one socket at or above the floor.
    let floor = runtime_floor();
    let sockets: Vec<(i32, u64)> = descriptors_from(floor)
        .into_iter()
        .filter_map(|(fd, target)| Some((fd, socket_inode(&target)?)))
        .collect();
    let setup = format!("ring={} floor={floor} sockets={sockets:?}", ring.fd);
    report("setup", "E16", "ring", &setup);
    let &[(reserved, inode)] = sockets.as_slice() else {
        let reason = format!("not exactly one socket at or above the runtime floor {floor}");
        report("no-result", "E16", "ring", &reason);
        return;
    };
    // 5. One statx of R, published with no system call. The path and the
    // zeroed output buffer live as long as the process.
    let statx = Box::into_raw(Box::new(unsafe { core::mem::zeroed::<libc::statx>() }));
    let entry = io_uring::Sqe {
        opcode: io_uring::IORING_OP_STATX,
        fd: reserved,
        addr: c"".as_ptr() as u64,
        len: libc::STATX_TYPE | libc::STATX_INO,
        off: statx as u64,
        op_flags: libc::AT_EMPTY_PATH as u32,
        user_data: STATX_TAG,
        ..io_uring::Sqe::default()
    };
    ring.publish(entry);
    if ring.submission_flags() & io_uring::IORING_SQ_NEED_WAKEUP != 0 {
        let reason = "IORING_SQ_NEED_WAKEUP: the polling thread slept as the entry was published";
        report("no-result", "E16", "ring", reason);
        return;
    }
    // 6. The completion, polled in memory.
    let deadline = Instant::now() + COMPLETION_LIMIT;
    let completion = loop {
        if let Some(completion) = ring.completion() {
            break Some(completion);
        }
        if Instant::now() >= deadline {
            break None;
        }
        unsafe { libc::sched_yield() };
    };
    let Some(completion) = completion else {
        let reason = format!("no completion within {COMPLETION_LIMIT:?}");
        report("no-result", "E16", "ring", &reason);
        return;
    };
    // The completion was read with acquire ordering after Linux wrote the
    // buffer.
    let statx = unsafe { statx.read_volatile() };
    let kind = u32::from(statx.stx_mode) & libc::S_IFMT;
    let kind = if kind == libc::S_IFSOCK {
        "S_IFSOCK".to_string()
    } else {
        format!("{kind:#o}")
    };
    let ino = if statx.stx_ino == inode {
        "match".to_string()
    } else {
        format!("{}-not-{inode}", statx.stx_ino)
    };
    let mut observed = format!("res={} type={kind} ino={ino}", completion.res);
    if completion.user_data != STATX_TAG {
        observed.push_str(&format!(" user_data={:#x}", completion.user_data));
    }
    report("cell", "E16", "ring", &observed);
}
