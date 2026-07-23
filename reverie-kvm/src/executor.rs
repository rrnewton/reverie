/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::path::PathBuf;

use crate::GuestMemory;
use crate::SyscallRequest;
use crate::bootstrap::BOOT_RESERVED_END;
use crate::bootstrap::SegmentBase;
use crate::elf::LoadedStaticElf;
use crate::elf::STACK_LIMIT;
use crate::runtime::SyscallExecutor;

const MAX_HOST_IO: usize = 16 * 1024 * 1024;
const MAX_CAPTURED_OUTPUT: usize = 64 * 1024 * 1024;
const PAGE_SIZE: u64 = 4096;
const ARCH_SET_GS: u64 = 0x1001;
const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;
const ARCH_GET_GS: u64 = 0x1004;

pub(crate) enum SyscallAction {
    Continue {
        result: i64,
        segment: Option<(SegmentBase, u64)>,
    },
    Exit(i32),
}

#[derive(Default)]
pub(crate) struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CapturedOutput {
    pub(crate) fn take(&mut self) -> (Vec<u8>, Vec<u8>) {
        (
            std::mem::take(&mut self.stdout),
            std::mem::take(&mut self.stderr),
        )
    }
}

pub(crate) fn execute_basic_syscall(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    request: &SyscallRequest,
) -> SyscallAction {
    execute_basic_syscall_with_output(memory, state, request, None)
}

fn execute_basic_syscall_with_output(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    request: &SyscallRequest,
    output: Option<&mut CapturedOutput>,
) -> SyscallAction {
    let args = request.args();
    let number = request.number();

    if number == libc::SYS_exit as u64 || number == libc::SYS_exit_group as u64 {
        return SyscallAction::Exit(args[0] as i32);
    }

    let result = if number == libc::SYS_write as u64 {
        write(memory, args, output)
    } else if number == libc::SYS_read as u64 {
        read(memory, state, args)
    } else if number == libc::SYS_pread64 as u64 {
        pread64(memory, state, args)
    } else if number == libc::SYS_openat as u64 {
        openat(memory, state, args)
    } else if number == libc::SYS_fstat as u64 {
        fstat(memory, state, args)
    } else if number == libc::SYS_access as u64 {
        access(memory, state, args)
    } else if number == libc::SYS_getcwd as u64 {
        getcwd(memory, state, args)
    } else if number == libc::SYS_getdents64 as u64 {
        getdents64(memory, state, args)
    } else if number == libc::SYS_getpid as u64
        || number == libc::SYS_gettid as u64
        || number == libc::SYS_getppid as u64
    {
        1
    } else if number == libc::SYS_getuid as u64
        || number == libc::SYS_geteuid as u64
        || number == libc::SYS_getgid as u64
        || number == libc::SYS_getegid as u64
    {
        0
    } else if number == libc::SYS_arch_prctl as u64 {
        return arch_prctl(memory, state, args);
    } else if number == libc::SYS_brk as u64 {
        brk(memory, state, args[0])
    } else if number == libc::SYS_mmap as u64 {
        mmap(memory, state, args)
    } else if number == libc::SYS_munmap as u64 {
        munmap(memory, args[0], args[1])
    } else if number == libc::SYS_mprotect as u64 || number == libc::SYS_madvise as u64 {
        validate_range(memory, args[0], args[1])
    } else if number == libc::SYS_getrandom as u64 {
        getrandom(memory, args[0], args[1])
    } else if number == libc::SYS_clock_gettime as u64 {
        write_bytes(memory, args[1], &[0; 16])
    } else if number == libc::SYS_readlink as u64 {
        readlink(memory, state, args)
    } else if number == libc::SYS_uname as u64 {
        uname(memory, args[0])
    } else if number == libc::SYS_prlimit64 as u64 {
        prlimit64(memory, args)
    } else if number == libc::SYS_rt_sigaction as u64 {
        if args[2] == 0 {
            0
        } else {
            write_bytes(memory, args[2], &[0; 32])
        }
    } else if number == libc::SYS_rt_sigprocmask as u64 {
        if args[2] == 0 {
            0
        } else {
            write_bytes(memory, args[2], &[0; 8])
        }
    } else if number == libc::SYS_set_tid_address as u64 {
        1
    } else if number == libc::SYS_close as u64 {
        close(state, args[0])
    } else if number == libc::SYS_set_robust_list as u64
        || number == libc::SYS_sigaltstack as u64
        || number == libc::SYS_rseq as u64
        || number == libc::SYS_futex as u64
    {
        0
    } else {
        negative_errno(libc::ENOSYS)
    };

    continue_with(result)
}

