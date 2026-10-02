/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// Component controls: the test's original controller holds its exact child at
// a consumed ptrace stop. This fixture does NOT mint a backend SourceAcquisition
// or prove backend completion, caller source exclusion, deadlines or cursors.
use reader::BoundRead;
use reader::RegisterObservation;
use reader::Step;

use super::super::mm_bound as reader;
use super::*;

fn fixture_directory(tid: i32) -> (File, (u64, u64)) {
    // Only the native fixture establishes this original directory. Production
    // must borrow the registered identity; it cannot repeat this numeric open.
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(format!("/proc/{tid}"))
        .unwrap();
    let metadata = directory.metadata().unwrap();
    let identity = (metadata.dev(), metadata.ino());
    (directory, identity)
}

fn worker_read(
    observation: RegisterObservation,
    directory: File,
    identity: (u64, u64),
    length: usize,
) -> (Result<(), Error>, [u8; MAX_READ + 2], Vec<Step>) {
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                reader::hooks::take();
                let mut canary = [0xa5; MAX_READ + 2];
                let result = BoundRead::bind(observation, directory.as_fd(), identity)
                    .and_then(|bound| bound.read_exact(&mut canary[1..1 + length]));
                (result, canary, reader::hooks::take())
            })
            .join()
            .unwrap()
    })
}

fn preads(trace: &[Step]) -> usize {
    assert!(!trace.contains(&Step::Prstatus), "no worker PRSTATUS");
    assert!(!trace.contains(&Step::Xstate), "no worker XSTATE");
    trace.iter().filter(|step| **step == Step::Pread).count()
}

#[test]
fn mm_read_actual_native64_worker_exact_and_canaries() {
    for length in (1..=8).chain([512]) {
        native_child(
            libc::PROT_READ | libc::PROT_WRITE,
            0,
            0,
            length,
            |memory, tid, address, native| {
                let (_, shape) = raw_prstatus(tid).unwrap();
                assert_eq!(shape, 216);
                reader::hooks::take();
                let observation =
                    RegisterObservation::capture(memory, tid, address, length).unwrap();
                assert_eq!(reader::hooks::take(), [Step::Prstatus, Step::Xstate]);
                let (directory, identity) = fixture_directory(tid);
                let (result, canary, trace) = worker_read(observation, directory, identity, length);
                assert_eq!(result, Ok(()));
                assert_eq!(native, length as i64, "independent target pipe read");
                assert_eq!(preads(&trace), 1);
                assert_eq!(canary[0], 0xa5);
                assert!(canary[1..1 + length].iter().all(|b| *b == 0x3c));
                assert!(canary[1 + length..].iter().all(|b| *b == 0xa5));
            },
        );
    }
}

#[test]
fn mm_read_actual_compat68_refuses_before_proc_or_source() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, native| {
            let (original, shape) = raw_prstatus(tid).unwrap();
            assert_eq!(shape, 216);
            let mut compat = original;
            compat[PRSTATUS_CS..PRSTATUS_CS + 8].copy_from_slice(&0x23u64.to_ne_bytes());
            set_native_prstatus(tid, &compat).unwrap();
            let (raw, shape) = raw_prstatus(tid).unwrap();
            assert_eq!(shape, 68, "actual kernel reply; no fabricated compat size");
            assert!(raw[shape..].iter().all(|b| *b == 0xa5));
            reader::hooks::take();
            let result = RegisterObservation::capture(memory, tid, address, 8);
            let trace = reader::hooks::take();
            // No unchecked getregs or user continuation in compat mode. The
            // enclosing exact-child owner kills/reaps if any earlier check fails.
            set_native_prstatus(tid, &original).unwrap();
            assert_eq!(raw_prstatus(tid).unwrap(), (original, PRSTATUS_BYTES));
            assert!(matches!(
                result,
                Err(Error::Refused(Refusal::RegisterShape(68)))
            ));
            assert_eq!(trace, [Step::Prstatus]);
            assert_eq!(native, 8);
        },
    );
}

