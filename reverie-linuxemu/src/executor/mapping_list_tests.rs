/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The lines the list of mappings (`mapping_list`) gives for mappings of
//! files and of anonymous memory, checked against Linux itself. Each test
//! runs one sequence of calls twice, through the executor's syscalls and
//! natively in this test process on the same host files, and requires the
//! executor's lines over the range to equal the lines this process's own
//! `/proc/self/maps` shows: the same splits and merges, protection, sharing,
//! file offset, and the device, inode and name Linux prints for the file
//! (the superblock's device, the file's inode, `d_path`'s text).

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::MappingBacking;

use super::*;

const PAGE: u64 = PAGE_SIZE;
const NONE: u64 = libc::PROT_NONE as u64;
const R: u64 = libc::PROT_READ as u64;
const RW: u64 = (libc::PROT_READ | libc::PROT_WRITE) as u64;
const PRIVATE: u64 = libc::MAP_PRIVATE as u64;
const SHARED: u64 = libc::MAP_SHARED as u64;
const ANONYMOUS: u64 = libc::MAP_ANONYMOUS as u64;
const FIXED: u64 = libc::MAP_FIXED as u64;
/// `MADV_GUARD_INSTALL` (Linux 6.13), which the libc crate may lack.
const MADV_GUARD_INSTALL: u64 = 102;
/// The pages each sequence runs in, reserved first as one inaccessible
/// mapping and then mapped over with `MAP_FIXED`.
const RANGE_PAGES: u64 = 16;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-linuxemu-mapping-list-{}-{}",
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

/// A call of a sequence, at a page index of the range.
#[derive(Clone, Copy, Debug)]
enum Op {
    /// `mmap(MAP_FIXED)` of `pages` pages of anonymous memory.
    Anonymous {
        at: u64,
        pages: u64,
        prot: u64,
        flags: u64,
    },
    /// `mmap(MAP_FIXED)` of `pages` pages of file `file` (an index into the
    /// sequence's files) from page `page` of it.
    File {
        at: u64,
        pages: u64,
        prot: u64,
        flags: u64,
        file: usize,
        page: u64,
    },
    Protect {
        at: u64,
        pages: u64,
        prot: u64,
    },
    Advise {
        at: u64,
        pages: u64,
        advice: u64,
    },
}

/// The host files a sequence maps: each is an open file description, so two
/// entries that open one file are two descriptions of it.
struct Files(Vec<std::fs::File>);

/// A memfd of `pages` pages named `name`.
fn memfd(name: &std::ffi::CStr, pages: u64) -> std::fs::File {
    use std::os::fd::FromRawFd;
    // SAFETY: the name is NUL-terminated; the result is checked.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: memfd_create returned a new descriptor this test owns.
    let file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.set_len(pages * PAGE).unwrap();
    file
}

/// Another open file description of the file `file` holds.
fn reopened(file: &std::fs::File) -> std::fs::File {
    std::fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap()
}

/// A line over the range, as the tests compare it: its pages relative to the
/// range's start, its permissions and sharing, its file offset, and what
/// backs it, with the file's raw device, inode and name.
type Line = (u64, u64, u32, bool, u64, MappingBacking);

/// `mapping`'s line relative to the range at user address `base`.
/// Shared anonymous memory's inode is left out (zero): each side's mapping
/// has an object of its own, as two processes' mappings do on Linux.
fn relative(mapping: &reverie::MappingFact, base: u64) -> Line {
    let backing = match &mapping.backing {
        MappingBacking::File {
            raw_device, path, ..
        } if path.as_slice() == b"/dev/zero (deleted)" => MappingBacking::File {
            raw_device: *raw_device,
            raw_inode: 0,
            path: path.clone(),
        },
        other => other.clone(),
    };
    (
        (mapping.start - base) / PAGE,
        (mapping.end - base) / PAGE,
        mapping.prot,
        mapping.shared,
        mapping.offset,
        backing,
    )
}

/// The lines of `mappings` over the `RANGE_PAGES` pages at `base`, which
/// must each lie wholly inside it.
fn lines_over(mappings: &[reverie::MappingFact], base: u64) -> Vec<Line> {
    let end = base + RANGE_PAGES * PAGE;
    mappings
        .iter()
        .filter(|mapping| mapping.start < end && base < mapping.end)
        .inspect(|mapping| {
            assert!(
                base <= mapping.start && mapping.end <= end,
                "a line crosses the range: {mapping:?}"
            )
        })
        .map(|mapping| relative(mapping, base))
        .collect()
}

