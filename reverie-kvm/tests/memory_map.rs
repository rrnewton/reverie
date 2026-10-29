/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::memory_map` on a KVM guest lists the part of the address space the
//! backend keeps enough of, its stack and its program break's area, as Linux
//! lists them, and says that the list is not complete: the same C guest, run
//! natively without address randomization (the layout the backend gives
//! every guest), reads `/proc/<tid>/maps` at the points where the KVM run
//! asks the Tool. The points cover the break grown by three pages and its
//! middle page made read-only, inaccessible, read and execute, write-only and
//! read-write again; a page of the stack made inaccessible and accessible
//! again; the break shrunk back; a sibling thread, asked about by itself and
//! by the leader; a child process; and numbers that name no guest thread.
//!
//! Where the backend keeps what Linux shows, the fact is Linux's: the heap's
//! mappings byte for byte, and the stack's but for where the lowest one
//! starts. The backend's limits show as named differences: it maps the
//! whole stack window at load, where Linux maps the stack's arguments and
//! 128 KiB and grows it on demand; it keeps no execute permission and no read
//! permission apart from access; it lists nothing else; and another process's
//! map is not provided. The fact's stack is also the one line of the
//! backend's own `/proc/self/maps`, which the guest reads on KVM today.

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
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
const HEAP: u64 = 2;
const HEAP_READ_ONLY: u64 = 3;
const HEAP_NONE: u64 = 4;
const HEAP_EXEC: u64 = 5;
const HEAP_WRITE_ONLY: u64 = 6;
const HEAP_JOINED: u64 = 7;
const STACK_GUARD: u64 = 8;
const STACK_JOINED: u64 = 9;
const BREAK_SHRUNK: u64 = 10;
const SIBLING_LEADER: u64 = 11;
const SIBLING: u64 = 12;
const SIBLING_SELF: u64 = 13;
const CHILD: u64 = 14;
const ZOMBIE_CHILD: u64 = 15;
const REAPED_CHILD: u64 = 16;
const HOST_PARENT: u64 = 17;
const NO_TASK: u64 = 18;
const ZERO: u64 = 19;
const NEGATIVE: u64 = 20;
const POINTS: u64 = 20;

/// The top of the user address space with four-level paging, where both
/// Linux without randomization and the backend end the stack.
const TOP: u64 = 0x7fff_ffff_f000;
/// The start of the backend's stack window: 8 MiB below the end of the user
/// address space.
const WINDOW_START: u64 = 0x7fff_ff80_0000;
const PAGE: u64 = 4096;
const RW: u32 = (libc::PROT_READ | libc::PROT_WRITE) as u32;
const READ: u32 = libc::PROT_READ as u32;

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
/// errno of its read of that thread's `maps`, whether it asked, the break's
/// first page and the stack page it protected.
#[derive(Debug, Default)]
struct Listing {
    tids: BTreeMap<u64, i64>,
    reads: BTreeMap<u64, Result<Vec<u8>, i32>>,
    asked: Vec<u64>,
    heap: u64,
    guard: u64,
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
            ["heap", address] => listing.heap = u64::from_str_radix(address, 16).unwrap(),
            ["guard", address] => listing.guard = u64::from_str_radix(address, 16).unwrap(),
            ["tid", point, tid] => {
                listing
                    .tids
                    .insert(point.parse().unwrap(), tid.parse().unwrap());
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

/// The mappings of `map` named the heap.
fn heap(map: &MemoryMap) -> Vec<MappingFact> {
    map.mappings
        .iter()
        .filter(|mapping| mapping.backing == MappingBacking::Heap)
        .cloned()
        .collect()
}

/// The mappings of `map` in the backend's stack window: the stack, and the
/// parts `mprotect(2)` split from it. Without randomization Linux places no
/// other mapping there.
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

fn mapping(start: u64, end: u64, prot: u32, backing: MappingBacking) -> MappingFact {
    MappingFact::new(start, end, prot, false, 0, backing)
}

#[test]
fn a_kvm_guest_reports_the_stack_and_heap_linux_shows() {
    Kvm::new().expect("memory map test requires usable /dev/kvm");
    let directory = Directory::new();
    let source = directory.0.join("memory_map.c");
    let executable = directory.0.join("memory_map");
    std::fs::write(&source, include_str!("fixtures/memory_map.c")).unwrap();
    let compiled = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-std=c11", "-pthread", "-Wall", "-Wextra", "-Werror"])
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

    // Natively, as the backend runs the guest: without address
    // randomization, and with an empty environment.
    let mut command = std::process::Command::new(&executable);
    command.arg("native").env_clear();
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
    let native = command.output().unwrap();
    assert!(native.status.success(), "native: {native:?}");
    let native = listing(&String::from_utf8(native.stdout).unwrap());
    assert_eq!(native.reads.len() as u64, POINTS, "{native:?}");
    assert!(native.asked.is_empty());

    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap(), "tool"],
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
    // Every point asked once, in order, but that the sibling asks about
    // itself before the leader asks about either thread.
    let order: Vec<u64> = (START..=BREAK_SHRUNK)
        .chain([SIBLING_SELF, SIBLING_LEADER, SIBLING])
        .chain(CHILD..=NEGATIVE)
        .collect();
    assert_eq!(kvm.asked, order);
    let facts = log.0.into_inner().unwrap();
    assert_eq!(facts.len() as u64, POINTS, "{facts:?}");
    for (point, (tid, _)) in &facts {
        assert_eq!(i64::from(*tid), kvm.tids[point], "point {point}");
    }
    let fact = |point: u64| match &facts[&point].1 {
        Ok(map) => {
            assert!(!map.complete, "point {point}: {map:?}");
            map.clone()
        }
        other => panic!("point {point}: {other:?}"),
    };

    // The backend's layout is Linux's without randomization: the same first
    // page of the break, the stack's top at the top of the user address space.
    assert_eq!(kvm.heap, native.heap);
    assert_eq!(kvm.guard, native.guard);
    let base = kvm.heap;
    let guard = kvm.guard;

    // The guest's own address space at every point it asks about: the stack
    // and the break's area and nothing else; the heap Linux's, byte for
    // byte; the stack Linux's but for where its lowest mapping starts, which
    // is the window's start on KVM, which maps the whole window at load,
    // where Linux maps the stack's arguments and 128 KiB below them.
    let own_points = [
        START,
        HEAP,
        HEAP_READ_ONLY,
        HEAP_NONE,
        HEAP_EXEC,
        HEAP_WRITE_ONLY,
        HEAP_JOINED,
        STACK_GUARD,
        STACK_JOINED,
        BREAK_SHRUNK,
        SIBLING_LEADER,
        SIBLING,
        SIBLING_SELF,
    ];
    for point in own_points {
        let (map, linux) = (fact(point), native.map(point));
        let (stack, linux_stack) = (stack_area(&map), stack_area(&linux));
        assert_eq!(
            map.mappings.len(),
            heap(&map).len() + stack.len(),
            "point {point}: {map:?}"
        );
        assert!(!stack.is_empty(), "point {point}");
        assert_eq!(stack.len(), linux_stack.len(), "point {point}: {linux:?}");
        assert_eq!(stack[0].start, WINDOW_START, "point {point}");
        assert!(linux_stack[0].start > WINDOW_START, "point {point}");
        assert_eq!(
            stack.iter().map(without_start).collect::<Vec<_>>(),
            linux_stack.iter().map(without_start).collect::<Vec<_>>(),
            "point {point}"
        );
        assert_eq!(stack[1..], linux_stack[1..], "point {point}");
        assert_eq!(stack.last().unwrap().end, TOP, "point {point}");
        if ![HEAP_EXEC, HEAP_WRITE_ONLY].contains(&point) {
            assert_eq!(heap(&map), heap(&linux), "point {point}: Linux's {linux:?}");
        }
    }

    // The points spelled out.
    let stack = mapping(WINDOW_START, TOP, RW, MappingBacking::Stack);
    let with_stack = |mut mappings: Vec<MappingFact>| {
        mappings.push(stack.clone());
        mappings
    };
    let heap_page = |page: u64, prot: u32| {
        mapping(
            base + page * PAGE,
            base + (page + 1) * PAGE,
            prot,
            MappingBacking::Heap,
        )
    };
    let whole_heap = mapping(base, base + 3 * PAGE, RW, MappingBacking::Heap);
    assert_eq!(fact(START).mappings, std::slice::from_ref(&stack));
    assert_eq!(fact(HEAP).mappings, with_stack(vec![whole_heap.clone()]));
    assert_eq!(
        fact(HEAP_READ_ONLY).mappings,
        with_stack(vec![heap_page(0, RW), heap_page(1, READ), heap_page(2, RW)])
    );
    assert_eq!(
        fact(HEAP_NONE).mappings,
        with_stack(vec![heap_page(0, RW), heap_page(1, 0), heap_page(2, RW)])
    );
    assert_eq!(
        fact(HEAP_JOINED).mappings,
        with_stack(vec![whole_heap.clone()])
    );
    assert_eq!(
        fact(STACK_GUARD).mappings,
        [
            whole_heap.clone(),
            mapping(WINDOW_START, guard, RW, MappingBacking::Anonymous),
            mapping(guard, guard + PAGE, 0, MappingBacking::Anonymous),
            mapping(guard + PAGE, TOP, RW, MappingBacking::Stack),
        ]
    );
    assert_eq!(fact(STACK_JOINED).mappings, with_stack(vec![whole_heap]));
    assert_eq!(fact(BREAK_SHRUNK).mappings, std::slice::from_ref(&stack));
    // The threads share the address space: the sibling's map, asked by
    // either thread, is the leader's, and so natively.
    assert_eq!(fact(SIBLING), fact(SIBLING_LEADER));
    assert_eq!(fact(SIBLING_SELF), fact(SIBLING_LEADER));
    assert_eq!(native.reads[&SIBLING], native.reads[&SIBLING_LEADER]);
    assert_eq!(native.reads[&SIBLING_SELF], native.reads[&SIBLING_LEADER]);
    assert_ne!(facts[&SIBLING].0, facts[&SIBLING_LEADER].0);

    // Named difference: the backend keeps no execute permission. Linux shows
    // the middle page as r-x; the fact, readable only.
    assert_eq!(
        heap(&native.map(HEAP_EXEC)),
        [
            heap_page(0, RW),
            heap_page(1, (libc::PROT_READ | libc::PROT_EXEC) as u32),
            heap_page(2, RW)
        ]
    );
    assert_eq!(
        fact(HEAP_EXEC).mappings,
        with_stack(vec![heap_page(0, RW), heap_page(1, READ), heap_page(2, RW)])
    );
    // Named difference: the backend keeps no read permission apart from
    // access. Linux shows the middle page as -w-; the fact's page is read and
    // write, one mapping with its neighbours.
    assert_eq!(
        heap(&native.map(HEAP_WRITE_ONLY)),
        [
            heap_page(0, RW),
            heap_page(1, libc::PROT_WRITE as u32),
            heap_page(2, RW)
        ]
    );
    assert_eq!(
        fact(HEAP_WRITE_ONLY).mappings,
        with_stack(vec![mapping(
            base,
            base + 3 * PAGE,
            RW,
            MappingBacking::Heap
        )])
    );

    // Named difference: the list is not complete. Linux also lists the
    // image, the libraries, the vDSO's pages and every other mapping.
    for point in own_points {
        let linux = native.map(point);
        assert!(linux.complete);
        assert!(
            linux.mappings.len() > fact(point).mappings.len() + 4,
            "point {point}"
        );
        assert!(
            linux
                .mappings
                .iter()
                .any(|mapping| matches!(mapping.backing, MappingBacking::File { .. })),
            "point {point}"
        );
    }

    // The backend's own /proc/self/maps, which the guest reads on KVM today,
    // is one line: the fact's stack.
    assert_eq!(
        kvm.reads[&START],
        Ok(format!("{WINDOW_START:012x}-{TOP:012x} rw-p 00000000 00:00 0 [stack]\n").into_bytes())
    );

    // Named difference: another process's address space is not this
    // executor's. Linux shows the child's map.
    assert!(!native.map(CHILD).mappings.is_empty());
    assert_eq!(facts[&CHILD].1, Err(FactError::NotProvided));
    assert_ne!(facts[&CHILD].0, facts[&START].0);

    // Numbers that name no live guest thread: the exited child, as a zombie
    // (whose map Linux prints empty: it has no address space left) and once
    // reaped, the guest's parent, a number no task has, zero and -1.
    for point in [
        ZOMBIE_CHILD,
        REAPED_CHILD,
        HOST_PARENT,
        NO_TASK,
        ZERO,
        NEGATIVE,
    ] {
        assert_eq!(
            facts[&point].1,
            Err(FactError::Errno(Errno::ESRCH)),
            "point {point}"
        );
    }
    assert_eq!(native.reads[&ZOMBIE_CHILD], Ok(Vec::new()));
    assert_eq!(facts[&ZOMBIE_CHILD].0, facts[&CHILD].0);
    for point in [REAPED_CHILD, NO_TASK, ZERO, NEGATIVE] {
        assert_eq!(native.reads[&point], Err(libc::ENOENT), "point {point}");
    }
    assert!(native.reads[&HOST_PARENT].is_ok());
    assert_eq!(
        [facts[&NO_TASK].0, facts[&ZERO].0, facts[&NEGATIVE].0],
        [i32::MAX, 0, -1]
    );
}