#[test]
fn mm_read_wrong_task_range_and_original_thread_fail_before_proc() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, _| {
            for expected in [0, -1, unsafe { libc::syscall(libc::SYS_gettid) } as i32] {
                reader::hooks::take();
                assert!(matches!(
                    RegisterObservation::capture(memory, expected, address, 8),
                    Err(Error::Refused(Refusal::WrongTask))
                ));
                assert!(reader::hooks::take().is_empty());
            }
            for (address, length) in [
                (address, 0),
                (address, 513),
                (usize::MAX, 8),
                (address + PAGE - 1, 2),
            ] {
                reader::hooks::take();
                assert!(matches!(
                    RegisterObservation::capture(memory, tid, address, length),
                    Err(Error::Refused(Refusal::UnsupportedRange))
                ));
                assert!(reader::hooks::take().is_empty());
            }
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (directory, identity) = fixture_directory(tid);
            reader::hooks::take();
            assert!(matches!(
                BoundRead::bind(observation, directory.as_fd(), identity),
                Err(Error::Refused(Refusal::TargetState(Errno::EPERM)))
            ));
            assert!(reader::hooks::take().is_empty(), "no foreground proc IO");
            std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        let wrong_thread =
                            Stopped::new_unchecked(reverie_process::Pid::from_raw(tid));
                        assert!(matches!(
                            RegisterObservation::capture(&wrong_thread, tid, address, 8),
                            Err(Error::Refused(Refusal::TargetState(_)))
                        ));
                        assert_eq!(reader::hooks::take(), [Step::Prstatus]);
                    })
                    .join()
                    .unwrap()
            });
        },
    );
}

#[test]
fn mm_read_actual_prot_none_refuses_before_pread() {
    native_child(libc::PROT_NONE, 0, 0, 8, |memory, tid, address, native| {
        assert_eq!(
            native,
            -(libc::EFAULT as i64),
            "actual target pipe access denial"
        );
        let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
        let (directory, identity) = fixture_directory(tid);
        let (result, canary, trace) = worker_read(observation, directory, identity, 8);
        assert_eq!(result, Err(Error::Fault(Fault::NoAccessMapping)));
        assert_eq!(preads(&trace), 0);
        assert_eq!(canary, [0xa5; MAX_READ + 2]);
    });
}

#[test]
fn mm_read_bound_handle_cannot_move_io_back_to_controller() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, _| {
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (directory, identity) = fixture_directory(tid);
            let bound = std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        BoundRead::bind(observation, directory.as_fd(), identity).unwrap()
                    })
                    .join()
                    .unwrap()
            });
            reader::hooks::take();
            let mut canary = [0xa5; 10];
            assert_eq!(
                bound.read_exact(&mut canary[1..9]),
                Err(refused(Refusal::TargetState(Errno::EPERM)))
            );
            assert_eq!(canary, [0xa5; 10]);
            assert!(reader::hooks::take().is_empty());
        },
    );
}

fn mm_pkru_case(key: i32, pkru: u32) {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        key,
        pkru,
        512,
        |memory, tid, address, native| {
            let observation = RegisterObservation::capture(memory, tid, address, 512).unwrap();
            let (directory, identity) = fixture_directory(tid);
            let (result, canary, trace) = worker_read(observation, directory, identity, 512);
            if pkru & (1 << (2 * key)) != 0 {
                assert_eq!(native, -(libc::EFAULT as i64));
                assert_eq!(result, Err(Error::Fault(Fault::ProtectionKey(key as u8))));
                assert_eq!(preads(&trace), 0);
                assert_eq!(canary, [0xa5; MAX_READ + 2]);
            } else {
                assert_eq!(native, 512, "actual native read permits WD-only");
                assert_eq!(result, Ok(()));
                assert_eq!(preads(&trace), 1);
                assert_eq!(canary[0], 0xa5);
                assert_eq!(&canary[1..513], &[0x3c; 512]);
                assert_eq!(canary[513], 0xa5);
            }
        },
    );
}

#[test]
fn mm_read_actual_key0_pkru_ad_vs_wd() {
    for bits in 0..4 {
        mm_pkru_case(0, bits);
    }
}

#[test]
fn mm_read_actual_nonzero_pkru_and_unrelated_key0() {
    struct Key(i32);
    impl Drop for Key {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::syscall(libc::SYS_pkey_free, self.0) }, 0);
        }
    }
    let key = unsafe { libc::syscall(libc::SYS_pkey_alloc, 0, 0) };
    assert!(
        (1..16).contains(&key),
        "real nonzero pkey required; no skip"
    );
    let key = Key(key as i32);
    for bits in 0..4 {
        mm_pkru_case(key.0, bits << (2 * key.0));
    }
    mm_pkru_case(key.0, 1);
    mm_pkru_case(key.0, 1 | (2 << (2 * key.0)));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MmBacking {
    PrivateAnonymous,
    SharedAnonymous,
    SharedFile,
    PrivateBeforeCow,
    PrivateAfterCow,
}

