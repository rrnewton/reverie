/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::time::Duration;
use std::time::Instant;

use nix::sys::ptrace;
use nix::sys::signal::Signal;
use nix::sys::wait::WaitPidFlag;
use nix::sys::wait::WaitStatus;
use nix::sys::wait::waitpid;
use nix::unistd::ForkResult;
use nix::unistd::Pid;
use nix::unistd::fork;
use reverie_memory::Addr;
use reverie_memory::MemoryAccess;

use super::*;

thread_local! {
    static NATIVE_TRANSFERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROC_OBSERVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn note_native_transfer() {
    NATIVE_TRANSFERS.with(|count| count.set(count.get() + 1));
}

pub(super) fn note_proc_observation() {
    PROC_OBSERVATIONS.with(|count| count.set(count.get() + 1));
}

// Parser premises, not observations of a kernel mapping or successful reads.
fn smaps(key: u8) -> String {
    format!(
        "1000-2000 rw-p 00000000 00:00 0\nSize: 4 kB\nKernelPageSize: 4 kB\nMMUPageSize: 4 kB\nLazyFree: 0 kB\nProtectionKey: {key}\nVmFlags: rd wr mr mw me ac sd\n"
    )
}

#[test]
fn native_read_mapping_keys_use_ad_only_for_every_key() {
    for key in 0..16 {
        let bytes = smaps(key);
        let map = mapping(bytes.as_bytes(), 0x1000, 0x1200).unwrap();
        for bits in 0u32..4 {
            let expected = if bits & 1 != 0 {
                Err(Error::Fault(Fault::ProtectionKey(key)))
            } else {
                Ok(())
            };
            assert_eq!(map.read_access(bits << (2 * key)), expected);
        }
        let other_key = (key + 1) % 16;
        assert_eq!(map.read_access(3 << (2 * other_key)), Ok(()));
    }
}

#[test]
fn native_read_xstate_decode_preserves_all_keys_and_rejects_unknown_state() {
    // Explicit standard-layout parser premises, never a target-state receipt.
    let layout = super::super::NativePkruLayout {
        offset: 576,
        size: 8,
        user_features: 1 << 9,
    };
    let mut state = vec![0; 584];
    state[512..520].copy_from_slice(&(1u64 << 9).to_le_bytes());
    for pkru in [0u32, 1, 2, 3, 0x5555_5555, 0xaaaa_aaaa, 0xffff_ffff] {
        state[576..580].copy_from_slice(&pkru.to_le_bytes());
        assert_eq!(decode_native_pkru_xstate(&state, layout), Ok(pkru));
    }
    for length in [0, 512, 575, 576, 580, 583] {
        assert_eq!(
            decode_native_pkru_xstate(&state[..length], layout),
            Err(Errno::EOPNOTSUPP)
        );
    }
    for (offset, value) in [(519, 0x80), (520, 1), (527, 0x80), (528, 1), (580, 1)] {
        let mut malformed = state.clone();
        malformed[offset] |= value;
        assert_eq!(
            decode_native_pkru_xstate(&malformed, layout),
            Err(Errno::EOPNOTSUPP)
        );
    }
    state[512..520].fill(0);
    state[576..584].fill(0xff);
    assert_eq!(decode_native_pkru_xstate(&state, layout), Ok(0));
}

#[test]
fn native_read_mapping_metadata_missing_duplicate_truncated_and_malformed_refuse() {
    let valid = smaps(0);
    let cases = [
        String::new(),
        valid[..valid.len() - 1].into(),
        valid.replace("ProtectionKey: 0\n", ""),
        valid.replace("ProtectionKey: 0\n", "ProtectionKey: 0\nProtectionKey: 0\n"),
        valid.replace("ProtectionKey: 0", "ProtectionKey: 16"),
        valid.replace("ProtectionKey: 0", "ProtectionKey: -1"),
        valid.replace("ProtectionKey: 0", "ProtectionKey: 0 1"),
        valid.replace("ProtectionKey: 0", "ProtectionKey: bogus"),
        valid.replace("ProtectionKey: 0", "ProtectionKey: \u{2003}0"),
        valid.replace("KernelPageSize: 4 kB\n", ""),
        valid.replace(
            "MMUPageSize: 4 kB\n",
            "MMUPageSize: 4 kB\nMMUPageSize: 4 kB\n",
        ),
        valid.replace("VmFlags: rd wr mr mw me ac sd\n", ""),
        valid.replace(
            "VmFlags: rd wr mr mw me ac sd\n",
            "VmFlags: rd wr mr mw me ac sd\nVmFlags: rd wr\n",
        ),
        valid.replace("VmFlags: rd wr", "VmFlags: rd rd wr"),
        valid.replace("rw-p", "r--p"),
        valid.replace("1000-2000", "1000-1000"),
        valid.replace("1000-2000", "1001-2000"),
        valid.replace("1000-2000", "1000-10000000000000000"),
        format!("{valid}{valid}"),
        format!("{valid}2000-3000 rw-p 00000000 00:00 0\n"),
    ];
    for bytes in cases {
        assert!(
            matches!(
                mapping(bytes.as_bytes(), 0x1000, 0x1008),
                Err(Error::Refused(Refusal::MappingMetadata))
            ),
            "{bytes:?}"
        );
    }
    assert!(matches!(
        mapping(&[b'x'; MAX_SMAPS + 1], 0x1000, 0x1008),
        Err(Error::Refused(Refusal::MetadataTooLarge))
    ));
    assert!(mapping(b"\xff\n", 0x1000, 0x1008).is_err());
}

#[test]
fn native_read_mapping_special_shapes_and_missing_stack_are_not_guest_faults() {
    let valid = smaps(0);
    for flag in [
        "io", "pf", "mm", "um", "uw", "ui", "ht", "ss", "ar", "??", "gu", "dp",
    ] {
        let bytes = valid.replace("ac sd", &format!("ac sd {flag}"));
        assert_eq!(
            mapping(bytes.as_bytes(), 0x1000, 0x1200)
                .unwrap()
                .read_access(0),
            Err(refused(Refusal::UnsupportedMapping))
        );
    }
    for field in ["KernelPageSize", "MMUPageSize"] {
        let bytes = valid.replace(&format!("{field}: 4 kB"), &format!("{field}: 64 kB"));
        assert_eq!(
            mapping(bytes.as_bytes(), 0x1000, 0x1200)
                .unwrap()
                .read_access(0),
            Err(refused(Refusal::UnsupportedMapping))
        );
    }
    for (perms, flags) in [("-w-p", "wr"), ("--xp", "ex")] {
        let bytes = valid.replace("rw-p", perms).replace("rd wr", flags);
        assert_eq!(
            mapping(bytes.as_bytes(), 0x1000, 0x1200)
                .unwrap()
                .read_access(0),
            Err(refused(Refusal::UnsupportedMapping))
        );
    }
    let none = valid.replace("rw-p", "---p").replace("rd wr ", "");
    assert_eq!(
        mapping(none.as_bytes(), 0x1000, 0x1200)
            .unwrap()
            .read_access(0),
        Err(Error::Fault(Fault::NoAccessMapping))
    );
    let stack = valid.replace("ac sd", "ac sd gd");
    assert_eq!(
        mapping(stack.as_bytes(), 0x1000, 0x1200)
            .unwrap()
            .read_access(0),
        Ok(())
    );
    assert!(matches!(
        mapping(stack.as_bytes(), 0xf00, 0xf08),
        Err(Error::Refused(Refusal::MappingMissing))
    ));
}

#[test]
fn native_read_backing_metadata_premises_reject_shared_file_special_and_lazy_free() {
    // These are parser specimens, not kernel backing/immutability receipts.
    let valid = smaps(0);
    for (label, bytes) in [
        ("private-anonymous", valid.clone()),
        ("file-device", valid.replace("00:00 0", "08:01 0")),
        ("file-inode", valid.replace("00:00 0", "00:00 47")),
        ("file-offset", valid.replace("00000000", "00001000")),
        (
            "may-share",
            valid.replace("rw-p", "rw-s").replace("ac sd", "ac sd ms"),
        ),
        ("shared", valid.replace("ac sd", "ac sd sh")),
        ("kernel-special", valid.replace("ac sd", "ac sd de")),
        ("mergeable", valid.replace("ac sd", "ac sd mg")),
        (
            "lazy-free",
            valid.replace("LazyFree: 0 kB", "LazyFree: 4 kB"),
        ),
        // Neither a private-looking name nor a reported private/COW folio
        // removes the VMA's file backing identity.
        (
            "named-file",
            valid.replace("00:00 0", "08:01 47 [anon:premise]"),
        ),
        (
            "private-file-cow",
            valid.replace("00:00 0", "08:01 47").replacen(
                "Size: 4 kB\n",
                "Size: 4 kB\nAnonymous: 4 kB\n",
                1,
            ),
        ),
    ] {
        let map = mapping(bytes.as_bytes(), 0x1000, 0x1200).unwrap();
        assert_eq!(
            map.read_access(0),
            if label == "private-anonymous" {
                Ok(())
            } else {
                Err(refused(Refusal::UnsupportedBacking))
            },
            "{label}",
        );
    }
    let named = valid.replace("00:00 0", "00:00 0 [anon:documentary]");
    let map = mapping(named.as_bytes(), 0x1000, 0x1200).unwrap();
    assert_eq!((map.offset, map.device, map.inode), (0, (0, 0), 0));
    assert_eq!(map.read_access(0), Ok(())); // name grants no extra authority
}

#[test]
fn native_read_backing_metadata_missing_malformed_and_duplicate_refuse_premises() {
    let valid = smaps(0);
    for bytes in [
        valid.replace("00:00", "00:"),
        valid.replace("00:00", "bogus:00"),
        valid.replace("00:00 0", "00:00"),
        valid.replace("00:00 0", "00:00 -1"),
        valid.replace("00:00 0", "00:00 18446744073709551616"),
        valid.replace("LazyFree: 0 kB\n", ""),
        valid.replace("LazyFree: 0 kB\n", "LazyFree: 0 kB\nLazyFree: 0 kB\n"),
        valid.replace("LazyFree: 0 kB", "LazyFree: -1 kB"),
        valid.replace("LazyFree: 0 kB", "LazyFree: 0 bytes"),
        valid.replace("LazyFree: 0 kB", "LazyFree: 0 kB trailing"),
        valid.replace("KernelPageSize: 4 kB", "KernelPageSize: 0 kB"),
    ] {
        assert!(
            matches!(
                mapping(bytes.as_bytes(), 0x1000, 0x1200),
                Err(Error::Refused(Refusal::MappingMetadata))
            ),
            "{bytes:?}",
        );
    }
}

#[test]
fn native_read_bounds_proc_view_and_transport_refusals_preserve_canaries() {
    for (address, length) in [
        (0x1000, 0),
        (0x1000, 513),
        (usize::MAX, 1),
        (usize::MAX - 7, 8),
        (0x1fff, 2),
        (1usize << 47, 1),
    ] {
        assert_eq!(
            range_end(address, length),
            Err(refused(Refusal::UnsupportedRange))
        );
    }
    assert_eq!(range_end(0x1000, 512), Ok(0x1200));
    assert_eq!(
        proc_view(b"Name:\tfixture\nPid:\t7\nNSpid:\t7\n", 7),
        Ok(())
    );
    for status in [
        &b"Pid:\t7\n"[..],
        b"Pid:\t7\nNSpid:\t70 7\n",
        b"Pid:\t7\nNSpid:\t7\nNSpid:\t7\n",
        b"Pid:\t8\nNSpid:\t8\n",
        b"Pid:\t7\nNSpid:\t7",
        b"Pid:\t7\nPid:\t7\nNSpid:\t7\n",
        b"Pid:\t7\nNSpid:\t\xff\n",
    ] {
        assert_eq!(
            proc_view(status, 7),
            Err(refused(Refusal::ProcfsViewMismatch))
        );
    }
    // Explicit transport premises: even native EFAULT is not a proven user
    // fault. No failing/short transport outcome may publish staged bytes.
    for result in [
        Err(Errno::EFAULT),
        Err(Errno::EPERM),
        Err(Errno::ESRCH),
        Err(Errno::ENOMEM),
        Err(Errno::EINTR),
        Ok(0),
        Ok(7),
        Ok(9),
    ] {
        let mut canary = [0xa5; 10];
        let expected = match result {
            Err(e) => refused(Refusal::NativeTransfer(e)),
            Ok(n) => refused(Refusal::ShortTransfer(n)),
        };
        assert_eq!(
            publish(result, &[0x3c; MAX_READ], &mut canary[1..9]),
            Err(expected)
        );
        assert_eq!(canary, [0xa5; 10]);
    }
    let mut canary = [0xa5; 10];
    assert_eq!(publish(Ok(8), &[0x3c; MAX_READ], &mut canary[1..9]), Ok(()));
    assert_eq!(
        canary,
        [0xa5, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0xa5]
    );
}

#[test]
fn native_read_proc_files_require_same_held_proc_mount() {
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open("/proc")
        .unwrap();
    let mount = verify_proc(&root, None).unwrap();
    assert_eq!(verify_proc(&root, Some(mount)), Ok(mount));
    assert_eq!(
        verify_proc(
            &root,
            Some(ProcMount {
                device: mount.device ^ 1,
                ..mount
            })
        ),
        Err(refused(Refusal::ProcfsViewMismatch))
    );
    assert_eq!(
        verify_proc(
            &root,
            Some(ProcMount {
                mount_id: mount.mount_id ^ 1,
                ..mount
            })
        ),
        Err(refused(Refusal::ProcfsViewMismatch))
    );
    let non_proc = File::open("/dev/null").unwrap();
    assert_eq!(
        verify_proc(&non_proc, None),
        Err(refused(Refusal::ProcfsViewMismatch))
    );
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as usize;
    let status = proc_file(&root, mount, "thread-self/status", MAX_STATUS).unwrap();
    assert_eq!(proc_view(&status, tid), Ok(()));
    assert_eq!(
        proc_file(&root, mount, "thread-self/status", 1),
        Err(refused(Refusal::MetadataTooLarge))
    );
}

struct Page(*mut libc::c_void);
impl Page {
    fn new() -> Self {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(ptr, libc::MAP_FAILED);
        unsafe { std::ptr::write_bytes(ptr.cast::<u8>(), 0x3c, PAGE) };
        Self(ptr)
    }
}
impl Drop for Page {
    fn drop(&mut self) {
        assert_eq!(unsafe { libc::munmap(self.0, PAGE) }, 0);
    }
}

struct Child {
    pid: Pid,
    reaped: bool,
}
impl Child {
    fn event(&mut self) -> WaitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::EINTR) => (),
                result => {
                    let status = result.unwrap();
                    if matches!(&status, WaitStatus::Exited(..) | WaitStatus::Signaled(..)) {
                        // Record reap before an assertion can panic, so Drop
                        // never signals an already-reaped/recyclable PID.
                        self.reaped = true;
                    }
                    return status;
                }
            }
            assert!(Instant::now() < deadline, "exact child wait deadline");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            // Panic/timeout guard only: this child has not been reaped or
            // released, so its exact PID cannot have been recycled.
            unsafe {
                libc::kill(self.pid.as_raw(), libc::SIGKILL);
            }
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(self.pid.as_raw(), &mut status, 0) };
                if waited == self.pid.as_raw() {
                    if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                        break;
                    }
                    let _ = ptrace::cont(self.pid, Signal::SIGKILL);
                    continue;
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
        }
    }
}