/// A [`SyscallExecutor`] that supplies the static-ELF guest-kernel semantics
/// ([`execute_basic_syscall`]) to the tool-driven run loop
/// ([`crate::KvmBackend::run_static_elf_with_tool`]).
///
/// `execute` returns the raw syscall result and records, as side effects for
/// the run loop to apply after the tool handler completes, any pending FS/GS
/// base update (from `arch_prctl`) and the exit code (from `exit`/`exit_group`).
/// This lets a Reverie tool's `tail_inject` drive the same guest-kernel that
/// [`crate::KvmBackend::run_static_elf`] uses directly.
pub(crate) struct ElfExecutor {
    state: LoadedStaticElf,
    output: Option<CapturedOutput>,
    pending_segment: Option<(SegmentBase, u64)>,
    exit_code: Option<i32>,
}

impl ElfExecutor {
    pub(crate) fn new(state: LoadedStaticElf, capture_output: bool) -> Self {
        Self {
            state,
            output: capture_output.then(CapturedOutput::default),
            pending_segment: None,
            exit_code: None,
        }
    }

    /// Returns and clears a pending FS/GS base update requested via `arch_prctl`.
    pub(crate) fn take_segment(&mut self) -> Option<(SegmentBase, u64)> {
        self.pending_segment.take()
    }

    /// Returns and clears the exit code once the guest calls `exit`/`exit_group`.
    pub(crate) fn take_exit(&mut self) -> Option<i32> {
        self.exit_code.take()
    }

    pub(crate) fn take_output(&mut self) -> (Vec<u8>, Vec<u8>) {
        self.output
            .as_mut()
            .map(CapturedOutput::take)
            .unwrap_or_default()
    }
}

impl SyscallExecutor for ElfExecutor {
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        // Clones share the underlying MAP_SHARED mapping, so writes through this
        // handle reach the guest; `execute_basic_syscall` needs `&mut` access.
        let mut memory = memory.clone();
        match execute_basic_syscall_with_output(
            &mut memory,
            &mut self.state,
            request,
            self.output.as_mut(),
        ) {
            SyscallAction::Continue { result, segment } => {
                if segment.is_some() {
                    self.pending_segment = segment;
                }
                result
            }
            SyscallAction::Exit(code) => {
                self.exit_code = Some(code);
                0
            }
        }
    }
}

fn write(memory: &GuestMemory, args: &[u64; 6], output: Option<&mut CapturedOutput>) -> i64 {
    if args[0] != libc::STDOUT_FILENO as u64 && args[0] != libc::STDERR_FILENO as u64 {
        return negative_errno(libc::EBADF);
    }
    let Ok(length) = usize::try_from(args[2]) else {
        return negative_errno(libc::EINVAL);
    };
    if length > MAX_HOST_IO {
        return negative_errno(libc::E2BIG);
    }

    let mut bytes = vec![0; length];
    if memory.read(args[1], &mut bytes).is_err() {
        return negative_errno(libc::EFAULT);
    }

    if let Some(output) = output {
        let destination = if args[0] == libc::STDOUT_FILENO as u64 {
            &mut output.stdout
        } else {
            &mut output.stderr
        };
        if destination
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_CAPTURED_OUTPUT)
        {
            return negative_errno(libc::EFBIG);
        }
        destination.extend_from_slice(&bytes);
        return bytes.len() as i64;
    }

    // SAFETY: bytes is a live host buffer of exactly length bytes and the file
    // descriptor was restricted to stdout or stderr above.
    let written = unsafe {
        libc::write(
            args[0] as libc::c_int,
            bytes.as_ptr().cast::<libc::c_void>(),
            bytes.len(),
        )
    };
    if written < 0 {
        negative_errno(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        )
    } else {
        written as i64
    }
}

fn read(memory: &mut GuestMemory, state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    if args[0] == libc::STDIN_FILENO as u64 {
        return 0;
    }
    let Ok(fd) = i32::try_from(args[0]) else {
        return negative_errno(libc::EBADF);
    };
    let Ok(length) = usize::try_from(args[2]) else {
        return negative_errno(libc::EINVAL);
    };
    if length > MAX_HOST_IO {
        return negative_errno(libc::E2BIG);
    }
    let Some(file) = state.files.get_mut(&fd) else {
        return negative_errno(libc::EBADF);
    };
    let mut bytes = vec![0; length];
    match file.read(&mut bytes) {
        Ok(count) => match memory.write(args[1], &bytes[..count]) {
            Ok(()) => count as i64,
            Err(_) => negative_errno(libc::EFAULT),
        },
        Err(error) => io_error(error),
    }
}