fn mm_backing_case(backing: MmBacking) {
    let file = matches!(
        backing,
        MmBacking::SharedFile | MmBacking::PrivateBeforeCow | MmBacking::PrivateAfterCow
    )
    .then(|| {
        let fd = Errno::result(unsafe {
            libc::memfd_create(c"mm-bound-backing".as_ptr(), libc::MFD_CLOEXEC)
        })
        .unwrap();
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(PAGE as u64).unwrap();
        file.write_all_at(&[0x3cu8; PAGE], 0).unwrap();
        file
    });
    let flags = match backing {
        MmBacking::PrivateAnonymous => libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        MmBacking::SharedAnonymous => libc::MAP_SHARED | libc::MAP_ANONYMOUS,
        MmBacking::SharedFile => libc::MAP_SHARED,
        MmBacking::PrivateBeforeCow | MmBacking::PrivateAfterCow => libc::MAP_PRIVATE,
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
    if file.is_none() || backing == MmBacking::PrivateAfterCow {
        // Actual write faults the file mapping to a private anonymous page.
        // Its VMA still has real vm_file identity and MUST remain unsupported.
        unsafe { std::ptr::write_bytes(raw.cast::<u8>(), 0x3c, PAGE) };
    }
    native_child_page(
        page,
        libc::PROT_READ,
        0,
        0,
        8,
        |memory, tid, address, native| {
            assert_eq!(native, 8);
            let smaps = std::fs::read(format!("/proc/{tid}/smaps")).unwrap();
            let map = mapping(&smaps, address, address + 8).unwrap();
            if let Some(file) = &file {
                let metadata = file.metadata().unwrap();
                assert_eq!(
                    map.device,
                    (
                        libc::major(metadata.dev()) as usize,
                        libc::minor(metadata.dev()) as usize
                    )
                );
                assert_eq!(map.inode, metadata.ino() as usize);
                assert_ne!(map.inode, 0);
            }
            if backing == MmBacking::PrivateAfterCow {
                let text = std::str::from_utf8(&smaps).unwrap();
                let start = text
                    .find(&format!("{:x}-{:x} ", map.start, map.end))
                    .unwrap();
                let record = text[start..].lines().skip(1).take_while(|line| {
                    !line.split_ascii_whitespace().next().unwrap().contains('-')
                });
                let anonymous = record
                    .filter_map(|line| line.strip_prefix("Anonymous:"))
                    .map(|value| page_field(value).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(anonymous.len(), 1);
                assert!(
                    anonymous[0] >= PAGE,
                    "real post-COW anonymous residency, still file VMA"
                );
            }
            for written in [0x6du8, 0x7e] {
                if let Some(file) = &file {
                    file.write_all_at(&[written; PAGE], 0).unwrap();
                } else {
                    unsafe { std::ptr::write_bytes(raw.cast::<u8>(), written, PAGE) };
                }
                let expected = if matches!(
                    backing,
                    MmBacking::PrivateAnonymous | MmBacking::PrivateAfterCow
                ) {
                    0x3c
                } else {
                    written
                };
                // Independent observer, not the transport and not a production fallback.
                let mut bytes = [0xa5u8; 10];
                let local = libc::iovec {
                    iov_base: bytes[1..].as_mut_ptr().cast(),
                    iov_len: 8,
                };
                let remote = libc::iovec {
                    iov_base: address as *mut libc::c_void,
                    iov_len: 8,
                };
                assert_eq!(
                    Errno::result(unsafe { libc::process_vm_readv(tid, &local, 1, &remote, 1, 0) }),
                    Ok(8)
                );
                assert_eq!(&bytes[1..9], &[expected; 8]);
                assert_eq!((bytes[0], bytes[9]), (0xa5, 0xa5));
                let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
                let (directory, identity) = fixture_directory(tid);
                let (result, canary, trace) = worker_read(observation, directory, identity, 8);
                if backing == MmBacking::PrivateAnonymous {
                    assert_eq!(result, Ok(()));
                    assert_eq!(preads(&trace), 1);
                    assert_eq!(&canary[1..9], &[0x3c; 8]);
                    assert_eq!(canary[0], 0xa5);
                    assert!(canary[9..].iter().all(|byte| *byte == 0xa5));
                } else {
                    assert_eq!(
                        result,
                        Err(refused(Refusal::UnsupportedBacking)),
                        "{backing:?}"
                    );
                    assert_eq!(preads(&trace), 0);
                    assert_eq!(canary, [0xa5; MAX_READ + 2]);
                }
            }
        },
    );
}

#[test]
fn mm_read_actual_private_anonymous_excludes_independent_writer() {
    mm_backing_case(MmBacking::PrivateAnonymous);
}
#[test]
fn mm_read_actual_shared_anonymous_refuses_independent_writer() {
    mm_backing_case(MmBacking::SharedAnonymous);
}
#[test]
fn mm_read_actual_shared_file_refuses_independent_writer() {
    mm_backing_case(MmBacking::SharedFile);
}
#[test]
fn mm_read_actual_private_file_before_cow_refuses_independent_writer() {
    mm_backing_case(MmBacking::PrivateBeforeCow);
}
#[test]
fn mm_read_actual_private_file_after_cow_still_refuses_backing() {
    mm_backing_case(MmBacking::PrivateAfterCow);
}

#[test]
fn mm_read_actual_vdso_refuses_before_pread() {
    let vdso = unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } as usize;
    assert_ne!(vdso, 0);
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, _, _| {
            let observation = RegisterObservation::capture(memory, tid, vdso, 8).unwrap();
            let (directory, identity) = fixture_directory(tid);
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert_eq!(result, Err(refused(Refusal::UnsupportedBacking)));
            assert_eq!(preads(&trace), 0);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);
        },
    );
}