// Actual stopped child and actual pipe write. Its syscall happens before INT3
// and leaves the raw count/-errno in R8. No stack/data instruction executes
// while key0 is denied; only WRPKRU, register instructions and the native syscall.
fn native_child(
    protection: i32,
    key: i32,
    pkru: u32,
    length: usize,
    check: impl FnOnce(&Stopped, i32, usize, i64),
) {
    native_child_page(Page::new(), protection, key, pkru, length, check);
}

fn native_child_page(
    page: Page,
    protection: i32,
    key: i32,
    pkru: u32,
    length: usize,
    check: impl FnOnce(&Stopped, i32, usize, i64),
) {
    let layout = native_pkru_layout()
        .expect("supported native PKRU layout")
        .expect("native WRPKRU qualification requires OSPKE; no skip");
    let address = page.0 as usize;
    let mut pipe = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    let input = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let output = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    match unsafe { fork() }.unwrap() {
        ForkResult::Child => {
            if ptrace::traceme().is_err() {
                unsafe { libc::_exit(90) }
            }
            let protected = unsafe {
                if key == 0 {
                    libc::mprotect(page.0, PAGE, protection) as libc::c_long
                } else {
                    libc::syscall(libc::SYS_pkey_mprotect, page.0, PAGE, protection, key)
                }
            };
            if protected != 0 {
                unsafe { libc::_exit(91) }
            }
            unsafe {
                core::arch::asm!(
                    "xor ecx, ecx", "xor edx, edx", "wrpkru",
                    "mov eax, {write_nr}", "mov rdx, r9", "syscall", "mov r8, rax", "int3",
                    "xor eax, eax", "xor ecx, ecx", "xor edx, edx", "wrpkru",
                    write_nr = const libc::SYS_write,
                    inout("eax") pkru => _, in("rdi") output.as_raw_fd() as usize,
                    in("rsi") address, in("r9") length,
                    out("r8") _, out("rcx") _, out("rdx") _, out("r11") _,
                    options(nostack),
                );
                libc::_exit(0);
            }
        }
        ForkResult::Parent { child } => {
            let mut owner = Child {
                pid: child,
                reaped: false,
            };
            assert_eq!(owner.event(), WaitStatus::Stopped(child, Signal::SIGTRAP));
            let memory = Stopped::new_unchecked(child.into());
            let actual = memory.getregs().unwrap().r8 as i64;
            let state = memory.getxstate().unwrap();
            assert_eq!(decode_native_pkru_xstate(&state.0, layout), Ok(pkru));
            check(&memory, child.as_raw(), address, actual);
            let mut bytes = [0xa5u8; MAX_READ];
            let observed = Errno::result(unsafe {
                libc::read(input.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
            });
            if actual >= 0 {
                assert_eq!(actual as usize, length);
                assert_eq!(observed, Ok(length as isize));
                assert_eq!(&bytes[..length], &vec![0x3c; length]);
                assert!(bytes[length..].iter().all(|b| *b == 0xa5));
            } else {
                assert_eq!(actual, -(libc::EFAULT as i64));
                assert_eq!(observed, Err(Errno::EAGAIN));
                assert_eq!(bytes, [0xa5; MAX_READ]);
            }
            // Like with_actual_stopped_pkru: restore key0 only AFTER recording
            // all outcomes, for safe return-to-user teardown and exact reap.
            let mut state = memory.getxstate().unwrap();
            state.0[layout.offset..layout.offset + 4].copy_from_slice(&(pkru & !3).to_le_bytes());
            let features = u64::from_le_bytes(state.0[512..520].try_into().unwrap());
            state.0[512..520].copy_from_slice(&(features | (1 << 9)).to_le_bytes());
            memory.setxstate(&state).unwrap();
            ptrace::cont(child, None).unwrap();
            assert_eq!(owner.event(), WaitStatus::Exited(child, 0));
            owner.reaped = true;
        }
    }
}

#[test]
fn native_read_actual_stopped_readable_one_through_eight_and_512() {
    for length in (1..=8).chain([512]) {
        native_child(
            libc::PROT_READ | libc::PROT_WRITE,
            0,
            0,
            length,
            |memory, tid, address, native| {
                let mut canary = [0xa5; MAX_READ + 2];
                assert_eq!(
                    memory.read_native_user_exact(tid, address, &mut canary[1..1 + length]),
                    Ok(())
                );
                assert_eq!(native, length as i64);
                assert!(canary[1..1 + length].iter().all(|b| *b == 0x3c));
                assert_eq!(canary[0], 0xa5);
                assert!(canary[1 + length..].iter().all(|b| *b == 0xa5));
            },
        );
    }
}

#[test]
fn native_read_actual_prot_none_refuses_without_copy_and_old_peek_succeeds() {
    for length in (1..=8).chain([512]) {
        native_child(
            libc::PROT_NONE,
            0,
            0,
            length,
            |memory, tid, address, native| {
                let mut canary = [0xa5; MAX_READ + 2];
                assert_eq!(native, -(libc::EFAULT as i64));
                assert_eq!(
                    memory.read_native_user_exact(tid, address, &mut canary[1..1 + length]),
                    Err(Error::Fault(Fault::NoAccessMapping))
                );
                assert_eq!(canary, [0xa5; MAX_READ + 2]);
                let mut legacy = [0; 8];
                let short = length.min(8);
                assert_eq!(
                    memory.read(Addr::from_raw(address).unwrap(), &mut legacy[..short]),
                    Ok(short)
                );
                assert!(
                    legacy[..short].iter().all(|b| *b == 0x3c),
                    "historical ptrace bypass premise"
                );
            },
        );
    }
}

#[test]
fn native_read_actual_wrong_tid_bounds_and_untraced_refuse_without_copy() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, _| {
            for wrong in [0, -1, unsafe { libc::syscall(libc::SYS_gettid) } as i32] {
                let mut canary = [0xa5; 10];
                assert_eq!(
                    memory.read_native_user_exact(wrong, address, &mut canary[1..9]),
                    Err(refused(Refusal::WrongTask))
                );
                assert_eq!(canary, [0xa5; 10]);
            }
            for (source, length) in [
                (address, 0),
                (address, 513),
                (usize::MAX, 8),
                (address + PAGE - 1, 2),
            ] {
                let mut canary = [0xa5; MAX_READ + 3];
                assert_eq!(
                    memory.read_native_user_exact(tid, source, &mut canary[1..1 + length]),
                    Err(refused(Refusal::UnsupportedRange))
                );
                assert_eq!(canary, [0xa5; MAX_READ + 3]);
            }
        },
    );
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
    let memory = Stopped::new_unchecked(reverie_process::Pid::from_raw(tid));
    let mut canary = [0xa5; 10];
    assert!(matches!(
        memory.read_native_user_exact(tid, 0x1000, &mut canary[1..9]),
        Err(Error::Refused(Refusal::TargetState(_)))
    ));
    assert_eq!(canary, [0xa5; 10]);
}