/// Runs `ops` natively in this process over a fresh range and returns the
/// lines this process's own `/proc/self/maps` shows over it, after
/// `after` ran. The range is unmapped afterwards.
fn native(ops: &[Op], files: &Files, after: impl FnOnce()) -> Vec<Line> {
    let length = (RANGE_PAGES * PAGE) as usize;
    // SAFETY: a new inaccessible anonymous mapping of the host's choosing,
    // a page wider on each side; those two pages are unmapped at once, so
    // that no line of the range can merge with a neighbour of the process's
    // (a thread's inaccessible stack guard, say).
    let wide = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length + 2 * PAGE as usize,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(wide, libc::MAP_FAILED);
    let base = wide as u64 + PAGE;
    // SAFETY: the two edge pages of the mapping just made.
    unsafe {
        assert_eq!(libc::munmap(wide, PAGE as usize), 0);
        assert_eq!(
            libc::munmap(
                (base + RANGE_PAGES * PAGE) as *mut libc::c_void,
                PAGE as usize
            ),
            0
        );
    }
    let address = |at: u64| (base + at * PAGE) as *mut libc::c_void;
    for op in ops {
        // SAFETY: every call names pages of the range this test reserved,
        // which nothing else in the process uses.
        let result = unsafe {
            match *op {
                Op::Anonymous {
                    at,
                    pages,
                    prot,
                    flags,
                } => {
                    libc::mmap(
                        address(at),
                        (pages * PAGE) as usize,
                        prot as libc::c_int,
                        (flags | ANONYMOUS | FIXED) as libc::c_int,
                        -1,
                        0,
                    ) as u64
                        != address(at) as u64
                }
                Op::File {
                    at,
                    pages,
                    prot,
                    flags,
                    file,
                    page,
                } => {
                    libc::mmap(
                        address(at),
                        (pages * PAGE) as usize,
                        prot as libc::c_int,
                        (flags | FIXED) as libc::c_int,
                        files.0[file].as_raw_fd(),
                        (page * PAGE) as libc::off_t,
                    ) as u64
                        != address(at) as u64
                }
                Op::Protect { at, pages, prot } => {
                    libc::mprotect(address(at), (pages * PAGE) as usize, prot as libc::c_int) != 0
                }
                Op::Advise { at, pages, advice } => {
                    libc::madvise(address(at), (pages * PAGE) as usize, advice as libc::c_int) != 0
                }
            }
        };
        assert!(
            !result,
            "native {op:?}: {}",
            std::io::Error::last_os_error()
        );
    }
    after();
    // Other threads of the test process may change their own mappings while
    // the file is read; each line is parsed by itself, and only the lines
    // over the range, which nothing else changes, are kept.
    let text = std::fs::read("/proc/self/maps").unwrap();
    let mappings: Vec<reverie::MappingFact> = text
        .split_inclusive(|byte| *byte == b'\n')
        .filter_map(|line| reverie::MemoryMap::from_maps(line).ok())
        .flat_map(|map| map.mappings)
        .collect();
    let lines = lines_over(&mappings, base);
    // SAFETY: the range this test reserved.
    assert_eq!(
        unsafe { libc::munmap(base as *mut libc::c_void, length) },
        0
    );
    lines
}

fn call(
    executor: &mut ElfExecutor,
    memory: &GuestMemory,
    number: libc::c_long,
    args: [u64; 6],
) -> i64 {
    executor.execute(&SyscallRequest::new(number as u64, args), memory)
}

/// The address space of a minimal image loaded from a file, as the backend
/// loads a program (so that every page's mapping is known and the list can
/// be complete), and an executor of its first thread.
fn loaded(directory: &Directory) -> (ElfExecutor, GuestMemory) {
    let image = directory.0.join("mapping-list-image");
    std::fs::write(&image, crate::elf::minimal_test_elf(&[0xf4])).unwrap();
    let mut memory = GuestMemory::new(0, 32 * 1024 * 1024).unwrap();
    let mut state = crate::elf::load_static_elf_file(
        &mut memory,
        std::fs::File::open(&image).unwrap(),
        &["mapping-list"],
        &[],
        &directory.0,
    )
    .unwrap();
    state.children = ChildWaitContext::test_root();
    (ElfExecutor::new(state, false), memory)
}