#[test]
fn mm_read_actual_foreign_directory_root_and_identity_refuse() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, _| {
            let (directory, mut identity) = fixture_directory(tid);
            identity.1 ^= 1;
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert_eq!(result, Err(refused(Refusal::ProcfsViewMismatch)));
            assert_eq!(trace, [Step::Acquisition]);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);

            let foreign_tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            let (directory, identity) = fixture_directory(foreign_tid);
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert_eq!(result, Err(refused(Refusal::ProcfsViewMismatch)));
            assert_eq!(preads(&trace), 0);
            assert!(!trace.contains(&Step::MemBound));
            assert_eq!(canary, [0xa5; MAX_READ + 2]);

            let directory = File::open("/dev").unwrap();
            let metadata = directory.metadata().unwrap();
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (result, canary, trace) =
                worker_read(observation, directory, (metadata.dev(), metadata.ino()), 8);
            assert_eq!(result, Err(refused(Refusal::ProcfsViewMismatch)));
            assert_eq!(preads(&trace), 0);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);
        },
    );
}

#[test]
fn mm_read_actual_proc_descriptor_mount_expectation_mismatch_refuses() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |_, tid, _, _| {
            let (directory, _) = fixture_directory(tid);
            std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        let root = File::open("/proc").unwrap();
                        let mount = verify_proc(&root, None).unwrap();
                        assert_eq!(verify_proc(&directory, Some(mount)), Ok(mount));
                        // Actual descriptor/statx observation with corrupted expectations.
                        // This does not claim that a replacement mount was performed.
                        for expected in [
                            ProcMount {
                                device: mount.device ^ 1,
                                ..mount
                            },
                            ProcMount {
                                mount_id: mount.mount_id ^ 1,
                                ..mount
                            },
                        ] {
                            assert_eq!(
                                verify_proc(&directory, Some(expected)),
                                Err(refused(Refusal::ProcfsViewMismatch))
                            );
                        }
                    })
                    .join()
                    .unwrap()
            });
        },
    );
}

#[test]
fn mm_read_acquisition_and_read_delay_have_no_worker_ptrace() {
    for pause in [Step::Acquisition, Step::MemBound, Step::BeforeRead] {
        native_child(
            libc::PROT_READ | libc::PROT_WRITE,
            0,
            0,
            8,
            |memory, tid, address, _| {
                let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
                let (directory, identity) = fixture_directory(tid);
                reader::hooks::take();
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let (resume_tx, resume_rx) = std::sync::mpsc::channel();
                std::thread::scope(|scope| {
                    let worker = scope.spawn(move || {
                        reader::hooks::set(move |step| {
                            if step == pause {
                                ready_tx
                                    .send(unsafe { libc::syscall(libc::SYS_gettid) })
                                    .unwrap();
                                resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                            }
                        });
                        let mut canary = [0xa5; 10];
                        let result = BoundRead::bind(observation, directory.as_fd(), identity)
                            .and_then(|bound| bound.read_exact(&mut canary[1..9]));
                        (result, canary, reader::hooks::take())
                    });
                    let worker_tid = ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    assert_ne!(worker_tid, unsafe { libc::syscall(libc::SYS_gettid) });
                    assert_eq!(
                        memory.getregs().unwrap().cs,
                        0x33,
                        "controller still owns actual held stop"
                    );
                    assert!(
                        reader::hooks::take().is_empty(),
                        "no preparation or IO on paused controller"
                    );
                    resume_tx.send(()).unwrap();
                    let (result, canary, trace) = worker.join().unwrap();
                    assert_eq!(result, Ok(()));
                    assert_eq!(preads(&trace), 1);
                    assert_eq!(
                        canary,
                        [0xa5, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0x3c, 0xa5]
                    );
                });
            },
        );
    }
}