fn pkru_case(key: i32, pkru: u32, length: usize) {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        key,
        pkru,
        length,
        |memory, tid, address, native| {
            let mut canary = [0xa5; MAX_READ + 2];
            let result = memory.read_native_user_exact(tid, address, &mut canary[1..1 + length]);
            if pkru & (1u32 << (2 * key)) == 0 {
                assert_eq!(native, length as i64, "WD alone must not deny source reads");
                assert_eq!(result, Ok(()));
                assert!(canary[1..1 + length].iter().all(|b| *b == 0x3c));
                assert_eq!(canary[0], 0xa5);
                assert!(canary[1 + length..].iter().all(|b| *b == 0xa5));
            } else {
                assert_eq!(native, -(libc::EFAULT as i64));
                assert_eq!(result, Err(Error::Fault(Fault::ProtectionKey(key as u8))));
                assert_eq!(canary, [0xa5; MAX_READ + 2]);
                // Numeric remote operands demonstrate actual FOLL_REMOTE bypass.
                // This is deliberately NOT the capability or a success receipt.
                let mut bypass = [0u8; MAX_READ];
                let local = libc::iovec {
                    iov_base: bypass.as_mut_ptr().cast(),
                    iov_len: length,
                };
                let remote = libc::iovec {
                    iov_base: address as *mut libc::c_void,
                    iov_len: length,
                };
                assert_eq!(
                    Errno::result(unsafe { libc::process_vm_readv(tid, &local, 1, &remote, 1, 0) }),
                    Ok(length as isize)
                );
                assert!(bypass[..length].iter().all(|b| *b == 0x3c));
            }
        },
    );
}