/// Runs `ops` through the executor over a fresh range of its address space
/// and returns the lines its list of mappings gives over it, after `after`
/// ran, and whether the list says it is complete.
fn emulated(ops: &[Op], files: &Files, after: impl FnOnce()) -> (Vec<Line>, bool) {
    let directory = Directory::new();
    let (mut executor, memory) = loaded(&directory);
    let length = RANGE_PAGES * PAGE;
    // A page wider on each side, those two pages unmapped at once, as
    // natively.
    let wide = call(
        &mut executor,
        &memory,
        libc::SYS_mmap,
        [0, length + 2 * PAGE, NONE, PRIVATE | ANONYMOUS, u64::MAX, 0],
    );
    assert!(wide > 0, "mmap of the range: {wide}");
    let base = wide as u64 + PAGE;
    for edge in [wide as u64, base + length] {
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_munmap,
                [edge, PAGE, 0, 0, 0, 0]
            ),
            0
        );
    }
    for op in ops {
        let (number, args) = match *op {
            Op::Anonymous {
                at,
                pages,
                prot,
                flags,
            } => (
                libc::SYS_mmap,
                [
                    base + at * PAGE,
                    pages * PAGE,
                    prot,
                    flags | ANONYMOUS | FIXED,
                    u64::MAX,
                    0,
                ],
            ),
            Op::File {
                at,
                pages,
                prot,
                flags,
                file,
                page,
            } => {
                // The executor's own calls replace its state's descriptors
                // with its table's, so each descriptor goes in just before
                // its mmap, as one more descriptor of the same description.
                let fd = insert_file_with_flags(
                    &mut executor.state,
                    files.0[file].try_clone().unwrap(),
                    false,
                    None,
                );
                assert!(fd >= 0);
                let mut owner = memory.clone();
                let result = execute_basic_syscall(
                    &mut owner,
                    &mut executor.state,
                    &SyscallRequest::new(
                        libc::SYS_mmap as u64,
                        [
                            base + at * PAGE,
                            pages * PAGE,
                            prot,
                            flags | FIXED,
                            fd as u64,
                            page * PAGE,
                        ],
                    ),
                );
                match result {
                    SyscallAction::Continue { result, .. } => {
                        assert_eq!(result, (base + at * PAGE) as i64, "{op:?}")
                    }
                    _ => panic!("mmap {op:?} did not continue"),
                }
                continue;
            }
            Op::Protect { at, pages, prot } => (
                libc::SYS_mprotect,
                [base + at * PAGE, pages * PAGE, prot, 0, 0, 0],
            ),
            Op::Advise { at, pages, advice } => (
                libc::SYS_madvise,
                [base + at * PAGE, pages * PAGE, advice, 0, 0, 0],
            ),
        };
        let result = call(&mut executor, &memory, number, args);
        if number == libc::SYS_mmap {
            assert_eq!(result, args[0] as i64, "{op:?}");
        } else {
            assert_eq!(result, 0, "{op:?}");
        }
    }
    // The guest's descriptors of the mapped files are gone by now: the
    // executor's next call replaced its state's table with its own, which
    // never held them. The mappings keep their files, as Linux's keep their
    // `struct file`.
    after();
    let map = executor.memory_map(&memory, executor.state.tid).unwrap();
    (lines_over(&map.mappings, base), map.complete)
}

/// The executor's lines equal Linux's for `ops` over `files`, and its list
/// is complete.
fn assert_linux(ops: &[Op], files: &Files) -> Vec<Line> {
    let linux = native(ops, files, || {});
    let (lines, complete) = emulated(ops, files, || {});
    assert!(complete, "the list says it is incomplete: {lines:#?}");
    assert_eq!(lines, linux, "executor (left) and Linux (right) differ");
    lines
}