#[test]
fn mm_read_partial_transport_counts_preserve_canaries() {
    // Decoder/transport premises, not a fabricated native partial-read receipt.
    // Real old-MM EOF is exercised separately through BoundRead::read_exact.
    for count in [0, 1, 7, 9] {
        let mut canary = [0xa5; 10];
        assert_eq!(
            publish(Ok(count), &[0x3c; MAX_READ], &mut canary[1..9]),
            Err(refused(Refusal::ShortTransfer(count)))
        );
        assert_eq!(canary, [0xa5; 10]);
    }
}

#[test]
fn mm_read_actual_worker_proc_permission_denial_has_no_fallback() {
    let page = Page::new();
    match unsafe { fork() }.unwrap() {
        ForkResult::Child => {
            if ptrace::traceme().is_err()
                || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
            {
                unsafe { libc::_exit(90) }
            }
            unsafe {
                core::arch::asm!("int3", options(nostack));
                libc::_exit(0)
            }
        }
        ForkResult::Parent { child } => {
            let mut owner = Child {
                pid: child,
                reaped: false,
            };
            assert_eq!(owner.event(), WaitStatus::Stopped(child, Signal::SIGTRAP));
            let memory = Stopped::new_unchecked(child.into());
            // Already-established ptrace register observation still works.
            // Reopening protected proc memory must enforce its own permission.
            let observation =
                RegisterObservation::capture(&memory, child.as_raw(), page.0 as usize, 8).unwrap();
            let (directory, identity) = fixture_directory(child.as_raw());
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert!(
                matches!(
                    result,
                    Err(Error::Refused(Refusal::Procfs(
                        Errno::EACCES | Errno::EPERM
                    )))
                ),
                "requires actual worker-side permission denial; a privileged bypass is not a skip: {result:?}"
            );
            assert_eq!(preads(&trace), 0);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);
            ptrace::cont(child, None).unwrap();
            assert_eq!(owner.event(), WaitStatus::Exited(child, 0));
        }
    }
}

