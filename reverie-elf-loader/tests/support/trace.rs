/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::arch::x86_64::__cpuid;
use std::arch::x86_64::__cpuid_count;
use std::arch::x86_64::_xgetbv;
use std::collections::BTreeMap;
use std::fs::File;
use std::fs::{self};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::time::Duration;
use std::time::Instant;

use super::Artifacts;
use super::Case;
use super::DIRECTORY_FD;
use super::METADATA_FD;
use super::TARGET_FD;
use super::TestResult;
use super::io as convert_io;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Registers {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub mxcsr: u32,
    pub x87_control: u16,
    pub xmm: Vec<u8>,
    pub feature_mask: u64,
    pub xcr0: u64,
    pub extended: BTreeMap<&'static str, Vec<u8>>,
}

#[derive(Debug)]
pub(super) struct Entry {
    pub registers: Registers,
    pub stack_start: u64,
    pub stack: Vec<u8>,
    pub aux: Vec<(u64, u64)>,
    pub proc_aux: Vec<(u64, u64)>,
    pub stat: BTreeMap<u32, u64>,
    pub maps: Vec<u8>,
    pub exe: Vec<u8>,
    pub comm: Vec<u8>,
    pub cmdline: Vec<u8>,
}

impl Entry {
    pub fn execfn(&self) -> &[u8] {
        let pointer = self
            .aux
            .iter()
            .find(|entry| entry.0 == 31)
            .expect("AT_EXECFN exists")
            .1;
        let bytes = &self.stack[(pointer - self.stack_start) as usize..];
        &bytes[..bytes
            .iter()
            .position(|byte| *byte == 0)
            .expect("AT_EXECFN is terminated")]
    }
}

pub(super) struct Run {
    pub entry: Entry,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub origin: KernelOrigin,
    pub record_inode: Option<u64>,
    pub exit: i32,
    pub peak_data_kib: u64,
    pub elapsed: Duration,
}

pub(super) struct KernelOrigin {
    pub proc_aux: Vec<(u64, u64)>,
    pub stat: BTreeMap<u32, u64>,
}

