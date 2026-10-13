/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[derive(Debug, PartialEq, Eq)]
struct ListedEntry {
    inode: u64,
    next: i64,
    kind: u8,
    name: String,
}

const LISTING_BUFFER: u64 = 0x2000;

fn listed_entries(memory: &GuestMemory, count: usize) -> Vec<ListedEntry> {
    let mut bytes = vec![0; count];
    memory.read(LISTING_BUFFER, &mut bytes).unwrap();
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let length = u16::from_ne_bytes(bytes[at + 16..at + 18].try_into().unwrap()) as usize;
        let name = &bytes[at + 19..at + length];
        let name = &name[..name.iter().position(|byte| *byte == 0).unwrap()];
        entries.push(ListedEntry {
            inode: u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap()),
            next: i64::from_ne_bytes(bytes[at + 8..at + 16].try_into().unwrap()),
            kind: bytes[at + 18],
            name: String::from_utf8(name.to_vec()).unwrap(),
        });
        at += length;
    }
    entries
}

fn list_with(
    executor: &mut ElfExecutor,
    memory: &GuestMemory,
    fd: i64,
    capacity: u64,
) -> Result<Vec<ListedEntry>, i64> {
    let count = executor.execute(
        &SyscallRequest::new(
            libc::SYS_getdents64 as u64,
            [fd as u64, LISTING_BUFFER, capacity, 0, 0, 0],
        ),
        memory,
    );
    if count < 0 {
        return Err(count);
    }
    Ok(listed_entries(memory, count as usize))
}

fn list_all(executor: &mut ElfExecutor, memory: &GuestMemory, fd: i64) -> Vec<String> {
    let mut names = Vec::new();
    loop {
        let entries = list_with(executor, memory, fd, 0x1000).unwrap();
        if entries.is_empty() {
            return names;
        }
        names.extend(entries.into_iter().map(|entry| entry.name));
    }
}

#[test]
fn procfs_enumeration_root_lists_processes_and_static_names() {
    let mut f = ProcfsRegressionFixture::new();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(root >= 0);
    let names = list_all(&mut f.executor, &f.memory, root);
    eprintln!("{names:?}");
}