// Small actual x86-64 ET_EXEC fixture; no compiler, multilib dependency, shell
// process, pathname lookup or conditional skip. It maps the SAME numeric source
// address in a NEW mm, writes different bytes to an inherited pipe, then traps.
// This is a fixture program, never production source execution or read authority.
fn replacement_elf(address: usize, output: i32) -> File {
    assert_ne!(address / PAGE, 0x400000 / PAGE);
    let mut code = Vec::new();
    code.extend_from_slice(&[0xb8, 9, 0, 0, 0]); // mov eax, SYS_mmap
    code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, address
    code.extend_from_slice(&(address as u64).to_le_bytes());
    code.extend_from_slice(&[0xbe, 0, 0x10, 0, 0]); // mov esi, 4096
    code.extend_from_slice(&[0xba, 3, 0, 0, 0]); // mov edx, PROT_READ|PROT_WRITE
    code.extend_from_slice(&[0x41, 0xba, 0x32, 0, 0, 0]); // private|anon|fixed
    code.extend_from_slice(&[0x49, 0xc7, 0xc0, 0xff, 0xff, 0xff, 0xff]); // r8=-1
    code.extend_from_slice(&[0x45, 0x31, 0xc9, 0x0f, 0x05]); // xor r9d,r9d; syscall
    code.extend_from_slice(&[0x48, 0xbb]); // movabs rbx, eight replacement bytes
    code.extend_from_slice(&[0x7e; 8]);
    code.extend_from_slice(&[0x48, 0x89, 0x18]); // mov [rax],rbx (failure faults)
    code.extend_from_slice(&[0x48, 0x89, 0xc6]); // mov rsi,rax
    code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xbf]); // SYS_write; mov edi,fd
    code.extend_from_slice(&(output as u32).to_le_bytes());
    code.extend_from_slice(&[0xba, 8, 0, 0, 0, 0x0f, 0x05, 0xcc]); // count=8; syscall; int3
    code.extend_from_slice(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b]);

    let mut elf = vec![0u8; 120];
    elf[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    elf[16..18].copy_from_slice(&2u16.to_le_bytes());
    elf[18..20].copy_from_slice(&62u16.to_le_bytes());
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    elf[52..54].copy_from_slice(&64u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    elf[68..72].copy_from_slice(&5u32.to_le_bytes()); // PF_R|PF_X
    elf[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    let size = (elf.len() + code.len()) as u64;
    elf[96..104].copy_from_slice(&size.to_le_bytes());
    elf[104..112].copy_from_slice(&size.to_le_bytes());
    elf[112..120].copy_from_slice(&(PAGE as u64).to_le_bytes());
    elf.extend_from_slice(&code);
    let fd = Errno::result(unsafe {
        libc::memfd_create(c"mm-reader-replacement-elf".as_ptr(), libc::MFD_CLOEXEC)
    })
    .unwrap();
    let file = unsafe { File::from_raw_fd(fd) };
    file.write_all_at(&elf, 0).unwrap();
    file
}

#[test]
fn mm_read_actual_old_mm_exec_same_address_then_reap_never_reads_replacement() {
    let page = Page::new();
    let address = page.0 as usize;
    let mut pipe = [-1; 2];
    // The fixture's write end deliberately survives its one actual exec.
    assert_eq!(
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK) },
        0
    );
    let input = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let output = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    let elf = replacement_elf(address, output.as_raw_fd());
    let argv = [c"mm-reader-replacement".as_ptr(), std::ptr::null()];
    let envp: [*const libc::c_char; 1] = [std::ptr::null()];
    match unsafe { fork() }.unwrap() {
        ForkResult::Child => {
            if ptrace::traceme().is_err() {
                unsafe { libc::_exit(90) }
            }
            unsafe {
                core::arch::asm!("int3", options(nostack));
                libc::syscall(
                    libc::SYS_execveat,
                    elf.as_raw_fd(),
                    c"".as_ptr(),
                    argv.as_ptr(),
                    envp.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                libc::_exit(92);
            }
        }
        ForkResult::Parent { child } => {
            let mut owner = Child {
                pid: child,
                reaped: false,
            };
            assert_eq!(owner.event(), WaitStatus::Stopped(child, Signal::SIGTRAP));
            let memory = Stopped::new_unchecked(child.into());
            let observations = [
                RegisterObservation::capture(&memory, child.as_raw(), address, 8).unwrap(),
                RegisterObservation::capture(&memory, child.as_raw(), address, 8).unwrap(),
                RegisterObservation::capture(&memory, child.as_raw(), address, 8).unwrap(),
            ];
            let (directory, identity) = fixture_directory(child.as_raw());
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                let worker = scope.spawn(move || {
                    // All three files bind under this test's single HELD stop.
                    // The later resumes deliberately test FD identity only;
                    // they are NOT permission for a production result commit.
                    let bound = observations
                        .into_iter()
                        .map(|observation| {
                            BoundRead::bind(observation, directory.as_fd(), identity).unwrap()
                        })
                        .collect::<Vec<_>>();
                    reader::hooks::take();
                    for (phase, bound) in bound.into_iter().enumerate() {
                        if phase != 0 {
                            resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                        }
                        let mut canary = [0xa5; 10];
                        let result = bound.read_exact(&mut canary[1..9]);
                        result_tx
                            .send((result, canary, reader::hooks::take()))
                            .unwrap();
                    }
                });
                let (result, canary, trace) =
                    result_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!(result, Ok(()));
                assert_eq!(&canary[1..9], &[0x3c; 8], "positive OLD mm read");
                assert_eq!((canary[0], canary[9]), (0xa5, 0xa5));
                assert_eq!(preads(&trace), 1);

                ptrace::cont(child, None).unwrap();
                assert_eq!(
                    owner.event(),
                    WaitStatus::Stopped(child, Signal::SIGTRAP),
                    "actual exec stop"
                );
                let registers = memory.getregs().unwrap();
                assert_eq!(registers.rip, 0x400078, "new ELF entry, new mm");
                ptrace::cont(child, None).unwrap();
                assert_eq!(
                    owner.event(),
                    WaitStatus::Stopped(child, Signal::SIGTRAP),
                    "replacement mapping/code trap"
                );
                assert_eq!(
                    memory.getregs().unwrap().rax,
                    8,
                    "actual replacement pipe write"
                );
                let mut bytes = [0xa5u8; 10];
                assert_eq!(
                    Errno::result(unsafe {
                        libc::read(input.as_raw_fd(), bytes[1..].as_mut_ptr().cast(), 8)
                    }),
                    Ok(8)
                );
                assert_eq!(
                    bytes,
                    [0xa5, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0xa5]
                );
                // Independent oracle makes a fresh-numeric-reopen mutant
                // sensitive: the same address in the replacement really reads.
                let fresh = File::open(format!("/proc/{}/mem", child.as_raw())).unwrap();
                let mut replacement = [0xa5u8; 10];
                assert_eq!(
                    fresh
                        .read_at(&mut replacement[1..9], address as u64)
                        .unwrap(),
                    8
                );
                assert_eq!(replacement, bytes);
                resume_tx.send(()).unwrap();
                let (result, canary, trace) =
                    result_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!(
                    result,
                    Err(refused(Refusal::ShortTransfer(0))),
                    "old mm_users is zero after exec"
                );
                assert_eq!(canary, [0xa5; 10]);
                assert_eq!(preads(&trace), 1);

                assert_eq!(unsafe { libc::kill(child.as_raw(), libc::SIGKILL) }, 0);
                assert_eq!(
                    owner.event(),
                    WaitStatus::Signaled(child, Signal::SIGKILL, false)
                );
                assert!(owner.reaped, "exact child reaped before final old-FD read");
                resume_tx.send(()).unwrap();
                let (result, canary, trace) =
                    result_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!(result, Err(refused(Refusal::ShortTransfer(0))));
                assert_eq!(canary, [0xa5; 10]);
                assert_eq!(preads(&trace), 1);
                worker.join().unwrap();
            });
        }
    }
}