pub(super) fn run(case: &Case, artifacts: Option<&Artifacts>) -> TestResult<Run> {
    let start = Instant::now();
    let mode = if artifacts.is_some() {
        "loader"
    } else {
        "native"
    };
    let stdout_path = case.root.join(format!("{mode}.stdout"));
    let stderr_path = case.root.join(format!("{mode}.stderr"));
    let stdout = convert_io(File::create(&stdout_path))?;
    let stderr = convert_io(File::create(&stderr_path))?;
    let mut argv = case
        .argv
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    argv.push(std::ptr::null());
    let mut env = case
        .env
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    env.push(std::ptr::null());
    let path = artifacts.map_or(case.invocation.path(), |value| {
        value.prepared.padded_path.as_c_str()
    });
    let target_fd = case.target.as_raw_fd();
    let metadata_fd = artifacts.map(|value| value.metadata.as_raw_fd());
    let directory_fd = case.directory.as_ref().map(AsRawFd::as_raw_fd);
    let output_fd = stdout.as_raw_fd();
    let error_fd = stderr.as_raw_fd();
    // All pointers and descriptors are prepared before fork. The child below
    // allocates nothing, acquires no Rust lock, and only calls syscall wrappers.
    // SAFETY: the child does not access the inherited allocator or mutex state.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    if pid == 0 {
        // SAFETY: these pointers refer to inherited, live allocations; calls
        // below either return immediately or terminate/replace this child.
        unsafe {
            if libc::dup2(target_fd, TARGET_FD) < 0
                || libc::dup2(output_fd, 1) < 0
                || libc::dup2(error_fd, 2) < 0
            {
                child_error(b"descriptor setup failed\n");
            }
            if let Some(fd) = metadata_fd
                && (libc::dup2(fd, METADATA_FD) < 0
                    || libc::fcntl(METADATA_FD, libc::F_SETFD, 0) < 0)
            {
                child_error(b"metadata setup failed\n");
            }
            if let Some(fd) = directory_fd
                && (libc::dup2(fd, DIRECTORY_FD) < 0
                    || libc::fcntl(DIRECTORY_FD, libc::F_SETFD, libc::FD_CLOEXEC) < 0)
            {
                child_error(b"directory descriptor setup failed\n");
            }
            let close_on_exec = if artifacts.is_none() {
                libc::FD_CLOEXEC
            } else {
                0
            };
            if libc::fcntl(TARGET_FD, libc::F_SETFD, close_on_exec) < 0
                || libc::chdir(case.cwd.as_ptr()) < 0
            {
                child_error(b"launch setup failed\n");
            }
            let current = libc::personality(!0 as libc::c_ulong);
            if current < 0
                || libc::personality(
                    current as libc::c_ulong | libc::ADDR_NO_RANDOMIZE as libc::c_ulong,
                ) < 0
                || libc::personality(!0 as libc::c_ulong) & libc::ADDR_NO_RANDOMIZE == 0
            {
                child_error(b"ADDR_NO_RANDOMIZE could not be installed (required; not skipped)\n");
            }
            let data = libc::rlimit {
                rlim_cur: case.limits.data,
                rlim_max: case.limits.data,
            };
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_DATA, &data) < 0
                || libc::setrlimit(libc::RLIMIT_CORE, &core) < 0
            {
                child_error(b"RLIMIT_DATA setup failed\n");
            }
            libc::alarm(30);
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) < 0 {
                child_error(b"PTRACE_TRACEME failed (required; not skipped)\n");
            }
            libc::kill(libc::getpid(), libc::SIGSTOP);
            if artifacts.is_some()
                || (case.invocation.dirfd() == libc::AT_FDCWD && case.invocation.flags() == 0)
            {
                libc::execve(path.as_ptr(), argv.as_ptr(), env.as_ptr());
            } else {
                libc::syscall(
                    libc::SYS_execveat,
                    case.invocation.dirfd(),
                    path.as_ptr(),
                    argv.as_ptr(),
                    env.as_ptr(),
                    case.invocation.flags(),
                );
            }
            child_error(b"native or loader exec syscall failed\n");
        }
    }
    let mut child = Child(pid);
    let first = wait(pid)?;
    if !libc::WIFSTOPPED(first) || libc::WSTOPSIG(first) != libc::SIGSTOP {
        if libc::WIFEXITED(first) || libc::WIFSIGNALED(first) {
            child.0 = 0;
        }
        return Err(format!(
            "{mode} child did not reach its ptrace stop: {}",
            diagnostics(&stderr_path)
        ));
    }
    let options = libc::PTRACE_O_TRACEEXEC
        | libc::PTRACE_O_TRACESYSGOOD
        | libc::PTRACE_O_TRACEEXIT
        | libc::PTRACE_O_EXITKILL;
    ptrace(libc::PTRACE_SETOPTIONS, pid, 0, options as usize)?;
    ptrace(libc::PTRACE_SYSCALL, pid, 0, 0)?;
    let mut exec_seen = false;
    let mut entry = None;
    let mut breakpoint = None;
    let mut peak_data_kib = 0;
    let mut memory = None;
    let mut origin = None;
    let mut record_inode = None;
    let exit = loop {
        let status = wait(pid)?;
        if libc::WIFEXITED(status) {
            break libc::WEXITSTATUS(status);
        }
        if libc::WIFSIGNALED(status) {
            break 128 + libc::WTERMSIG(status);
        }
        if !libc::WIFSTOPPED(status) {
            return Err(format!("unexpected wait status {status:#x}"));
        }
        let signal = libc::WSTOPSIG(status);
        let event = status >> 16;
        if event == libc::PTRACE_EVENT_EXEC {
            exec_seen = true;
            memory = Some(convert_io(File::open(format!("/proc/{pid}/mem")))?);
            origin = Some(KernelOrigin {
                proc_aux: decode_aux(&convert_io(fs::read(format!("/proc/{pid}/auxv")))?)?,
                stat: read_stat(&convert_io(fs::read_to_string(format!(
                    "/proc/{pid}/stat"
                )))?)?,
            });
            let registers = get_regs(pid)?;
            seed_random(
                pid,
                memory.as_ref().expect("memory was opened"),
                registers.rsp,
            )?;
            if artifacts.is_none() {
                // The exec event precedes the successful syscall return:
                // x86-64 still exposes the tracer's provisional -ENOSYS RAX.
                // Capture the actual first instruction with the same restored
                // INT3 witness used for the loader; do not alter any register.
                let address = registers.rip;
                let original = ptrace(libc::PTRACE_PEEKDATA, pid, address, 0)? as u64;
                ptrace(
                    libc::PTRACE_POKEDATA,
                    pid,
                    address,
                    ((original & !0xff) | 0xcc) as usize,
                )?;
                breakpoint = Some((address, original));
            }
        }
        if exec_seen {
            peak_data_kib = peak_data_kib.max(vm_data(pid)?);
        }
        if artifacts.is_some() && entry.is_none() && signal == (libc::SIGTRAP | 0x80) {
            let registers = get_regs(pid)?;
            if registers.orig_rax == libc::SYS_mmap as u64
                && registers.rax == 0x200000
                && registers.rdi == 0x200000
                && registers.rdx == 3
                && registers.r10 == 0x100021
            {
                let maps = convert_io(fs::read(format!("/proc/{pid}/maps")))?;
                record_inode = Some(super::compare::initial_record_inode(&maps, registers.rsi)?);
            }
            if registers.orig_rax == libc::SYS_mprotect as u64
                && registers.rax == 0
                && registers.rdi == 0x200000
                && registers.rsi == 4096
                && registers.rdx == 1
            {
                let address = read_u64(memory.as_ref().expect("exec memory is open"), 0x200008)?;
                let original = ptrace(libc::PTRACE_PEEKDATA, pid, address, 0)? as u64;
                ptrace(
                    libc::PTRACE_POKEDATA,
                    pid,
                    address,
                    ((original & !0xff) | 0xcc) as usize,
                )?;
                breakpoint = Some((address, original));
            }
        }
        if signal == libc::SIGTRAP && event == 0 {
            let (address, original) = breakpoint
                .take()
                .ok_or_else(|| "unexpected SIGTRAP".to_string())?;
            let mut registers = get_regs(pid)?;
            if registers.rip != address + 1 {
                return Err(format!(
                    "interpreter breakpoint hit at {:#x}, expected {address:#x}",
                    registers.rip
                ));
            }
            ptrace(libc::PTRACE_POKEDATA, pid, address, original as usize)?;
            registers.rip = address;
            ptrace(
                libc::PTRACE_SETREGS,
                pid,
                0,
                (&mut registers as *mut libc::user_regs_struct) as usize,
            )?;
            entry = Some(capture(pid, memory.as_ref().expect("exec memory is open"))?);
        }
        let deliver = if signal == libc::SIGTRAP || signal == (libc::SIGTRAP | 0x80) {
            0
        } else {
            signal
        };
        ptrace(libc::PTRACE_SYSCALL, pid, 0, deliver as usize)?;
    };
    child.0 = 0;
    if !exec_seen {
        return Err(format!(
            "{mode} exec failed before the exec event (status{exit}): {}",
            diagnostics(&stderr_path)
        ));
    }
    let entry = entry.ok_or_else(|| {
        format!(
            "{mode} did not reach the interpreter entry (status{exit}): {}",
            diagnostics(&stderr_path)
        )
    })?;
    Ok(Run {
        entry,
        stdout: convert_io(fs::read(stdout_path))?,
        stderr: convert_io(fs::read(stderr_path))?,
        origin: origin.ok_or("exec origin witness missing")?,
        record_inode,
        exit,
        peak_data_kib,
        elapsed: start.elapsed(),
    })
}