#[test]
fn native_read_actual_key0_wrpkru_ad_wd_matches_native_pipe_write() {
    for bits in 0..4 {
        for length in [8, 512] {
            pkru_case(0, bits, length);
        }
    }
}

#[test]
fn native_read_actual_nonzero_key_wrpkru_and_unrelated_key0_match_native_pipe_write() {
    struct Key(i32);
    impl Drop for Key {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::syscall(libc::SYS_pkey_free, self.0) }, 0);
        }
    }
    let key = unsafe { libc::syscall(libc::SYS_pkey_alloc, 0, 0) };
    assert!(
        (1..16).contains(&key),
        "actual nonzero pkey required; no skip"
    );
    let key = Key(key as i32);
    for bits in 0..4 {
        pkru_case(key.0, bits << (2 * key.0), 512);
    }
    // Key0 AD is unrelated to this buffer: no accidental key0-only predicate.
    pkru_case(key.0, 1, 8);
    pkru_case(key.0, 1 | (2 << (2 * key.0)), 8);
}

#[test]
fn native_read_actual_write_only_and_execute_only_are_refusals_not_guest_efault() {
    for protection in [libc::PROT_WRITE, libc::PROT_EXEC] {
        native_child(protection, 0, 0, 8, |memory, tid, address, native| {
            assert_eq!(
                native, 8,
                "x86 native data read with PKRU allowing its actual key"
            );
            let mut canary = [0xa5; 10];
            assert_eq!(
                memory.read_native_user_exact(tid, address, &mut canary[1..9]),
                Err(refused(Refusal::UnsupportedMapping))
            );
            assert_eq!(canary, [0xa5; 10]);
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backing {
    PrivateAnonymous,
    SharedAnonymous,
    SharedFile,
    PrivateFileBeforeCow,
}

fn backing_case(backing: Backing, length: usize) {
    // Real mmap file backing, not a JSON/smaps specimen. A memfd gives the
    // held descriptor and VMA the same actual backing inode even when /tmp is
    // overlayfs (where fstat's overlay inode and vm_file's inode can differ).
    // This remains a file-backed VMA, including before private COW; no pathname
    // or anonymous-looking name is used as an admission premise.
    let file = matches!(backing, Backing::SharedFile | Backing::PrivateFileBeforeCow).then(|| {
        let fd = Errno::result(unsafe {
            libc::memfd_create(c"native-source-file-backing".as_ptr(), libc::MFD_CLOEXEC)
        })
        .expect("actual memory-backed file; no skip");
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(PAGE as u64).unwrap();
        file.write_all_at(&[0x3cu8; PAGE], 0).unwrap();
        file
    });
    let flags = match backing {
        Backing::PrivateAnonymous => libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        Backing::SharedAnonymous => libc::MAP_SHARED | libc::MAP_ANONYMOUS,
        Backing::SharedFile => libc::MAP_SHARED,
        Backing::PrivateFileBeforeCow => libc::MAP_PRIVATE,
    };
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            PAGE,
            libc::PROT_READ | libc::PROT_WRITE,
            flags,
            file.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            0,
        )
    };
    assert_ne!(raw, libc::MAP_FAILED);
    let page = Page(raw);
    if file.is_none() {
        unsafe { std::ptr::write_bytes(raw.cast::<u8>(), 0x3c, PAGE) };
    }
    // Never write to a private file mapping: its first native pipe read and
    // following stopped observation must still be before any target COW.
    native_child_page(
        page,
        libc::PROT_READ,
        0,
        0,
        length,
        |memory, tid, address, native| {
            assert_eq!(native, length as i64);
            assert_eq!(address, raw as usize);
            let bytes = std::fs::read(format!("/proc/{tid}/smaps")).unwrap();
            let map = mapping(&bytes, address, address + length).unwrap();
            assert_eq!(
                map.permissions[3],
                if matches!(backing, Backing::SharedFile | Backing::SharedAnonymous) {
                    b's'
                } else {
                    b'p'
                }
            );
            if let Some(file) = &file {
                let identity = file.metadata().unwrap();
                assert_eq!(
                    map.device,
                    (
                        libc::major(identity.dev()) as usize,
                        libc::minor(identity.dev()) as usize
                    )
                );
                assert_eq!(map.inode, identity.ino() as usize);
                assert_ne!(map.inode, 0);
            } else if backing == Backing::PrivateAnonymous {
                assert_eq!((map.offset, map.device, map.inode), (0, (0, 0), 0));
            }
            for written in [0x6du8, 0x7e] {
                // The independent parent writes only its own anonymous mapping or
                // the backing file, never the stopped child's address space.
                if let Some(file) = &file {
                    file.write_all_at(&[written; PAGE], 0).unwrap();
                } else {
                    unsafe { std::ptr::write_bytes(raw.cast::<u8>(), written, PAGE) };
                }
                let expected = if backing == Backing::PrivateAnonymous {
                    0x3c
                } else {
                    written
                };
                // Independent numeric-operand observation demonstrates both the
                // changed source and that ordinary process_vm access would succeed.
                // It is not the capability under test or an exclusion certificate.
                let mut observed = [0xa5u8; MAX_READ + 2];
                let local = libc::iovec {
                    iov_base: observed[1..].as_mut_ptr().cast(),
                    iov_len: length,
                };
                let remote = libc::iovec {
                    iov_base: address as *mut libc::c_void,
                    iov_len: length,
                };
                assert_eq!(
                    Errno::result(unsafe { libc::process_vm_readv(tid, &local, 1, &remote, 1, 0) }),
                    Ok(length as isize)
                );
                assert!(observed[1..1 + length].iter().all(|b| *b == expected));
                assert_eq!(observed[0], 0xa5);
                assert!(observed[1 + length..].iter().all(|b| *b == 0xa5));

                NATIVE_TRANSFERS.with(|count| count.set(0));
                let mut canary = [0xa5u8; MAX_READ + 2];
                let result =
                    memory.read_native_user_exact(tid, address, &mut canary[1..1 + length]);
                if backing == Backing::PrivateAnonymous {
                    assert_eq!(result, Ok(()));
                    assert_eq!(NATIVE_TRANSFERS.with(|count| count.get()), 1);
                    assert!(canary[1..1 + length].iter().all(|b| *b == 0x3c));
                    assert_eq!(canary[0], 0xa5);
                    assert!(canary[1 + length..].iter().all(|b| *b == 0xa5));
                } else {
                    assert_eq!(
                        result,
                        Err(refused(Refusal::UnsupportedBacking)),
                        "backing gate must refuse actual externally writable {backing:?}"
                    );
                    assert_eq!(
                        NATIVE_TRANSFERS.with(|count| count.get()),
                        0,
                        "backing refusal must precede the native source transfer"
                    );
                    assert_eq!(canary, [0xa5; MAX_READ + 2]);
                }
            }
        },
    );
}

#[test]
fn native_read_actual_private_anonymous_excludes_independent_writer() {
    for length in [8, 512] {
        backing_case(Backing::PrivateAnonymous, length);
    }
}

#[test]
fn native_read_actual_shared_anonymous_writer_refuses_without_native_transfer() {
    for length in [8, 512] {
        backing_case(Backing::SharedAnonymous, length);
    }
}

#[test]
fn native_read_actual_shared_file_refuses_without_native_transfer() {
    for length in [8, 512] {
        backing_case(Backing::SharedFile, length);
    }
}

#[test]
fn native_read_actual_private_file_before_cow_refuses_without_native_transfer() {
    for length in [8, 512] {
        backing_case(Backing::PrivateFileBeforeCow, length);
    }
}

#[test]
fn native_read_actual_kernel_vdso_is_not_private_anonymous_authority() {
    // Actual inherited kernel special mapping, whose zero device/inode alone
    // cannot distinguish it from anonymous user backing.
    let address = unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } as usize;
    assert_ne!(address, 0, "actual vDSO mapping required; no skip");
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, _, _| {
            let bytes = std::fs::read(format!("/proc/{tid}/smaps")).unwrap();
            let map = mapping(&bytes, address, address + 8).unwrap();
            assert_eq!((map.offset, map.device, map.inode), (0, (0, 0), 0));
            assert_eq!(map.permissions[0], b'r');
            assert!(map.flags.as_ref().unwrap().contains("de"));
            NATIVE_TRANSFERS.with(|count| count.set(0));
            let mut canary = [0xa5u8; 10];
            assert_eq!(
                memory.read_native_user_exact(tid, address, &mut canary[1..9]),
                Err(refused(Refusal::UnsupportedBacking)),
            );
            assert_eq!(NATIVE_TRANSFERS.with(|count| count.get()), 0);
            assert_eq!(canary, [0xa5; 10]);
        },
    );
}