fn pread64(memory: &mut GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Ok(fd) = i32::try_from(args[0]) else {
        return negative_errno(libc::EBADF);
    };
    let Ok(length) = usize::try_from(args[2]) else {
        return negative_errno(libc::EINVAL);
    };
    if length > MAX_HOST_IO {
        return negative_errno(libc::E2BIG);
    }
    if !range_is_valid(memory, args[1], args[2]) {
        return negative_errno(libc::EFAULT);
    }
    let Some(file) = state.files.get(&fd) else {
        return negative_errno(libc::EBADF);
    };
    let mut bytes = vec![0; length];
    match file.read_at(&mut bytes, args[3]) {
        Ok(count) => match memory.write(args[1], &bytes[..count]) {
            Ok(()) => count as i64,
            Err(_) => negative_errno(libc::EFAULT),
        },
        Err(error) => io_error(error),
    }
}

fn openat(memory: &GuestMemory, state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Some(path) = read_c_string(memory, args[1], 4096) else {
        return negative_errno(libc::EFAULT);
    };
    if path.is_empty() {
        return negative_errno(libc::ENOENT);
    }
    let flags = args[2] as libc::c_int;
    if flags & (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC) != 0 {
        return negative_errno(libc::EROFS);
    }
    if !path.starts_with(b"/") && args[0] as i32 != libc::AT_FDCWD {
        return negative_errno(libc::EBADF);
    }
    let path = resolve_path(state, &path);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => return io_error(error),
    };
    let fd = state.next_fd;
    state.next_fd = state.next_fd.saturating_add(1);
    state.files.insert(fd, file);
    i64::from(fd)
}