// SAFETY: invoked only in the fork child, with a static valid byte slice.
unsafe fn child_error(message: &'static [u8]) -> ! {
    unsafe {
        libc::write(2, message.as_ptr().cast(), message.len());
        libc::_exit(126);
    }
}

struct Child(libc::pid_t);

impl Drop for Child {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        // SAFETY: this guard owns this traced child and no other process.
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
        while let Ok(status) = wait(self.0) {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                break;
            }
            let _ = ptrace(libc::PTRACE_CONT, self.0, 0, libc::SIGKILL as usize);
        }
    }
}

fn wait(pid: libc::pid_t) -> TestResult<i32> {
    let mut status = 0;
    loop {
        // SAFETY: status is writable, and pid names our child.
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return Ok(status);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
    }
}

fn ptrace(
    request: libc::c_uint,
    pid: libc::pid_t,
    address: u64,
    data: usize,
) -> TestResult<libc::c_long> {
    // PEEKDATA may legitimately return -1; errno distinguishes that value.
    // SAFETY: all callers pass the request's documented argument form and own
    // this stopped traced child. Pointer outputs remain live through the call.
    let (result, errno) = unsafe {
        *libc::__errno_location() = 0;
        let result = libc::ptrace(
            request,
            pid,
            address as *mut libc::c_void,
            data as *mut libc::c_void,
        );
        (result, *libc::__errno_location())
    };
    if result == -1 && errno != 0 {
        Err(format!(
            "ptrace request{request} failed: {}",
            io::Error::from_raw_os_error(errno)
        ))
    } else {
        Ok(result)
    }
}