#[test]
fn mm_read_actual_write_only_and_execute_only_refuse_before_pread() {
    for protection in [libc::PROT_WRITE, libc::PROT_EXEC] {
        native_child(protection, 0, 0, 8, |memory, tid, address, native| {
            assert_eq!(native, 8, "actual x86 target read with allowed PKRU");
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (directory, identity) = fixture_directory(tid);
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert_eq!(result, Err(refused(Refusal::UnsupportedMapping)));
            assert_eq!(preads(&trace), 0);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);
        });
    }
}

#[test]
fn mm_read_actual_observed_lazy_free_refuses_before_pread() {
    // A single MADV_FREE page can remain in the kernel's per-CPU lazyfree
    // batch and therefore not yet appear in smaps. Exercise enough pages in
    // ONE call to flush a batch; do not poll, pin a CPU, skip zero debt, or
    // substitute a fabricated smaps entry. The reader still observes 8 bytes.
    const LENGTH: usize = 64 * PAGE;
    struct Mapping(*mut libc::c_void);
    impl Drop for Mapping {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::munmap(self.0, LENGTH) }, 0);
        }
    }
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            LENGTH,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(raw, libc::MAP_FAILED);
    let page = Mapping(raw);
    // Keep the target page at base-page granularity for the actual MADV_FREE
    // observation. This is a component fixture, not discard-history authority.
    assert_eq!(
        unsafe { libc::madvise(page.0, LENGTH, libc::MADV_NOHUGEPAGE) },
        0
    );
    match unsafe { fork() }.unwrap() {
        ForkResult::Child => {
            if ptrace::traceme().is_err() {
                unsafe { libc::_exit(90) }
            }
            unsafe {
                // Fault every base page in the child: the target must own real
                // exclusive anonymous pages, not inherit a shared/zero folio.
                for offset in (0..LENGTH).step_by(PAGE) {
                    std::ptr::write_volatile(page.0.cast::<u8>().add(offset), 0x3c);
                }
                if libc::madvise(page.0, LENGTH, libc::MADV_FREE) != 0 {
                    libc::_exit(91)
                }
                core::arch::asm!("int3", options(nostack));
                libc::_exit(0);
            }
        }
        ForkResult::Parent { child } => {
            let mut owner = Child {
                pid: child,
                reaped: false,
            };
            assert_eq!(owner.event(), WaitStatus::Stopped(child, Signal::SIGTRAP));
            let address = page.0 as usize;
            let bytes = std::fs::read(format!("/proc/{child}/smaps")).unwrap();
            let map = mapping(&bytes, address, address + 8).unwrap();
            assert!(
                map.lazy_free.unwrap() >= PAGE,
                "actual observed LazyFree debt required; no skip"
            );
            assert_eq!((map.offset, map.device, map.inode), (0, (0, 0), 0));
            let memory = Stopped::new_unchecked(child.into());
            let observation =
                RegisterObservation::capture(&memory, child.as_raw(), address, 8).unwrap();
            let (directory, identity) = fixture_directory(child.as_raw());
            let (result, canary, trace) = worker_read(observation, directory, identity, 8);
            assert_eq!(result, Err(refused(Refusal::UnsupportedBacking)));
            assert_eq!(preads(&trace), 0);
            assert_eq!(canary, [0xa5; MAX_READ + 2]);
            ptrace::cont(child, None).unwrap();
            assert_eq!(owner.event(), WaitStatus::Exited(child, 0));
        }
    }
}