// These transport premises exercise the production length guard independently
// of any kernel observation. The native tests below obtain real PRSTATUS replies.
#[test]
fn native_read_register_shape_short_with_valid_cs_refuses_before_decode() {
    let mut calls = 0;
    let result = checked_native_mode(|bytes| {
        calls += 1;
        assert_eq!(*bytes, [0; PRSTATUS_BYTES], "initialized bounded storage");
        bytes[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x33u64.to_ne_bytes());
        // CS is entirely present, but the final native register byte is not.
        // Bypassing only the exact-length guard must incorrectly return Ok.
        assert!(PRSTATUS_CS + 8 < PRSTATUS_BYTES);
        Ok(PRSTATUS_BYTES - 1)
    });
    assert_eq!(calls, 1);
    assert_eq!(
        result,
        Err(refused(Refusal::RegisterShape(PRSTATUS_BYTES - 1)))
    );
    assert_eq!(
        checked_native_mode(|bytes| {
            bytes[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x33u64.to_ne_bytes());
            Ok(PRSTATUS_BYTES)
        }),
        Ok(())
    );
}

#[test]
fn native_read_register_shape_transport_rejects_other_shapes_modes_and_errors() {
    for returned in [0, 1, 68, PRSTATUS_CS + 7, PRSTATUS_BYTES + 1, usize::MAX] {
        let mut calls = 0;
        assert_eq!(
            checked_native_mode(|bytes| {
                calls += 1;
                assert_eq!(*bytes, [0; PRSTATUS_BYTES]);
                // Deliberately plausible bytes cannot override the reply shape.
                bytes[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x33u64.to_ne_bytes());
                Ok(returned)
            }),
            Err(refused(Refusal::RegisterShape(returned))),
        );
        assert_eq!(calls, 1);
    }
    for cs in [0u64, 0x23, 0x1_0000_0033, u64::MAX] {
        assert_eq!(
            checked_native_mode(|bytes| {
                bytes[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&cs.to_ne_bytes());
                Ok(PRSTATUS_BYTES)
            }),
            Err(refused(Refusal::UnsupportedPlatform)),
        );
    }
    for error in [Errno::ESRCH, Errno::EPERM, Errno::EIO, Errno::EINTR] {
        let mut calls = 0;
        assert_eq!(
            checked_native_mode(|bytes| {
                calls += 1;
                bytes[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x33u64.to_ne_bytes());
                Err(error)
            }),
            Err(refused(Refusal::TargetState(error))),
        );
        assert_eq!(calls, 1);
    }
}