fn get_regs(pid: libc::pid_t) -> TestResult<libc::user_regs_struct> {
    let mut registers = MaybeUninit::<libc::user_regs_struct>::uninit();
    ptrace(
        libc::PTRACE_GETREGS,
        pid,
        0,
        registers.as_mut_ptr() as usize,
    )?;
    // SAFETY: successful GETREGS initialized the full kernel-defined structure.
    Ok(unsafe { registers.assume_init() })
}

fn read_u64(memory: &File, address: u64) -> TestResult<u64> {
    let mut bytes = [0; 8];
    convert_io(memory.read_exact_at(&mut bytes, address))?;
    Ok(u64::from_le_bytes(bytes))
}

fn stack_aux(memory: &File, sp: u64) -> TestResult<Vec<(u64, u64)>> {
    let argc = read_u64(memory, sp)?;
    if argc > 4096 {
        return Err(format!("unreasonable initial argc{argc}"));
    }
    let mut pointer = sp + 8 * (argc + 2);
    for _ in 0..4096 {
        let word = read_u64(memory, pointer)?;
        pointer += 8;
        if word == 0 {
            let mut result = Vec::new();
            for _ in 0..128 {
                let kind = read_u64(memory, pointer)?;
                let value = read_u64(memory, pointer + 8)?;
                pointer += 16;
                result.push((kind, value));
                if kind == 0 {
                    return Ok(result);
                }
            }
            return Err("initial auxv lacks AT_NULL within128 entries".into());
        }
    }
    Err("initial envp lacks a terminator within4096 entries".into())
}

fn seed_random(pid: libc::pid_t, memory: &File, sp: u64) -> TestResult {
    let aux = stack_aux(memory, sp)?;
    let address = aux
        .iter()
        .find(|value| value.0 == 25)
        .ok_or("AT_RANDOM missing")?
        .1;
    for (index, bytes) in b"ELFloader-seed01".as_chunks::<8>().0.iter().enumerate() {
        ptrace(
            libc::PTRACE_POKEDATA,
            pid,
            address + (index * 8) as u64,
            u64::from_le_bytes(*bytes) as usize,
        )?;
    }
    Ok(())
}

fn read_string(memory: &File, mut address: u64) -> TestResult<Vec<u8>> {
    let mut result = Vec::new();
    while result.len() < 65536 {
        let length = ((4096 - address % 4096) as usize).min(256);
        let mut bytes = vec![0; length];
        convert_io(memory.read_exact_at(&mut bytes, address))?;
        if let Some(end) = bytes.iter().position(|value| *value == 0) {
            result.extend_from_slice(&bytes[..end]);
            return Ok(result);
        }
        result.extend_from_slice(&bytes);
        address += length as u64;
    }
    Err("initial string has no terminator within65536 bytes".into())
}

fn capture(pid: libc::pid_t, memory: &File) -> TestResult<Entry> {
    let registers = registers(pid)?;
    let sp = registers.gpr[7];
    let aux = stack_aux(memory, sp)?;
    let execfn = aux
        .iter()
        .find(|value| value.0 == 31)
        .ok_or("AT_EXECFN missing")?
        .1;
    let execfn_length = read_string(memory, execfn)?.len();
    let end = (execfn + execfn_length as u64 + 1 + 4095) & !4095;
    let stack_start = sp - 1024;
    let stack_length = end
        .checked_sub(stack_start)
        .ok_or("AT_EXECFN is below initial RSP")?;
    if stack_length > 1024 * 1024 {
        return Err("initial stack witness exceeds1MiB".into());
    }
    let mut stack = vec![0; stack_length as usize];
    convert_io(memory.read_exact_at(&mut stack, stack_start))?;
    let proc_aux = decode_aux(&convert_io(fs::read(format!("/proc/{pid}/auxv")))?)?;
    let stat = read_stat(&convert_io(fs::read_to_string(format!(
        "/proc/{pid}/stat"
    )))?)?;
    let exe = convert_io(fs::read_link(format!("/proc/{pid}/exe")))?;
    use std::os::unix::ffi::OsStrExt;
    Ok(Entry {
        registers,
        stack_start,
        stack,
        aux,
        proc_aux,
        stat,
        maps: convert_io(fs::read(format!("/proc/{pid}/maps")))?,
        exe: exe.as_os_str().as_bytes().to_vec(),
        comm: convert_io(fs::read(format!("/proc/{pid}/comm")))?,
        cmdline: convert_io(fs::read(format!("/proc/{pid}/cmdline")))?,
    })
}

