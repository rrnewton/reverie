/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Owned proc directory records and shared OFD resume cookies.

use super::*;

const ROOT_PID_COOKIE: u64 = 258;
// The existing virtual PID ABI admits positive i32 IDs. Its root cookie uses
// that owned ID space, never a host pid_max or a supervisor task number.
const ROOT_END_COOKIE: u64 = ROOT_PID_COOKIE + i32::MAX as u64 + 1;

#[derive(Debug, Default)]
pub(super) struct DirectoryCursor {
    pub(super) position: u64,
    resume_tid: Option<i32>,
}

struct Entry {
    cookie: u64,
    inode: u64,
    kind: u8,
    name: Vec<u8>,
    task: Option<i32>,
}

struct Listing {
    entries: Vec<Entry>,
    end: u64,
    tasks: bool,
}

fn entry(directory: &procfs_path::ProcDirectory, path: &[u8], name: &[u8], kind: u8, cookie: u64) -> Entry {
    let identity = if name == b"." {
        path.to_vec()
    } else if name == b".." {
        // A proc mount's superblock root has itself as its dentry parent.
        let split = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
        if split == 0 { path.to_vec() } else { path[..split].to_vec() }
    } else { [path, b"/", name].concat() };
    let inode = directory.target.as_ref().map_or_else(|| synthetic_proc_inode(&identity),
        |target| target.inode_for_path(&identity));
    Entry { cookie, inode, kind, name: name.to_vec(), task: None }
}

fn listing(directory: &procfs_path::ProcDirectory) -> Result<Listing, i64> {
    let namespace = directory.namespace.as_ref().ok_or_else(|| negative_errno(libc::ENOSYS))?;
    let path = directory.path();
    let mut entries = vec![entry(directory, &path, b".", libc::DT_DIR, 0), entry(directory, &path, b"..", libc::DT_DIR, 1)];
    if directory.components.len() == 1 {
        // Enumerate the existing backed static surface; no fabricated payloads.
        const STATIC: &[(&[u8], u8)] = &[
            (b"cpuinfo", libc::DT_REG), (b"filesystems", libc::DT_REG),
            (b"loadavg", libc::DT_REG), (b"locks", libc::DT_REG),
            (b"meminfo", libc::DT_REG), (b"mounts", libc::DT_REG),
            (b"stat", libc::DT_REG), (b"sys", libc::DT_DIR),
            (b"uptime", libc::DT_REG), (b"version", libc::DT_REG), (b"vmstat", libc::DT_REG),
        ];
        for (index, (name, kind)) in STATIC.iter().enumerate() {
            entries.push(entry(directory, &path, name, *kind, index as u64 + 2));
        }
        entries.push(entry(directory, &path, b"self", libc::DT_LNK, 256));
        entries.push(entry(directory, &path, b"thread-self", libc::DT_LNK, 257));
        for owner in namespace.owners()? {
            let pid = owner.identity.tgid.as_raw();
            let mut item = entry(directory, &path, pid.to_string().as_bytes(), libc::DT_DIR,
                ROOT_PID_COOKIE + pid as u64);
            item.inode = procfs_path::owned_proc_inode(format!("/proc/{pid}").as_bytes(), owner.identity.generation, 0);
            entries.push(item);
        }
        entries.sort_by_key(|entry| entry.cookie);
        return Ok(Listing { entries, end: ROOT_END_COOKIE, tasks: false });
    }
    let static_children: Option<&[(&[u8], u8)]> = match path.as_slice() {
        b"/proc/sys" => Some(&[(b"kernel", libc::DT_DIR)]),
        b"/proc/sys/kernel" => Some(&[(b"osrelease", libc::DT_REG), (b"pty", libc::DT_DIR)]),
        b"/proc/sys/kernel/pty" => Some(&[]),
        _ => None,
    };
    if let Some(children) = static_children {
        for (index, (name, kind)) in children.iter().enumerate() {
            entries.push(entry(directory, &path, name, *kind, index as u64 + 2));
        }
        return Ok(Listing { end: entries.len() as u64, entries, tasks: false });
    }
    let selected = directory.target.as_ref().ok_or_else(|| negative_errno(libc::ENOENT))?;
    selected.live()?;
    match directory.components.last().map(Vec::as_slice) {
        Some(b"fd") => {
            let (fds, capacity) = selected.fd_numbers()?;
            for fd in fds {
                entries.push(entry(directory, &path, fd.to_string().as_bytes(), libc::DT_LNK, fd as u64 + 2));
            }
            Ok(Listing { entries, end: capacity + 2, tasks: false })
        }
        Some(b"fdinfo") => Err(negative_errno(libc::ENOENT)),
        Some(b"task") => {
            let owner = namespace.owner(selected.process_identity())?;
            let mut tasks: Vec<_> = owner.tasks.values().filter(|task|
                !task.exited || task.tid == owner.identity.tgid.as_raw()).collect();
            // Task generations are minted at actual admission and preserve the
            // thread-list creation order, even if a numeric TID is later reused.
            tasks.sort_by_key(|task| task.generation);
            for (index, task) in tasks.into_iter().enumerate() {
                let mut item = entry(directory, &path, task.tid.to_string().as_bytes(), libc::DT_DIR, index as u64 + 2);
                item.inode = procfs_path::owned_proc_inode(
                    format!("/proc/{}/task/{}", owner.identity.tgid.as_raw(), task.tid).as_bytes(),
                    owner.identity.generation, task.generation);
                item.task = Some(task.tid);
                entries.push(item);
            }
            Ok(Listing { end: entries.len() as u64, entries, tasks: true })
        }
        _ => {
            const PROCESS: &[(&[u8], u8)] = &[
                (b"cmdline", libc::DT_REG), (b"cwd", libc::DT_LNK),
                (b"exe", libc::DT_LNK), (b"fd", libc::DT_DIR),
                (b"fdinfo", libc::DT_DIR), (b"mountinfo", libc::DT_REG),
                (b"mounts", libc::DT_REG), (b"root", libc::DT_LNK),
                (b"stat", libc::DT_REG), (b"status", libc::DT_REG),
            ];
            for (index, (name, kind)) in PROCESS.iter().enumerate() {
                entries.push(entry(directory, &path, name, *kind, index as u64 + 2));
            }
            if directory.components.len() == 2 {
                entries.push(entry(directory, &path, b"task", libc::DT_DIR, entries.len() as u64));
            }
            Ok(Listing { end: entries.len() as u64, entries, tasks: false })
        }
    }
}