// Independent raw observation: this deliberately does not call the checked
// reader, Stopped::getregs, or any decoder that assumes a native reply size.
fn raw_prstatus(tid: i32) -> Result<([u8; PRSTATUS_BYTES], usize), Errno> {
    let mut bytes = [0xa5u8; PRSTATUS_BYTES];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    Errno::result(unsafe {
        libc::ptrace(
            libc::PTRACE_GETREGSET,
            tid,
            libc::NT_PRSTATUS as usize as *mut libc::c_void,
            &mut iov as *mut libc::iovec,
        )
    })?;
    Ok((bytes, iov.iov_len))
}

fn set_native_prstatus(tid: i32, bytes: &[u8; PRSTATUS_BYTES]) -> Result<(), Errno> {
    // On x86-64, PTRACE_SETREGS uses the native tracer layout even when the
    // stopped target's current CS selects compat GETREGSET. This permits exact
    // restoration without executing a single instruction in compat mode.
    Errno::result(unsafe {
        libc::ptrace(
            libc::PTRACE_SETREGS,
            tid,
            std::ptr::null_mut::<libc::c_void>(),
            bytes.as_ptr(),
        )
    })
    .map(|_| ())
}

#[test]
fn native_read_register_shape_actual_native64_preserves_exact_source_and_canaries() {
    for length in [8, 512] {
        native_child(
            libc::PROT_READ | libc::PROT_WRITE,
            0,
            0,
            length,
            |memory, tid, address, native| {
                let (bytes, returned) = raw_prstatus(tid).unwrap();
                assert_eq!(returned, PRSTATUS_BYTES);
                assert_eq!(returned, 216, "actual native x86-64 PRSTATUS");
                assert_eq!(
                    u64::from_ne_bytes(bytes[PRSTATUS_CS..PRSTATUS_CS + 8].try_into().unwrap()),
                    0x33
                );
                PROC_OBSERVATIONS.with(|count| count.set(0));
                NATIVE_TRANSFERS.with(|count| count.set(0));
                let mut canary = [0xa5u8; MAX_READ + 2];
                assert_eq!(
                    memory.read_native_user_exact(tid, address, &mut canary[1..1 + length]),
                    Ok(())
                );
                assert_eq!(native, length as i64);
                assert_eq!(PROC_OBSERVATIONS.with(|count| count.get()), 1);
                assert_eq!(NATIVE_TRANSFERS.with(|count| count.get()), 1);
                assert!(canary[1..1 + length].iter().all(|b| *b == 0x3c));
                assert_eq!(canary[0], 0xa5);
                assert!(canary[1 + length..].iter().all(|b| *b == 0xa5));
            },
        );
    }
}