fn fstat(memory: &mut GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Ok(fd) = i32::try_from(args[0]) else {
        return negative_errno(libc::EBADF);
    };
    let host_fd = if (0..=2).contains(&fd) {
        fd
    } else if let Some(file) = state.files.get(&fd) {
        file.as_raw_fd()
    } else {
        return negative_errno(libc::EBADF);
    };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat points to writable storage and host_fd is either a standard
    // descriptor or an owned File descriptor.
    if unsafe { libc::fstat(host_fd, stat.as_mut_ptr()) } != 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: fstat initialized stat on success.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: libc::stat is plain kernel ABI data and the slice is bounded to it.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&stat).cast::<u8>(),
            std::mem::size_of::<libc::stat>(),
        )
    };
    match memory.write(args[1], bytes) {
        Ok(()) => 0,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn access(memory: &GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Some(path) = read_c_string(memory, args[0], 4096) else {
        return negative_errno(libc::EFAULT);
    };
    if resolve_path(state, &path).exists() {
        0
    } else {
        negative_errno(libc::ENOENT)
    }
}

fn getcwd(memory: &mut GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let bytes = state.cwd.as_os_str().as_bytes();
    let Ok(capacity) = usize::try_from(args[1]) else {
        return negative_errno(libc::EINVAL);
    };
    let Some(required) = bytes.len().checked_add(1) else {
        return negative_errno(libc::ERANGE);
    };
    if capacity < required {
        return negative_errno(libc::ERANGE);
    }
    let mut terminated = Vec::with_capacity(required);
    terminated.extend_from_slice(bytes);
    terminated.push(0);
    match memory.write(args[0], &terminated) {
        Ok(()) => required as i64,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn getdents64(memory: &mut GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Ok(fd) = i32::try_from(args[0]) else {
        return negative_errno(libc::EBADF);
    };
    let Ok(length) = usize::try_from(args[2]) else {
        return negative_errno(libc::EINVAL);
    };
    if length > MAX_HOST_IO {
        return negative_errno(libc::E2BIG);
    }
    if !range_is_valid(memory, args[1], args[2]) {
        return negative_errno(libc::EFAULT);
    }
    let Some(file) = state.files.get(&fd) else {
        return negative_errno(libc::EBADF);
    };
    let mut bytes = vec![0; length];
    // SAFETY: file owns a live descriptor and bytes is writable for length bytes.
    let count = unsafe {
        libc::syscall(
            libc::SYS_getdents64,
            file.as_raw_fd(),
            bytes.as_mut_ptr().cast::<libc::c_void>(),
            bytes.len(),
        )
    };
    if count < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    let count = count as usize;
    match memory.write(args[1], &bytes[..count]) {
        Ok(()) => count as i64,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn resolve_path(state: &LoadedStaticElf, bytes: &[u8]) -> PathBuf {
    let path = Path::new(OsStr::from_bytes(bytes));
    if path.is_absolute() {
        path.to_owned()
    } else {
        state.cwd.join(path)
    }
}

fn close(state: &mut LoadedStaticElf, raw_fd: u64) -> i64 {
    let Ok(fd) = i32::try_from(raw_fd) else {
        return negative_errno(libc::EBADF);
    };
    if (0..=2).contains(&fd) || state.files.remove(&fd).is_some() {
        0
    } else {
        negative_errno(libc::EBADF)
    }
}

fn arch_prctl(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    args: &[u64; 6],
) -> SyscallAction {
    match args[0] {
        ARCH_SET_FS | ARCH_SET_GS if args[1] < memory.guest_end() => {
            let (base, segment) = if args[0] == ARCH_SET_FS {
                state.fs_base = args[1];
                (state.fs_base, SegmentBase::Fs)
            } else {
                state.gs_base = args[1];
                (state.gs_base, SegmentBase::Gs)
            };
            SyscallAction::Continue {
                result: 0,
                segment: Some((segment, base)),
            }
        }
        ARCH_SET_FS | ARCH_SET_GS => continue_with(negative_errno(libc::EPERM)),
        ARCH_GET_FS => continue_with(write_u64(memory, args[1], state.fs_base)),
        ARCH_GET_GS => continue_with(write_u64(memory, args[1], state.gs_base)),
        _ => continue_with(negative_errno(libc::EINVAL)),
    }
}

fn brk(memory: &mut GuestMemory, state: &mut LoadedStaticElf, requested: u64) -> i64 {
    if requested == 0 {
        return state.program_break as i64;
    }
    if requested < BOOT_RESERVED_END || requested >= state.brk_limit {
        return state.program_break as i64;
    }
    if requested > state.program_break {
        let Ok(length) = usize::try_from(requested - state.program_break) else {
            return state.program_break as i64;
        };
        if memory.zero(state.program_break, length).is_err() {
            return state.program_break as i64;
        }
    }
    state.program_break = requested;
    requested as i64
}

fn mmap(memory: &mut GuestMemory, state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    if args[1] == 0 {
        return negative_errno(libc::EINVAL);
    }
    let flags = args[3];
    let is_anonymous = flags & libc::MAP_ANONYMOUS as u64 != 0;
    let is_private = flags & libc::MAP_PRIVATE as u64 != 0;
    let is_shared = flags & libc::MAP_SHARED as u64 != 0;
    if !is_private && !is_shared {
        return negative_errno(libc::EINVAL);
    }

    let Some(length) = align_up(args[1], PAGE_SIZE) else {
        return negative_errno(libc::ENOMEM);
    };
    let fixed = flags & libc::MAP_FIXED as u64 != 0;
    if fixed && !args[0].is_multiple_of(PAGE_SIZE) {
        return negative_errno(libc::EINVAL);
    }
    if !is_anonymous && !args[5].is_multiple_of(PAGE_SIZE) {
        return negative_errno(libc::EINVAL);
    }
    // Linux treats a nonfixed address as a hint. This bounded personality uses
    // its deterministic allocator rather than risking an occupied mapping.
    let address = if fixed { args[0] } else { state.mmap_next };
    let Some(end) = address.checked_add(length) else {
        return negative_errno(libc::ENOMEM);
    };
    if address < BOOT_RESERVED_END || end > state.mmap_limit {
        return negative_errno(libc::ENOMEM);
    }
    let Ok(length) = usize::try_from(length) else {
        return negative_errno(libc::ENOMEM);
    };
    let file_bytes = if !is_anonymous {
        let Ok(fd) = i32::try_from(args[4]) else {
            return negative_errno(libc::EBADF);
        };
        let Some(file) = state.files.get(&fd) else {
            return negative_errno(libc::EBADF);
        };
        let mut bytes = vec![0; length];
        let mut count = 0;
        while count < length {
            match file.read_at(&mut bytes[count..], args[5].saturating_add(count as u64)) {
                Ok(0) => break,
                Ok(read) => count += read,
                Err(error) => return io_error(error),
            }
        }
        Some(bytes)
    } else if args[4] as i32 != -1 {
        return negative_errno(libc::EINVAL);
    } else {
        None
    };

    if memory.zero(address, length).is_err() {
        return negative_errno(libc::ENOMEM);
    }
    if let Some(bytes) = file_bytes
        && memory.write(address, &bytes).is_err()
    {
        return negative_errno(libc::EFAULT);
    }

    if !fixed {
        state.mmap_next = end;
    }
    address as i64
}

fn munmap(memory: &mut GuestMemory, address: u64, length: u64) -> i64 {
    let Some(length) = align_up(length, PAGE_SIZE) else {
        return negative_errno(libc::EINVAL);
    };
    if address < BOOT_RESERVED_END
        || !address.is_multiple_of(PAGE_SIZE)
        || length == 0
        || !range_is_valid(memory, address, length)
    {
        return negative_errno(libc::EINVAL);
    }
    let Ok(length) = usize::try_from(length) else {
        return negative_errno(libc::EINVAL);
    };
    match memory.zero(address, length) {
        Ok(()) => 0,
        Err(_) => negative_errno(libc::EINVAL),
    }
}

fn validate_range(memory: &GuestMemory, address: u64, length: u64) -> i64 {
    if length == 0 || !range_is_valid(memory, address, length) {
        negative_errno(libc::EINVAL)
    } else {
        0
    }
}

fn getrandom(memory: &mut GuestMemory, address: u64, length: u64) -> i64 {
    let Ok(length) = usize::try_from(length) else {
        return negative_errno(libc::EINVAL);
    };
    if length > MAX_HOST_IO {
        return negative_errno(libc::E2BIG);
    }
    let bytes: Vec<u8> = (0..length)
        .map(|index| (index as u8).wrapping_mul(17).wrapping_add(0x5a))
        .collect();
    match memory.write(address, &bytes) {
        Ok(()) => length as i64,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn readlink(memory: &mut GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Some(path) = read_c_string(memory, args[0], 4096) else {
        return negative_errno(libc::EFAULT);
    };
    if path != b"/proc/self/exe" {
        return negative_errno(libc::ENOENT);
    }
    let Ok(capacity) = usize::try_from(args[2]) else {
        return negative_errno(libc::EINVAL);
    };
    if capacity == 0 {
        return negative_errno(libc::EINVAL);
    }

    let count = capacity.min(state.argv0.len());
    match memory.write(args[1], &state.argv0[..count]) {
        Ok(()) => count as i64,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn uname(memory: &mut GuestMemory, address: u64) -> i64 {
    let mut utsname = [0; 65 * 6];
    for (index, value) in [
        b"Linux".as_slice(),
        b"reverie-kvm".as_slice(),
        b"6.0.0".as_slice(),
        b"#1".as_slice(),
        b"x86_64".as_slice(),
        b"(none)".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let start = index * 65;
        utsname[start..start + value.len()].copy_from_slice(value);
    }
    write_bytes(memory, address, &utsname)
}

fn prlimit64(memory: &mut GuestMemory, args: &[u64; 6]) -> i64 {
    if args[2] != 0 {
        return negative_errno(libc::EPERM);
    }
    if args[3] == 0 {
        return 0;
    }
    let limit = STACK_LIMIT;
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&limit.to_le_bytes());
    bytes[8..].copy_from_slice(&limit.to_le_bytes());
    write_bytes(memory, args[3], &bytes)
}

fn write_u64(memory: &mut GuestMemory, address: u64, value: u64) -> i64 {
    write_bytes(memory, address, &value.to_le_bytes())
}

fn write_bytes(memory: &mut GuestMemory, address: u64, bytes: &[u8]) -> i64 {
    match memory.write(address, bytes) {
        Ok(()) => 0,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

fn read_c_string(memory: &GuestMemory, address: u64, limit: usize) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    for offset in 0..limit {
        let mut byte = [0];
        memory
            .read(address.checked_add(offset as u64)?, &mut byte)
            .ok()?;
        if byte[0] == 0 {
            return Some(result);
        }
        result.push(byte[0]);
    }
    None
}

fn range_is_valid(memory: &GuestMemory, address: u64, length: u64) -> bool {
    address >= memory.guest_base()
        && address
            .checked_add(length)
            .is_some_and(|end| end <= memory.guest_end())
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn continue_with(result: i64) -> SyscallAction {
    SyscallAction::Continue {
        result,
        segment: None,
    }
}

fn io_error(error: std::io::Error) -> i64 {
    negative_errno(error.raw_os_error().unwrap_or(libc::EIO))
}

const fn negative_errno(errno: libc::c_int) -> i64 {
    -(errno as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_getrandom_repeats() {
        let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();

        assert_eq!(getrandom(&mut memory, 0x100, 32), 32);
        let mut first = [0; 32];
        memory.read(0x100, &mut first).unwrap();

        assert_eq!(getrandom(&mut memory, 0x200, 32), 32);
        let mut second = [0; 32];
        memory.read(0x200, &mut second).unwrap();
        assert_eq!(first, second);
    }
}
