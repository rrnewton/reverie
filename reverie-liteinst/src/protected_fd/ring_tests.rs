use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixDatagram;

use super::raw_syscall6;
use crate::runtime::guarded_raw_syscall;

fn isolated(name: &str) -> bool {
    let name = format!("{}::{name}", module_path!().split_once("::").unwrap().1);
    crate::test_process::isolated(&name, "LITEINST_RING_TEST")
}

fn register(fd: i32, opcode: u64, address: u64, count: u64, guarded: bool) -> i64 {
    let args = [fd as u64, opcode, address, count, 0, 0];
    if guarded {
        guarded_raw_syscall(libc::SYS_io_uring_register, args)
    } else {
        unsafe { raw_syscall6(libc::SYS_io_uring_register, args) }
    }
}

fn ring() -> Option<OwnedFd> {
    let mut params = [0u64; 32];
    let fd = unsafe {
        raw_syscall6(
            libc::SYS_io_uring_setup,
            [2, params.as_mut_ptr() as u64, 0, 0, 0, 0],
        )
    };
    if matches!(fd, value if value == -i64::from(libc::ENOSYS) || value == -i64::from(libc::EPERM) || value == -i64::from(libc::EACCES))
    {
        eprintln!("genuine io_uring registration controls unmeasured: native setup returned {fd}");
        return None;
    }
    assert!(fd >= 0, "native io_uring_setup returned {fd}");
    Some(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[test]
fn ring_registration_preserves_outer_errors_before_bad_pointer() {
    if !isolated("ring_registration_preserves_outer_errors_before_bad_pointer") {
        return;
    }
    let (reserved, _receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let ordinary = tempfile::tempfile().unwrap();
    let mut rows = Vec::new();
    for fd in [-1, ordinary.as_raw_fd()] {
        for opcode in [2, 4, 6, 7, 13, 14] {
            let args = [fd as u64, opcode, usize::MAX as u64, 1, 0, 0];
            let native = unsafe { raw_syscall6(libc::SYS_io_uring_register, args) };
            let guarded = guarded_raw_syscall(libc::SYS_io_uring_register, args);
            println!("ring fd={fd} opcode={opcode} native={native} guarded={guarded}");
            rows.push((native, guarded));
        }
    }
    assert_eq!(ordinary.metadata().unwrap().len(), 0);
    assert!(
        rows.iter().all(|(native, guarded)| native == guarded),
        "{rows:?}"
    );
}

#[test]
fn genuine_ring_faults_counts_and_registered_forms_match_native() {
    if !isolated("genuine_ring_faults_counts_and_registered_forms_match_native") {
        return;
    }
    let (reserved, _receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let Some(ring) = ring() else {
        return;
    };
    let ring_fd = ring.as_raw_fd();
    let mut registration = [u32::MAX as u64, ring_fd as u64];
    assert_eq!(
        register(ring_fd, 20, registration.as_mut_ptr() as u64, 1, false),
        1
    );
    let registered = registration[0] as i32;
    let event = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(event >= 0);
    let event = unsafe { OwnedFd::from_raw_fd(event) };
    let ordinary = tempfile::tempfile().unwrap();
    for (fd, flag) in [(ring_fd, 0), (registered, 1u64 << 31)] {
        for opcode in [2, 4, 6, 7, 13, 14] {
            for count in [0, 1, 32] {
                for pointer in [0, usize::MAX as u64] {
                    let native = register(fd, opcode | flag, pointer, count, false);
                    let guarded = register(fd, opcode | flag, pointer, count, true);
                    println!(
                        "ring-fault registered={} opcode={opcode} count={count} pointer={pointer:#x} native={native} guarded={guarded}",
                        flag != 0
                    );
                    assert_eq!(guarded, native);
                    // Each failed request must leave both registrations available.
                    let file_fd = ordinary.as_raw_fd();
                    assert_eq!(
                        register(ring_fd, 2, (&raw const file_fd) as u64, 1, false),
                        0
                    );
                    assert_eq!(register(ring_fd, 3, 0, 0, false), 0);
                    let event_fd = event.as_raw_fd();
                    assert_eq!(
                        register(ring_fd, 4, (&raw const event_fd) as u64, 1, false),
                        0
                    );
                    assert_eq!(register(ring_fd, 5, 0, 0, false), 0);
                }
            }
        }
    }
    registration = [registered as u64, 0];
    assert_eq!(
        register(ring_fd, 21, registration.as_ptr() as u64, 1, false),
        1
    );
}

#[test]
fn genuine_ring_protected_and_neighboring_registrations() {
    if !isolated("genuine_ring_protected_and_neighboring_registrations") {
        return;
    }
    let (reserved, _receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let Some(ring) = ring() else {
        return;
    };
    let ring_fd = ring.as_raw_fd();
    let ordinary = tempfile::tempfile().unwrap();
    let event = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(event >= 0);
    let event = unsafe { OwnedFd::from_raw_fd(event) };
    for opcode in [2, 4, 6, 7, 13, 14] {
        let update = matches!(opcode, 6 | 14);
        let file = !matches!(opcode, 4 | 7);
        let good = if file {
            ordinary.as_raw_fd()
        } else {
            event.as_raw_fd()
        };
        for guarded in [false, true] {
            if update {
                let empty = -1i32;
                assert_eq!(register(ring_fd, 2, (&raw const empty) as u64, 1, false), 0);
            }
            // Compare single-descriptor rejection with Linux's invalid-fd return.
            let rejected = if guarded {
                reserved.as_raw_fd()
            } else {
                -12345
            };
            for (descriptor, expected) in [
                (rejected, -i64::from(libc::EBADF)),
                (good, i64::from(update)),
            ] {
                let header = match opcode {
                    13 => [1, 0, (&raw const descriptor) as u64, 0],
                    _ => [0, (&raw const descriptor) as u64, 0, 1],
                };
                let address = if matches!(opcode, 6 | 13 | 14) {
                    header.as_ptr() as u64
                } else {
                    (&raw const descriptor) as u64
                };
                let count = if matches!(opcode, 13 | 14) { 32 } else { 1 };
                let result = register(ring_fd, opcode, address, count, guarded);
                println!(
                    "ring-descriptor opcode={opcode} guarded={guarded} protected={} result={result}",
                    descriptor == rejected
                );
                assert_eq!(result, expected);
            }
            assert_eq!(register(ring_fd, if file { 3 } else { 5 }, 0, 0, false), 0);
        }
    }
}