#[test]
fn native_read_register_shape_actual_compat68_refuses_before_proc_or_source() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, native| {
            let (original, returned) = raw_prstatus(tid).unwrap();
            assert_eq!(returned, PRSTATUS_BYTES);
            assert_eq!(
                returned, 216,
                "save a complete actual native register reply"
            );
            assert_eq!(
                u64::from_ne_bytes(original[PRSTATUS_CS..PRSTATUS_CS + 8].try_into().unwrap()),
                0x33
            );
            let mut compat = original;
            compat[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x23u64.to_ne_bytes());
            set_native_prstatus(tid, &compat)
                .expect("kernel must accept actual compat CS; no skip");

            // No Stopped::getregs or generic typed regset access occurs between
            // this mutation and restoration. If any assertion panics, the
            // enclosing Child guard kills/reaps this exact unreleased child;
            // it never resumes compat user instructions.
            let (compat_bytes, compat_length) = raw_prstatus(tid).unwrap();
            assert_eq!(
                compat_length, 68,
                "actual kernel compat PRSTATUS, not a mock"
            );
            assert_eq!(
                u16::from_ne_bytes(compat_bytes[52..54].try_into().unwrap()),
                0x23
            );
            assert!(
                compat_bytes[compat_length..].iter().all(|b| *b == 0xa5),
                "kernel did not fill the native-layout tail"
            );
            PROC_OBSERVATIONS.with(|count| count.set(0));
            NATIVE_TRANSFERS.with(|count| count.set(0));
            let mut canary = [0xa5u8; 10];
            let result = memory.read_native_user_exact(tid, address, &mut canary[1..9]);
            let proc_observations = PROC_OBSERVATIONS.with(|count| count.get());
            let native_transfers = NATIVE_TRANSFERS.with(|count| count.get());

            // Restore before evaluating the capability assertions or returning
            // to the existing fixture, whose original native pipe/PKRU/reap
            // checks must still execute. A restore failure is a test failure.
            set_native_prstatus(tid, &original).expect("restore actual native registers");
            let (restored, restored_length) = raw_prstatus(tid).unwrap();
            assert_eq!(restored_length, PRSTATUS_BYTES);
            assert_eq!(restored, original, "exact full register restoration");
            assert_eq!(result, Err(refused(Refusal::RegisterShape(68))));
            assert_eq!(proc_observations, 0, "refuse before the reader opens proc");
            assert_eq!(native_transfers, 0, "refuse before native source transfer");
            assert_eq!(canary, [0xa5; 10]);
            assert_eq!(
                native, 8,
                "original supported native pipe read still succeeded"
            );
        },
    );
}