/// A memfd mapped shared and private, through one description and through
/// another, at contiguous offsets and not, and a regular file mapped
/// privately from an offset, each listed as Linux lists it: the memfd's
/// device and inode (an internal mount's, whose device is the file's own)
/// and its name `/memfd:NAME (deleted)`, the regular file's superblock
/// device and inode and its path.
#[test]
fn file_mappings_are_listed_with_linuxs_file_identity() {
    let directory = Directory::new();
    let path = directory.0.join("mapped-file");
    std::fs::write(&path, vec![7u8; 2 * PAGE as usize]).unwrap();
    let memfd = memfd(c"mapping-list-identity", 4);
    let files = Files(vec![
        memfd.try_clone().unwrap(),
        reopened(&memfd),
        std::fs::File::open(&path).unwrap(),
    ]);
    let ops = [
        // One description at contiguous offsets: one line.
        Op::File {
            at: 0,
            pages: 1,
            prot: R,
            flags: PRIVATE,
            file: 0,
            page: 0,
        },
        Op::File {
            at: 1,
            pages: 1,
            prot: R,
            flags: PRIVATE,
            file: 0,
            page: 1,
        },
        // Another description of the same memfd at the next offset: a line
        // of its own (`is_mergeable_vma` compares the `struct file`).
        Op::File {
            at: 2,
            pages: 1,
            prot: R,
            flags: PRIVATE,
            file: 1,
            page: 2,
        },
        // The first description again, at an offset that does not follow.
        Op::File {
            at: 3,
            pages: 1,
            prot: R,
            flags: PRIVATE,
            file: 0,
            page: 0,
        },
        // Shared and writable.
        Op::File {
            at: 5,
            pages: 2,
            prot: RW,
            flags: SHARED,
            file: 0,
            page: 0,
        },
        // A regular file, privately, from its second page.
        Op::File {
            at: 8,
            pages: 1,
            prot: R,
            flags: PRIVATE,
            file: 2,
            page: 1,
        },
    ];
    let lines = assert_linux(&ops, &files);
    let names: Vec<&[u8]> = lines
        .iter()
        .filter_map(|line| match &line.5 {
            MappingBacking::File { path, .. } => Some(path.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(names[0], b"/memfd:mapping-list-identity (deleted)");
    assert_eq!(lines[0].0..lines[0].1, 0..2, "{lines:#?}");
    assert!(
        names.last().unwrap().ends_with(b"/mapped-file"),
        "{names:?}"
    );
}

/// The regular file's line names it ` (deleted)` once it is unlinked after
/// it was mapped, as Linux names it: the name is read when the list is.
#[test]
fn an_unlinked_mapped_file_is_named_deleted() {
    let directory = Directory::new();
    let ops = [Op::File {
        at: 2,
        pages: 1,
        prot: R,
        flags: PRIVATE,
        file: 0,
        page: 0,
    }];
    let mut lines = Vec::new();
    for side in ["native", "executor"] {
        let path = directory.0.join(format!("unlinked-{side}"));
        std::fs::write(&path, vec![1u8; PAGE as usize]).unwrap();
        let files = Files(vec![std::fs::File::open(&path).unwrap()]);
        let unlink = || std::fs::remove_file(&path).unwrap();
        let side_lines = if side == "native" {
            native(&ops, &files, unlink)
        } else {
            let (side_lines, complete) = emulated(&ops, &files, unlink);
            assert!(complete);
            side_lines
        };
        let backing = side_lines
            .iter()
            .find(|line| line.0 == 2)
            .map(|line| line.5.clone());
        match backing {
            Some(MappingBacking::File { path: name, .. }) => {
                assert!(
                    name.ends_with(format!("/unlinked-{side} (deleted)").as_bytes()),
                    "{side}: {}",
                    String::from_utf8_lossy(&name)
                );
            }
            other => panic!("{side}: {other:?}"),
        }
        lines.push(
            side_lines
                .into_iter()
                .map(|line| (line.0, line.1, line.2, line.3, line.4))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(lines[0], lines[1]);
}

/// Anonymous memory merges and splits as Linux's does: two adjacent
/// mappings with equal protection made by two calls are one line; a
/// protection change in the middle splits it in three and setting it back
/// merges it again; a guard page splits nothing; an inaccessible mapping
/// beside an accessible one stays apart; shared anonymous memory is a
/// `/dev/zero (deleted)` line of its own beside private memory.
#[test]
fn anonymous_mappings_merge_and_split_as_linuxs_do() {
    let files = Files(Vec::new());
    let ops = [
        Op::Anonymous {
            at: 0,
            pages: 2,
            prot: RW,
            flags: PRIVATE,
        },
        Op::Anonymous {
            at: 2,
            pages: 2,
            prot: RW,
            flags: PRIVATE,
        },
        Op::Protect {
            at: 1,
            pages: 1,
            prot: R,
        },
        Op::Protect {
            at: 1,
            pages: 1,
            prot: RW,
        },
        Op::Advise {
            at: 2,
            pages: 1,
            advice: MADV_GUARD_INSTALL,
        },
        Op::Protect {
            at: 6,
            pages: 1,
            prot: RW,
        },
        Op::Protect {
            at: 7,
            pages: 1,
            prot: R,
        },
        Op::Anonymous {
            at: 10,
            pages: 1,
            prot: RW,
            flags: SHARED,
        },
        Op::Anonymous {
            at: 11,
            pages: 1,
            prot: RW,
            flags: PRIVATE,
        },
    ];
    let lines = assert_linux(&ops, &files);
    let ranges: Vec<(u64, u64)> = lines.iter().map(|line| (line.0, line.1)).collect();
    assert_eq!(
        ranges,
        [
            (0, 4),
            (4, 6),
            (6, 7),
            (7, 8),
            (8, 10),
            (10, 11),
            (11, 12),
            (12, 16)
        ],
        "{lines:#?}"
    );
}