/// filldir64 ordering: range admission, previous d_off, ino, reclen, type,
/// terminating NUL, then the name. Padding and any later fault prefix survive.
fn copy_record(
    memory: &GuestMemory,
    address: u64,
    previous: Option<u64>,
    entry: &Entry,
    cookie: u64,
    length: u16,
) -> Result<(), i64> {
    let previous = previous.unwrap_or(address);
    let span = address.checked_sub(previous).and_then(|prefix| prefix.checked_add(length as u64))
        .and_then(|span| usize::try_from(span).ok()).ok_or_else(|| negative_errno(libc::EFAULT))?;
    // Numeric user-range admission only; mapped permissions are tested in the
    // actual field order, not by prevalidating the whole output record/count.
    validate_guest_iovec_address(previous, span)?;
    let user = memory.user();
    let failed = || negative_errno(libc::EFAULT);
    user.store_user_u64(previous + 8, cookie).map_err(|_| failed())?;
    user.store_user_u64(address, entry.inode).map_err(|_| failed())?;
    user.put_user_i16(address + 16, length as i16).map_err(|_| failed())?;
    if user.copy_to_user_prefix(address + 18, &[entry.kind]).map_err(|_| failed())? != 1 {
        return Err(failed());
    }
    if user.copy_to_user_prefix(address + 19 + entry.name.len() as u64, &[0]).map_err(|_| failed())? != 1 {
        return Err(failed());
    }
    if user.copy_to_user_prefix(address + 19, &entry.name).map_err(|_| failed())? != entry.name.len() {
        return Err(failed());
    }
    // Memory/retirement infrastructure refusals already publish to the entry
    // gate; this errno cannot permit continuation past the driver's recheck.
    Ok(())
}

pub(super) fn getdents(description: &FdinfoDescription, memory: &GuestMemory, address: u64, count: usize) -> i64 {
    if description.path_only { return negative_errno(libc::EBADF); }
    let SeqProcSource::Directory(directory) = &description.source else { return negative_errno(libc::ENOTDIR); };
    // Observe endpoints before the cursor guard; fdinfo observers use
    // table -> cursor, and no namespace/table lock is taken below this guard.
    let listing = match listing(directory) { Ok(listing) => listing, Err(error) => return error };
    let mut cursor = directory.cookie.lock().expect("KVM proc directory cursor poisoned");
    let resume = cursor.resume_tid.take();
    let start = if listing.tasks && cursor.position >= 2 {
        resume.and_then(|tid| listing.entries.iter().position(|entry| entry.task == Some(tid)))
            .unwrap_or_else(|| usize::try_from(cursor.position).unwrap_or(usize::MAX))
    } else {
        listing.entries.iter().position(|entry| entry.cookie >= cursor.position).unwrap_or(listing.entries.len())
    };
    let mut copied = 0usize;
    let mut previous = None;
    let mut error = 0;
    let mut ended = true;
    for entry in listing.entries.iter().skip(start) {
        let cookie = if listing.tasks && entry.task.is_some() { cursor.position.max(2) } else { entry.cookie };
        cursor.position = cookie;
        let length = (19 + entry.name.len() + 1 + 7) & !7;
        if length > count - copied {
            error = negative_errno(libc::EINVAL); ended = false;
            cursor.resume_tid = entry.task; break;
        }
        let Some(record) = address.checked_add(copied as u64) else {
            error = negative_errno(libc::EFAULT); ended = false;
            cursor.resume_tid = entry.task; break;
        };
        if let Err(fault) = copy_record(memory, record, previous, entry, cookie, length as u16) {
            error = fault; ended = false;
            cursor.resume_tid = entry.task; break;
        }
        previous = Some(record);
        copied += length;
        cursor.position = cookie + 1;
    }
    if ended && !listing.tasks && cursor.position < listing.end { cursor.position = listing.end; }
    if let Some(previous) = previous {
        if memory.user().store_user_u64(previous + 8, cursor.position).is_err() {
            return negative_errno(libc::EFAULT);
        }
        return copied as i64;
    }
    error
}

pub(super) fn seek(description: &FdinfoDescription, offset: i64, whence: i32) -> i64 {
    if description.path_only { return negative_errno(libc::EBADF); }
    let SeqProcSource::Directory(directory) = &description.source else { return negative_errno(libc::ENOTDIR); };
    let mut cursor = directory.cookie.lock().expect("KVM proc directory cursor poisoned");
    let position = match whence {
        libc::SEEK_SET | libc::SEEK_END => Some(offset), // logical directory i_size == 0
        libc::SEEK_CUR => (cursor.position as i64).checked_add(offset),
        libc::SEEK_DATA | libc::SEEK_HOLE => return negative_errno(libc::ENXIO),
        _ => None,
    };
    match position.filter(|position| *position >= 0) {
        Some(position) => {
            if position as u64 != cursor.position {
                cursor.position = position as u64;
                cursor.resume_tid = None;
            }
            position
        }
        None => negative_errno(libc::EINVAL),
    }
}
