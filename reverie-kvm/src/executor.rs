/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::GuestMemory;
use crate::SyscallRequest;
use crate::bootstrap::BOOT_RESERVED_END;
use crate::bootstrap::SegmentBase;
use crate::elf::LoadedStaticElf;

const MAX_HOST_IO: usize = 16 * 1024 * 1024;
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

pub(crate) fn execute_basic_syscall(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    request: &SyscallRequest,
) -> SyscallAction {
    let args = request.args();
    let number = request.number();

    if number == libc::SYS_exit as u64 || number == libc::SYS_exit_group as u64 {
        return SyscallAction::Exit(args[0] as i32);
    }

    let result = if number == libc::SYS_write as u64 {
        write(memory, args)
    } else if number == libc::SYS_read as u64 {
        if args[0] == libc::STDIN_FILENO as u64 {
            0
        } else {
            negative_errno(libc::EBADF)
        }
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
    } else if number == libc::SYS_set_robust_list as u64
        || number == libc::SYS_sigaltstack as u64
        || number == libc::SYS_close as u64
    {
        0
    } else {
        negative_errno(libc::ENOSYS)
    };

    continue_with(result)
}

fn write(memory: &GuestMemory, args: &[u64; 6]) -> i64 {
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
    if requested < BOOT_RESERVED_END || requested >= state.mmap_next {
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
    let flags = args[3];
    let supported = libc::MAP_PRIVATE as u64 | libc::MAP_ANONYMOUS as u64;
    if args[0] != 0
        || args[1] == 0
        || flags & supported != supported
        || flags & libc::MAP_FIXED as u64 != 0
        || args[4] != u64::MAX
        || args[5] != 0
    {
        return negative_errno(libc::EINVAL);
    }

    let Some(length) = align_up(args[1], PAGE_SIZE) else {
        return negative_errno(libc::ENOMEM);
    };
    let address = state.mmap_next;
    let Some(end) = address.checked_add(length) else {
        return negative_errno(libc::ENOMEM);
    };
    if end > state.mmap_limit {
        return negative_errno(libc::ENOMEM);
    }
    let Ok(length) = usize::try_from(length) else {
        return negative_errno(libc::ENOMEM);
    };
    if memory.zero(address, length).is_err() {
        return negative_errno(libc::ENOMEM);
    }
    state.mmap_next = end;
    address as i64
}

fn munmap(memory: &mut GuestMemory, address: u64, length: u64) -> i64 {
    if length == 0 || !range_is_valid(memory, address, length) {
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
    let limit = 8_u64 * 1024 * 1024;
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
