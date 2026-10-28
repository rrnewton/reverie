/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::descriptor_status` on a KVM guest is the executor's answer for
//! the descriptor table the guest's syscalls use: the host kernel's `fdinfo`
//! for the host description behind a guest descriptor. A guest record lock
//! is held, and reported, as the open file description lock it is on the
//! host; the guest's own `flock(2)` is refused, so a flock lock is reported
//! only on a description the guest inherited locked. Descriptors the
//! executor keeps itself (a signalfd, captured standard output) are not
//! provided, and a descriptor the guest does not hold is `EBADF`.

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_ioctls::Kvm;
use reverie::DescriptorStatus;
use reverie::Errno;
use reverie::Error;
use reverie::FactError;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::LockClass;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_kvm::KvmBackend;

/// The first argument of the guest's `tuxcall` that asks for a fact
/// (`ASK` in the fixture).
const ASK: u64 = 0x6664_696e_666f;

// The fixture's numbered points.
const EVENTFD_WRITTEN: u64 = 1;
const EVENTFD_READ: u64 = 2;
const SOCKET_SENDER: u64 = 3;
const SOCKET_RECEIVER: u64 = 4;
const SOCKET_RECEIVED: u64 = 5;
const RECORD_LOCKS: u64 = 6;
const FLOCK_HELD: u64 = 7;
const INHERITED: u64 = 8;
const SIGNALFD: u64 = 9;
const CAPTURED: u64 = 10;
const CLOSED: u64 = 11;
const NEGATIVE: u64 = 12;

type Fact = Result<DescriptorStatus, FactError>;

/// The facts the Tool read, by the guest's point number.
#[derive(Default)]
struct FactLog(Mutex<BTreeMap<u64, (i32, Fact)>>);

#[reverie::global_tool]
impl GlobalTool for FactLog {
    type Request = (u64, i32, Fact);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, request: Self::Request) {
        let (point, fd, fact) = request;
        let previous = self.0.lock().unwrap().insert(point, (fd, fact));
        assert!(previous.is_none(), "point {point} asked twice");
    }
}

#[derive(Default)]
struct FactTool;

#[reverie::tool]
impl Tool for FactTool {
    type GlobalState = FactLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::tuxcall].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = syscall.into_parts();
        if args.arg0 as u64 != ASK {
            return Ok(guest.inject(syscall).await?);
        }
        let (fd, point) = (args.arg1 as i32, args.arg2 as u64);
        let fact = guest.descriptor_status(fd);
        guest.send_rpc((point, fd, fact)).await;
        Ok(0)
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-kvm-descriptor-status-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// The device Linux prints for files under `path`'s mount: its superblock's,
/// which `/proc/self/mountinfo` lists, and which on btrfs is not `st_dev`.
fn superblock_device(path: &Path) -> u64 {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: statx is plain data; an all-zero value is valid.
    let mut status: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: name is terminated and status is writable.
    let result = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            name.as_ptr(),
            0,
            libc::STATX_MNT_ID,
            &mut status,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    assert_ne!(status.stx_mask & libc::STATX_MNT_ID, 0);
    let mount = status.stx_mnt_id.to_string();
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    let line = mountinfo
        .lines()
        .find(|line| line.split(' ').next() == Some(mount.as_str()))
        .unwrap();
    let (major, minor) = line.split(' ').nth(2).unwrap().split_once(':').unwrap();
    libc::makedev(major.parse().unwrap(), minor.parse().unwrap())
}

fn status(facts: &BTreeMap<u64, (i32, Fact)>, point: u64) -> &DescriptorStatus {
    match &facts[&point].1 {
        Ok(status) => status,
        Err(error) => panic!("point {point}: {error:?}"),
    }
}

/// Checks that `lock` names `path`'s file and is not mandatory.
fn names(lock: &reverie::HeldLockFact, path: &Path) {
    assert_eq!(lock.raw_inode, std::fs::metadata(path).unwrap().ino());
    assert_eq!(lock.raw_device, superblock_device(path));
    assert!(!lock.mandatory);
}

#[test]
fn a_kvm_guest_reports_descriptor_status_from_its_executor() {
    Kvm::new().expect("descriptor status test requires usable /dev/kvm");
    let directory = Directory::new();
    let source = directory.0.join("descriptor_status.c");
    let executable = directory.0.join("descriptor_status");
    std::fs::write(&source, include_str!("fixtures/descriptor_status.c")).unwrap();
    let compiled = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "gcc: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    // The guest's standard input is a description the launcher holds a
    // shared flock lock on.
    let input_path = directory.0.join("input");
    std::fs::write(&input_path, b"input").unwrap();
    let input = std::fs::File::open(&input_path).unwrap();
    // SAFETY: flock takes no pointers; the descriptor is open.
    assert_eq!(unsafe { libc::flock(input.as_raw_fd(), libc::LOCK_SH) }, 0);

    let mut backend =
        KvmBackend::new_with_stdin(256 * 1024 * 1024, Some(input.try_clone().unwrap())).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap()],
            &[],
            &directory.0,
        )
        .unwrap();
    let (log, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<FactTool>((), true))
            .unwrap();
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    // The guest's flock(2) is refused with ENOSYS.
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        format!("flock {}\ndone\n", libc::ENOSYS)
    );
    let facts = log.0.into_inner().unwrap();
    assert_eq!(facts.len(), 12, "{facts:?}");

    let written = status(&facts, EVENTFD_WRITTEN);
    assert_eq!(written.eventfd_count, Some(7));
    assert_eq!(written.scm_fds, None);
    assert!(written.locks.is_empty());
    assert_eq!(status(&facts, EVENTFD_READ).eventfd_count, Some(0));

    assert_eq!(status(&facts, SOCKET_SENDER).scm_fds, Some(0));
    let receiver = status(&facts, SOCKET_RECEIVER);
    assert_eq!(receiver.scm_fds, Some(2));
    assert_eq!(receiver.eventfd_count, None);
    assert_eq!(status(&facts, SOCKET_RECEIVED).scm_fds, Some(0));

    // F_SETLK is held as an OFD lock on the host, so both record locks are
    // OFD locks of one description, listed by start.
    let locked = directory.0.join("locked");
    let records = &status(&facts, RECORD_LOCKS).locks;
    assert_eq!(records.len(), 2, "{records:?}");
    for lock in records {
        assert_eq!(lock.class, LockClass::OpenFileDescription);
        names(lock, &locked);
    }
    assert!(records[0].write);
    assert_eq!((records[0].start, records[0].end), (0, Some(9)));
    assert!(!records[1].write);
    assert_eq!((records[1].start, records[1].end), (100, None));

    // The refused flock left no lock.
    assert!(status(&facts, FLOCK_HELD).locks.is_empty());

    let inherited = &status(&facts, INHERITED).locks;
    assert_eq!(inherited.len(), 1, "{inherited:?}");
    assert_eq!(inherited[0].class, LockClass::Flock);
    assert!(!inherited[0].write);
    assert_eq!((inherited[0].start, inherited[0].end), (0, None));
    names(&inherited[0], &input_path);

    for point in [SIGNALFD, CAPTURED] {
        assert_eq!(
            facts[&point].1,
            Err(FactError::NotProvided),
            "point {point}"
        );
    }
    for point in [CLOSED, NEGATIVE] {
        assert_eq!(
            facts[&point].1,
            Err(FactError::Errno(Errno::EBADF)),
            "point {point}"
        );
    }
    drop(input);
}