pub(super) fn decode_aux(bytes: &[u8]) -> TestResult<Vec<(u64, u64)>> {
    if !bytes.len().is_multiple_of(16) {
        return Err("proc auxv is not an ELF64 vector".into());
    }
    let values = bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|entry| {
            (
                u64::from_le_bytes(entry[..8].try_into().expect("eight bytes")),
                u64::from_le_bytes(entry[8..].try_into().expect("eight bytes")),
            )
        })
        .collect::<Vec<_>>();
    if values.last() != Some(&(0, 0)) {
        return Err("proc auxv lacks its exact AT_NULL terminator".into());
    }
    Ok(values)
}

fn read_stat(text: &str) -> TestResult<BTreeMap<u32, u64>> {
    let close = text
        .rfind(") ")
        .ok_or("proc stat comm terminator missing")?;
    let mut result = BTreeMap::new();
    for (index, value) in text[close + 2..].split_whitespace().enumerate() {
        let field = index as u32 + 3;
        if (26..=28).contains(&field) || (45..=51).contains(&field) {
            result.insert(
                field,
                value.parse::<u64>().map_err(|error| error.to_string())?,
            );
        }
    }
    if result.len() != 10 {
        return Err("proc stat lacks required layout fields".into());
    }
    Ok(result)
}

fn vm_data(pid: libc::pid_t) -> TestResult<u64> {
    let status = convert_io(fs::read_to_string(format!("/proc/{pid}/status")))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmData:"))
        .ok_or_else(|| "VmData missing while traced image is alive".to_string())?
        .split_whitespace()
        .next()
        .ok_or("VmData value missing")?
        .parse::<u64>()
        .map_err(|error| error.to_string())
}

fn registers(pid: libc::pid_t) -> TestResult<Registers> {
    let g = get_regs(pid)?;
    let mut floating = MaybeUninit::<libc::user_fpregs_struct>::uninit();
    ptrace(
        libc::PTRACE_GETFPREGS,
        pid,
        0,
        floating.as_mut_ptr() as usize,
    )?;
    // SAFETY: successful GETFPREGS initialized the complete output structure.
    let floating = unsafe { floating.assume_init() };
    let xmm = floating
        .xmm_space
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let mut extended = BTreeMap::new();
    let mut feature_mask = 0;
    let cpu = __cpuid(1);
    let xcr0 = if cpu.ecx & (1 << 27) != 0 {
        // SAFETY: OSXSAVE advertises XGETBV support and XCR0 is a valid index.
        unsafe { _xgetbv(0) }
    } else {
        0
    };
    let features = __cpuid_count(7, 0);
    let avx = cpu.ecx & (1 << 28) != 0 && xcr0 & 6 == 6;
    let avx512 = features.ebx & (1 << 16) != 0 && xcr0 & 0xe6 == 0xe6;
    let pkru = features.ecx & (1 << 4) != 0 && xcr0 & (1 << 9) != 0;
    if avx || avx512 || pkru {
        let mut bytes = vec![0; 65536];
        let mut vector = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        ptrace(
            libc::PTRACE_GETREGSET,
            pid,
            0x202,
            (&mut vector as *mut libc::iovec) as usize,
        )?;
        bytes.truncate(vector.iov_len);
        for (enabled, component, length, name) in [
            (avx, 2, 256, "ymm_upper"),
            (avx512, 5, 64, "opmask"),
            (avx512, 6, 512, "zmm_upper"),
            (avx512, 7, 1024, "zmm16_31"),
            (pkru, 9, 4, "pkru"),
        ] {
            if enabled {
                let offset = __cpuid_count(0xd, component).ebx as usize;
                let component = bytes
                    .get(offset..offset + length)
                    .ok_or_else(|| format!("ptrace XSAVE does not expose advertised {name}"))?;
                extended.insert(name, component.to_vec());
            }
        }
        let wide_opmask = avx512 && features.ebx & (1 << 30) != 0;
        feature_mask = u64::from(avx)
            | (u64::from(avx512) << 1)
            | (u64::from(pkru) << 2)
            | (u64::from(wide_opmask) << 3);
    }
    Ok(Registers {
        gpr: [
            g.rax, g.rbx, g.rcx, g.rdx, g.rsi, g.rdi, g.rbp, g.rsp, g.r8, g.r9, g.r10, g.r11,
            g.r12, g.r13, g.r14, g.r15,
        ],
        rip: g.rip,
        rflags: g.eflags,
        mxcsr: floating.mxcsr,
        x87_control: floating.cwd,
        xmm,
        feature_mask,
        xcr0,
        extended,
    })
}

fn diagnostics(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| error.to_string())
}