#[test]
fn mm_read_bound_output_length_mismatch_has_no_pread_or_publication() {
    native_child(
        libc::PROT_READ | libc::PROT_WRITE,
        0,
        0,
        8,
        |memory, tid, address, _| {
            let observation = RegisterObservation::capture(memory, tid, address, 8).unwrap();
            let (directory, identity) = fixture_directory(tid);
            std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        let bound =
                            BoundRead::bind(observation, directory.as_fd(), identity).unwrap();
                        reader::hooks::take();
                        let mut canary = [0xa5; 10];
                        assert_eq!(
                            bound.read_exact(&mut canary[1..8]),
                            Err(refused(Refusal::UnsupportedRange))
                        );
                        assert_eq!(canary, [0xa5; 10]);
                        assert!(reader::hooks::take().is_empty());
                    })
                    .join()
                    .unwrap()
            });
        },
    );
}

#[test]
fn mm_read_metadata_fd_bounds_and_malformed_premises_refuse() {
    // Real file I/O with parser specimens. These are NOT genuine smaps files,
    // acquired MM handles or evidence of an actual target's mapping permission.
    let valid = smaps(0);
    for bytes in [
        Vec::new(),
        valid.as_bytes()[..valid.len() - 1].to_vec(),
        valid.replace("ProtectionKey: 0\n", "").into_bytes(),
        valid.replace("LazyFree: 0 kB\n", "").into_bytes(),
        valid
            .replace("VmFlags: rd wr mr mw me ac sd\n", "")
            .into_bytes(),
        vec![b'x'; MAX_SMAPS + 1],
    ] {
        let fd = Errno::result(unsafe {
            libc::memfd_create(c"mm-reader-metadata-specimen".as_ptr(), libc::MFD_CLOEXEC)
        })
        .unwrap();
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all_at(&bytes, 0).unwrap();
        reader::hooks::take();
        let result = reader::bounded_read(&mut file, MAX_SMAPS)
            .and_then(|bytes| mapping(&bytes, 0x1000, 0x1008).map(|_| ()));
        assert_eq!(
            result,
            Err(refused(if bytes.len() > MAX_SMAPS {
                Refusal::MetadataTooLarge
            } else {
                Refusal::MappingMetadata
            }))
        );
        assert_eq!(reader::hooks::take(), [Step::MetadataRead]);
    }
    let fd = Errno::result(unsafe {
        libc::memfd_create(c"mm-reader-metadata-positive".as_ptr(), libc::MFD_CLOEXEC)
    })
    .unwrap();
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all_at(valid.as_bytes(), 0).unwrap();
    let bytes = reader::bounded_read(&mut file, valid.len()).unwrap();
    assert_eq!(bytes, valid.as_bytes());
    assert_eq!(
        mapping(&bytes, 0x1000, 0x1008).unwrap().read_access(0),
        Ok(())
    );
}

#[test]
fn mm_read_target_namespace_parser_requires_original_outer_identity() {
    // Parser premises only; native mount/target controls are separate. The
    // worker's own proc_view must still require exactly one namespace entry.
    for status in [
        b"Pid:\t7\nNSpid:\t7\n".as_slice(),
        b"Pid:\t7\nNSpid:\t7 1\n",
        b"Pid:\t7\nNSpid:\t7 2 1\n",
    ] {
        assert_eq!(reader::target_proc_view(status, 7), Ok(()));
    }
    for status in [
        b"Pid:\t7\n".as_slice(),
        b"Pid:\t7\nNSpid:\t\n",
        b"Pid:\t7\nNSpid:\t70 7\n",
        b"Pid:\t7\nNSpid:\t7 0\n",
        b"Pid:\t7\nNSpid:\t7 -1\n",
        b"Pid:\t7\nNSpid:\t7 x\n",
        b"Pid:\t8\nNSpid:\t8 7\n",
        b"Pid:\t7\nNSpid:\t7\nNSpid:\t7\n",
        b"Pid:\t7\nPid:\t7\nNSpid:\t7\n",
        b"Pid:\t7\nNSpid:\t7",
        b"Pid:\t7\nNSpid:\t7 \xff\n",
    ] {
        assert_eq!(
            reader::target_proc_view(status, 7),
            Err(refused(Refusal::ProcfsViewMismatch))
        );
    }
    assert_eq!(
        proc_view(b"Pid:\t7\nNSpid:\t7 1\n", 7),
        Err(refused(Refusal::ProcfsViewMismatch))
    );
}
