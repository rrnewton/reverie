/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::memory_map` on a KVM guest lists the one mapping the backend can
//! establish exactly, the process's `[stack]`, while nothing has changed the
//! stack window since the image started, and says that the list is not
//! complete. The same C guest (`fixtures/memory_map.c`), run natively without
//! address randomization and with an empty environment (the layout and the
//! environment the backend gives every guest), reads `/proc/<tid>/maps` at
//! the points where the KVM run asks the Tool.
//!
//! While the window is unchanged, the entry is Linux's `[stack]` line but
//! for where it starts: the backend maps the whole window at load, where
//! Linux maps the stack's arguments and 128 KiB below them and grows the
//! stack on demand. Once a page of the window is protected (even if it is
//! then set back, and even by an mprotect that fails part way, which Linux
//! applies to the mappings before the hole), unmapped, advised with a
//! flag-changing madvise, or munlocked, the list is empty in that process
//! and in a child forked after it, where Linux shows a split stack or, once
//! it merged the parts back again, the stack's one line. A child that shares
//! the address space (vfork, and clone or clone3 with CLONE_VM|CLONE_VFORK)
//! and changes the stack empties its parent's list too, and a change the
//! parent's other threads make while it runs empties the child's. A new
//! image starts it again, and a posix_spawn child's new image leaves its
//! parent's entry. Advice that keeps the mapping's flags keeps it, and so
//! does mlock, which the backend refuses: there the comparison is with
//! Linux's stack without the mlock, since the backend's did not change.
//! Another process's map is not provided, and a number no task has is ESRCH.
//! For an image whose `PT_GNU_STACK` asks for an executable stack the entry
//! is executable, as Linux maps that stack.

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_ioctls::Kvm;
use reverie::Errno;
use reverie::Error;
use reverie::FactError;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::MappingBacking;
use reverie::MappingFact;
use reverie::MemoryMap;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_kvm::KvmBackend;

/// The first argument of the guest's `tuxcall` that asks for a fact
/// (`ASK` in the fixture).
const ASK: u64 = 0x6d65_6d6f_7279_6d61;

// The fixture's numbered points.
const START: u64 = 1;
const KEPT: u64 = 2;
const PROTECTED: u64 = 3;
const PROTECTED_BACK: u64 = 4;
const ADVISED: u64 = 5;
const MLOCKED: u64 = 6;
const MUNLOCKED: u64 = 7;
const REMAPPED: u64 = 8;
const FORKED: u64 = 9;
const EXECUTED: u64 = 10;
const PARTIAL: u64 = 11;
const VFORKED: u64 = 12;
const CLONED: u64 = 13;
const CLONED3: u64 = 14;
const VFORK_SEES: u64 = 15;
const SPAWNED: u64 = 16;
const SPAWN_PARENT: u64 = 17;
const PARENT_AGAIN: u64 = 18;
const SIBLING: u64 = 19;
const SIBLING_CHANGED: u64 = 20;
const CHILD: u64 = 21;
const NO_TASK: u64 = 22;
const POINTS: u64 = 22;

/// The top of the user address space with four-level paging, where both
/// Linux without randomization and the backend end the stack.
const TOP: u64 = 0x7fff_ffff_f000;
/// The start of the backend's stack window: 8 MiB below the end of the user
/// address space.
const WINDOW_START: u64 = 0x7fff_ff80_0000;
const RW: u32 = (libc::PROT_READ | libc::PROT_WRITE) as u32;
const RWX: u32 = (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) as u32;

type Fact = Result<MemoryMap, FactError>;

/// The facts the Tool read, by the guest's point number.
#[derive(Default)]
struct FactLog(Mutex<BTreeMap<u64, (i32, Fact)>>);

#[reverie::global_tool]
impl GlobalTool for FactLog {
    type Request = (u64, i32, Fact);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, request: Self::Request) {
        let (point, tid, fact) = request;
        let previous = self.0.lock().unwrap().insert(point, (tid, fact));
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
        let (tid, point) = (args.arg1 as i32, args.arg2 as u64);
        let fact = guest.memory_map(Pid::from_raw(tid));
        guest.send_rpc((point, tid, fact)).await;
        Ok(0)
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-kvm-memory-map-{}-{}",
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

/// What one run of the fixture listed: each point's thread and the text or
/// errno of its read of that thread's `maps`, the points it asked at, and
/// the results it printed.
#[derive(Debug, Default)]
struct Listing {
    tids: BTreeMap<u64, i64>,
    reads: BTreeMap<u64, Result<Vec<u8>, i32>>,
    asked: Vec<u64>,
    results: BTreeMap<u64, (i64, i32)>,
}

impl Listing {
    fn map(&self, point: u64) -> MemoryMap {
        match &self.reads[&point] {
            Ok(text) => MemoryMap::from_maps(text).unwrap(),
            other => panic!("point {point}: {other:?}"),
        }
    }
}

fn listing(stdout: &str) -> Listing {
    let mut listing = Listing::default();
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        match fields[..] {
            ["tid", point, tid] => {
                listing
                    .tids
                    .insert(point.parse().unwrap(), tid.parse().unwrap());
            }
            ["result", point, rc, errno] => {
                listing.results.insert(
                    point.parse().unwrap(),
                    (rc.parse().unwrap(), errno.parse().unwrap()),
                );
            }
            ["point", point, "asked"] => listing.asked.push(point.parse().unwrap()),
            ["point", point, "errno", errno] => {
                listing
                    .reads
                    .insert(point.parse().unwrap(), Err(errno.parse().unwrap()));
            }
            ["point", point, "text", hex] => {
                let text = (0..hex.len())
                    .step_by(2)
                    .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                    .collect();
                listing.reads.insert(point.parse().unwrap(), Ok(text));
            }
            _ => panic!("fixture line {line:?}"),
        }
    }
    listing
}

/// The mappings of `map` in the backend's stack window: the stack, and the
/// parts a change split from it. Without randomization Linux places no other
/// mapping there.
fn stack_area(map: &MemoryMap) -> Vec<MappingFact> {
    map.mappings
        .iter()
        .filter(|mapping| WINDOW_START <= mapping.start && mapping.end <= TOP)
        .cloned()
        .collect()
}

/// A mapping as the two runs' stacks are compared: everything but its start.
fn without_start(mapping: &MappingFact) -> (u64, u32, bool, u64, MappingBacking) {
    (
        mapping.end,
        mapping.prot,
        mapping.shared,
        mapping.offset,
        mapping.backing.clone(),
    )
}

/// The backend's list while the window is unchanged: the window, `prot`.
fn stack(prot: u32) -> MemoryMap {
    MemoryMap::new(
        vec![MappingFact::new(WINDOW_START, TOP, prot, false, 0, MappingBacking::Stack).unwrap()],
        false,
    )
    .unwrap()
}

/// Compiles the fixture into `directory` as `name`, with `flags`.
fn compile(directory: &Directory, name: &str, flags: &[&str]) -> PathBuf {
    let source = directory.0.join("memory_map.c");
    let executable = directory.0.join(name);
    std::fs::write(&source, include_str!("fixtures/memory_map.c")).unwrap();
    let compiled = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-std=c11", "-pthread", "-Wall", "-Wextra", "-Werror"])
        .args(flags)
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
    executable
}

/// Runs `executable` natively, as the backend runs the guest: without
/// address randomization, and with an empty environment.
fn native(executable: &Path, args: &[&str]) -> Listing {
    let mut command = std::process::Command::new(executable);
    command.args(args).env_clear();
    // SAFETY: personality(2) changes only the child about to exec, and is
    // async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::personality(libc::ADDR_NO_RANDOMIZE as libc::c_ulong) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let run = command.output().unwrap();
    assert!(run.status.success(), "native: {run:?}");
    let native = listing(&String::from_utf8(run.stdout).unwrap());
    assert!(native.asked.is_empty());
    native
}

/// Runs `executable` in a KVM VM under [`FactTool`]: what it listed, and the
/// facts the Tool read, by point.
fn kvm(directory: &Directory, executable: &Path, args: &[&str]) -> (Listing, BTreeMap<u64, Fact>) {
    let argv: Vec<&str> = std::iter::once(executable.to_str().unwrap())
        .chain(args.iter().copied())
        .collect();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(executable).unwrap(),
            &argv,
            &[],
            &directory.0,
        )
        .unwrap();
    let (log, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<FactTool>((), true))
            .unwrap();
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    let kvm = listing(&String::from_utf8(stdout).unwrap());
    let facts = log.0.into_inner().unwrap();
    for (point, (tid, _)) in &facts {
        assert_eq!(i64::from(*tid), kvm.tids[point], "point {point}");
    }
    let facts = facts
        .into_iter()
        .map(|(point, (_, fact))| (point, fact))
        .collect();
    (kvm, facts)
}

/// Checks that `fact`, a list with the stack, is Linux's `[stack]` line in
/// `linux` but for its start, and that Linux's stack is one mapping.
fn assert_linux_stack(point: u64, fact: &MemoryMap, linux: &MemoryMap) {
    let linux_stack = stack_area(linux);
    assert_eq!(linux_stack.len(), 1, "point {point}: {linux:?}");
    assert_eq!(fact.mappings.len(), 1, "point {point}: {fact:?}");
    let (ours, theirs) = (&fact.mappings[0], &linux_stack[0]);
    assert_eq!(without_start(ours), without_start(theirs), "point {point}");
    // Named difference: the window's start, below Linux's.
    assert_eq!(ours.start, WINDOW_START, "point {point}");
    assert!(theirs.start > WINDOW_START, "point {point}: {theirs:?}");
}

#[test]
fn a_kvm_guest_reports_its_stack_while_nothing_changed_it() {
    Kvm::new().expect("memory map test requires usable /dev/kvm");
    let directory = Directory::new();
    let executable = compile(&directory, "memory_map", &[]);
    let native = native(&executable, &["native"]);
    assert_eq!(native.reads.len() as u64, POINTS, "{native:?}");
    let (kvm, facts) = kvm(&directory, &executable, &["tool"]);
    assert_eq!(kvm.asked, (START..=NO_TASK).collect::<Vec<_>>());
    assert_eq!(facts.len() as u64, POINTS, "{facts:?}");
    for (point, fact) in &facts {
        if let Ok(map) = fact {
            assert!(!map.complete, "point {point}: {map:?}");
        }
    }

    // While the window is unchanged: as the process started, after advice
    // that keeps the mapping's flags, after an mlock the backend refuses
    // (ENOSYS), as a new image starts, as the image a posix_spawn child
    // executed starts and in its parent afterwards, in the process once its
    // children are done, and asked by a sibling thread before it changes
    // anything. Each is Linux's `[stack]` but for its start. The native run
    // leaves the mlock out, since the backend's stack did not change.
    let listed = [
        START,
        KEPT,
        MLOCKED,
        EXECUTED,
        SPAWNED,
        SPAWN_PARENT,
        PARENT_AGAIN,
        SIBLING,
    ];
    for point in listed {
        assert_eq!(facts[&point], Ok(stack(RW)), "point {point}");
        assert_linux_stack(point, &stack(RW), &native.map(point));
    }
    assert_eq!(kvm.results[&MLOCKED], (-1, libc::ENOSYS));
    assert!(!native.results.contains_key(&MLOCKED), "{native:?}");
    // The guest's own read on KVM is the executor's one synthetic line
    // today, the same range and protection.
    assert_eq!(
        kvm.reads[&START],
        Ok(format!("{WINDOW_START:012x}-{TOP:012x} rw-p 00000000 00:00 0 [stack]\n").into_bytes())
    );

    // Once the window changed, nothing is listed. Linux's stack at the same
    // points: split around a read-only page, and one mapping again once the
    // page is read-write again (the named difference: the backend cannot
    // tell the two apart, so it stays silent); split by MADV_RANDOM; one
    // mapping after munlock, which the backend records to be safe; split
    // around the page mapped again with MAP_FIXED, which the backend refuses
    // (ENOMEM) after the munmap; one mapping in a grandchild forked after its
    // parent set a page back, which inherits the change; split in two by an
    // mprotect of the top page and the page past it, which fails on both
    // with ENOMEM; split around the page that a child sharing the address
    // space made executable, made with vfork, with clone and with clone3, in
    // the parent once the child exited; the same, in a vfork child, around
    // the page a sibling thread of its parent made executable; one mapping
    // after the sibling set the leader's page back.
    for (point, linux_parts) in [
        (PROTECTED, 3),
        (PROTECTED_BACK, 1),
        (ADVISED, 3),
        (MUNLOCKED, 1),
        (REMAPPED, 3),
        (FORKED, 1),
        (PARTIAL, 2),
        (VFORKED, 3),
        (CLONED, 3),
        (CLONED3, 3),
        (VFORK_SEES, 3),
        (SIBLING_CHANGED, 1),
    ] {
        assert_eq!(
            facts[&point],
            Ok(MemoryMap::new(Vec::new(), false).unwrap()),
            "point {point}"
        );
        assert_eq!(
            stack_area(&native.map(point)).len(),
            linux_parts,
            "point {point}: {:?}",
            native.map(point)
        );
    }
    assert_eq!(kvm.results[&REMAPPED], (-1, libc::ENOMEM));
    assert_eq!(native.results[&REMAPPED], (0, 0));
    assert_eq!(kvm.results[&PARTIAL], (-1, libc::ENOMEM));
    assert_eq!(native.results[&PARTIAL], (-1, libc::ENOMEM));
    // The vfork child asked about itself; the posix_spawn child's image did.
    for point in [VFORK_SEES, SPAWNED] {
        assert_ne!(kvm.tids[&point], kvm.tids[&START], "point {point}");
        assert_ne!(native.tids[&point], native.tids[&START], "point {point}");
    }

    // Another process's address space is not this one's; no task has the
    // last number (natively the child's map is readable, and the number's
    // is not).
    assert_eq!(facts[&CHILD], Err(FactError::NotProvided));
    assert!(native.reads[&CHILD].is_ok());
    assert_eq!(facts[&NO_TASK], Err(FactError::Errno(Errno::ESRCH)));
    assert_eq!(native.reads[&NO_TASK], Err(libc::ENOENT));
}

/// An image whose `PT_GNU_STACK` header has `PF_X`: Linux maps its stack
/// executable, and the entry says so.
#[test]
fn a_kvm_guest_with_an_executable_stack_reports_it() {
    Kvm::new().expect("memory map test requires usable /dev/kvm");
    let directory = Directory::new();
    let executable = compile(&directory, "memory_map_execstack", &["-z", "execstack"]);
    let native = native(&executable, &["native", "start"]);
    let (kvm, facts) = kvm(&directory, &executable, &["tool", "start"]);
    assert_eq!(kvm.asked, [START]);
    assert_eq!(facts[&START], Ok(stack(RWX)));
    assert_linux_stack(START, &stack(RWX), &native.map(START));
}
