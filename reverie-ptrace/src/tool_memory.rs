/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The guest memory a Tool writes through.
//!
//! [`Stopped`]'s generic `write` sends an eight-byte buffer through
//! `PTRACE_POKEDATA`, which ignores page protection. That is the debugger
//! contract the GDB server and Reverie's own setup rely on. A Tool, though,
//! writes the results of the syscalls it emulates into guest-chosen output
//! pointers, and Linux fails such a write with `EFAULT` when the page is not
//! writable. So every Tool write goes through the permission-respecting path
//! instead, at every size. A Tool write that overlaps Reverie's private page
//! is refused before any access: that page is read-and-execute, so a native
//! write there fails too, and a forced one could replace the `ud2` that
//! guards the untraced stub (<https://github.com/rrnewton/reverie/issues/1064>).
//!
//! A scalar the kernel would publish with `put_user` (at most eight bytes) is
//! one store: on a fault nothing is written. A copy that crosses into a
//! non-writable page would instead leave its writable prefix behind, so
//! [`ToolMemory::write_value`] checks every page of such a scalar first.

use std::io;
use std::mem;

use reverie::Errno;
use reverie::Pid;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::RemoteIoVec;
use safeptrace::Stopped;

use crate::cp;

/// A stopped task's memory as a Tool sees it. Reads are [`Stopped`]'s; every
/// write respects the target's page permissions and never touches Reverie's
/// private page. Once the notifier publishes an exit stop that supersedes
/// this held non-exit capability, accesses fail with `ESRCH` without touching
/// the exit stop its original owner must handle.
pub struct ToolMemory(Stopped);

impl ToolMemory {
    pub(crate) fn new(stopped: Stopped) -> Self {
        Self(stopped)
    }

    pub(crate) fn pid(&self) -> Pid {
        self.0.pid()
    }

    #[cfg(test)]
    pub(crate) fn stopped(&self) -> &Stopped {
        &self.0
    }

    fn check_held_stop(&self) -> Result<(), Errno> {
        // Memory remains readable at PTRACE_EVENT_EXIT. A callback polling
        // it through the old stop can otherwise keep the EXIT owner from
        // progressing (https://github.com/rrnewton/reverie/issues/1091).
        // This passive publication check makes no ptrace/procfs request and
        // does not claim or advance the exit stop. Real Exec and FIFO-routed
        // second exits reset the fact; synchronous waits never publish it.
        // Before publication (including a legacy callback blocking its own
        // next wait poll), it proves nothing. An access can race publication
        // or reset; this does not make check-and-access atomic.
        if self.0.superseded_by_observed_exit_stop() {
            Err(Errno::ESRCH)
        } else {
            Ok(())
        }
    }

    /// Whether every page of `[address, address + length)` is mapped writable.
    fn pages_writable(&self, address: usize, length: usize) -> Result<bool, Errno> {
        let end = address.checked_add(length).ok_or(Errno::EFAULT)?;
        let maps = procfs::process::Process::new(self.0.pid().as_raw())
            .and_then(|process| process.maps())
            .map_err(|_| Errno::EFAULT)?;
        let mut page = address & !(PAGE_SIZE - 1);
        while page < end {
            let page_start = page as u64;
            let writable = maps.iter().any(|map| {
                map.address.0 <= page_start
                    && page_start < map.address.1
                    && map.perms.contains(procfs::process::MMPermissions::WRITE)
            });
            if !writable {
                return Ok(false);
            }
            page += PAGE_SIZE;
        }
        Ok(true)
    }
}

const PAGE_SIZE: usize = 4096;
/// The largest value the kernel publishes with one `put_user` store.
const PUT_USER_MAX: usize = 8;

fn crosses_page(address: usize, length: usize) -> bool {
    length > 1 && (address & (PAGE_SIZE - 1)) + length > PAGE_SIZE
}

/// Whether `[address, address + length)` overlaps Reverie's private page.
/// An empty range overlaps nothing; a range that wraps the address space is
/// treated as overlapping, so it is refused rather than reaching the kernel.
fn overlaps_private_page(address: usize, length: usize) -> bool {
    if length == 0 {
        return false;
    }
    let Some(end) = address.checked_add(length) else {
        return true;
    };
    let page_start = cp::PRIVATE_PAGE_OFFSET;
    let page_end = cp::PRIVATE_PAGE_OFFSET + cp::PRIVATE_PAGE_SIZE;
    address < page_end && page_start < end
}

fn refuse_private_page_write(address: usize, length: usize) -> Result<(), Errno> {
    if overlaps_private_page(address, length) {
        tracing::warn!(
            address,
            length,
            "refusing a Tool write into Reverie's private page"
        );
        return Err(Errno::EFAULT);
    }
    Ok(())
}

impl MemoryAccess for ToolMemory {
    fn read_vectored(
        &self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> Result<usize, Errno> {
        self.check_held_stop()?;
        self.0.read_vectored(read_from, write_to)
    }

    fn write_vectored(
        &mut self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> Result<usize, Errno> {
        self.check_held_stop()?;
        for remote in write_to.iter() {
            refuse_private_page_write(remote.as_ptr() as usize, remote.len())?;
        }
        // process_vm_writev: permission-respecting.
        self.0.write_vectored(read_from, write_to)
    }

    fn write_native_user_vectored(
        &mut self,
        expected_tid: i32,
        local: &[io::IoSlice],
        remote: &[RemoteIoVec],
    ) -> Result<usize, Errno> {
        self.check_held_stop()?;
        for span in remote {
            refuse_private_page_write(span.address(), span.length())?;
        }
        self.0
            .write_native_user_vectored(expected_tid, local, remote)
    }

    fn read<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<usize, Errno>
    where
        A: Into<Addr<'a, u8>>,
    {
        self.check_held_stop()?;
        self.0.read(addr, buf)
    }

    fn read_exact_with_user_access<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<(), Errno>
    where
        A: Into<Addr<'a, u8>>,
    {
        self.check_held_stop()?;
        self.0.read_exact_with_user_access(addr, buf)
    }

    /// Every size goes through the permission-respecting copy, never the
    /// eight-byte `PTRACE_POKEDATA` path of [`Stopped`]'s debugger `write`.
    fn write(&mut self, addr: AddrMut<u8>, buf: &[u8]) -> Result<usize, Errno> {
        self.check_held_stop()?;
        refuse_private_page_write(addr.as_raw(), buf.len())?;
        self.0.write_with_user_access(addr, buf)
    }

    fn write_with_user_access(&mut self, addr: AddrMut<u8>, buf: &[u8]) -> Result<usize, Errno> {
        self.check_held_stop()?;
        refuse_private_page_write(addr.as_raw(), buf.len())?;
        self.0.write_with_user_access(addr, buf)
    }

    /// A value of at most eight bytes is published as `put_user` publishes
    /// it: entirely, or not at all with `EFAULT`. Within one page the copy is
    /// already all-or-nothing. Across a page boundary every page is checked
    /// first, so a writable prefix is never left behind. Larger values keep
    /// `copy_to_user`'s prefix semantics.
    fn write_value<'a, A, T>(&mut self, addr: A, value: &T) -> Result<(), Errno>
    where
        A: Into<AddrMut<'a, T>>,
        T: Sized + 'a,
    {
        self.check_held_stop()?;
        let addr = addr.into().cast::<u8>();
        // SAFETY: `value` is a live `T`, viewed as its `size_of::<T>()` bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(value as *const T as *const u8, mem::size_of::<T>())
        };
        if bytes.len() <= PUT_USER_MAX && crosses_page(addr.as_raw(), bytes.len()) {
            refuse_private_page_write(addr.as_raw(), bytes.len())?;
            if !self.pages_writable(addr.as_raw(), bytes.len())? {
                return Err(Errno::EFAULT);
            }
        }
        self.write_exact(addr, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep the notifier worker and ptrace owner in one disposable libtest
    /// cell. The alarm and EXITKILL belong only to that cell and its tracee.
    #[test]
    fn a_published_exit_refuses_every_old_tool_memory_primitive() {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::process::Command;
        use std::process::Stdio;
        use std::time::Duration;
        use std::time::Instant;

        const CELL_ENV: &str = "REVERIE_TOOL_MEMORY_EXIT_CELL";
        const PASSED: &str = "TOOL_MEMORY_EXIT_CELL_PASSED";
        if let Ok(cell) = std::env::var(CELL_ENV) {
            assert_eq!(cell, "published-exit");
            published_exit_memory_cell();
            println!("{PASSED}");
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "tool_memory::tests::a_published_exit_refuses_every_old_tool_memory_primitive",
                "--exact",
                "--nocapture",
            ])
            .env(CELL_ENV, "published-exit")
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                // Only the newly created owned cell process group is signaled.
                assert_eq!(
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) },
                    0
                );
                let status = child.wait().unwrap();
                eprintln!("ToolMemory EXIT cell exceeded its 15s bound: {status}");
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(65537)
            .read_to_end(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .take(65537)
            .read_to_end(&mut stderr)
            .unwrap();
        assert!(stdout.len() <= 65536 && stderr.len() <= 65536);
        let stdout = String::from_utf8(stdout).unwrap();
        let stderr = String::from_utf8(stderr).unwrap();
        println!("ToolMemory EXIT cell, actual status {status}:\n{stdout}");
        eprintln!("{stderr}");
        assert!(status.success());
        assert_eq!(stdout.lines().filter(|line| *line == PASSED).count(), 1);
        assert!(stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"));
    }

    fn published_exit_memory_cell() {
        use std::time::Duration;

        const SENTINEL: u64 = 0x0123_4567_89ab_cdef;
        const REPLACEMENT: u64 = !SENTINEL;
        const BOUNDARY: [u8; 8] = [0x31, 0x42, 0x53, 0x64, 0x75, 0x86, 0x97, 0xa8];
        const WAIT: Duration = Duration::from_secs(2);

        struct Mapping(*mut libc::c_void);
        impl Drop for Mapping {
            fn drop(&mut self) {
                assert_eq!(unsafe { libc::munmap(self.0, 2 * PAGE_SIZE) }, 0);
            }
        }
        // This process is the re-executed owned cell, not the shared runner.
        unsafe { libc::alarm(10) };
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                2 * PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(raw, libc::MAP_FAILED);
        let mapping = Mapping(raw);
        let base = raw as usize;
        let cross = base + PAGE_SIZE - 4;
        // Both pages exist before fork, so every remote operand is also a
        // valid local address when the legacy IoSlice APIs form references.
        unsafe {
            (base as *mut u64).write(SENTINEL);
            std::ptr::copy_nonoverlapping(BOUNDARY.as_ptr(), cross as *mut u8, BOUNDARY.len());
        }
        assert_eq!(
            unsafe { libc::mprotect((base + PAGE_SIZE) as *mut _, PAGE_SIZE, libc::PROT_READ) },
            0
        );
        let expected_parent = unsafe { libc::getpid() };
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            // Death before PDEATHSIG was armed must not leave an orphan
            // holding the cell output pipes. Death after arming kills it.
            unsafe {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != expected_parent
                    || safeptrace::traceme_and_stop().is_err()
                {
                    libc::_exit(2);
                }
                loop {
                    libc::pause();
                }
            }
        }
        let pid = Pid::from_raw(child);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
        // The wait above proved this actual stop; no unchecked numeric
        // capability is created after notifier registration.
        let stopped = Stopped::new_unchecked(pid);
        stopped
            .setoptions(
                safeptrace::Options::PTRACE_O_TRACEEXIT | safeptrace::Options::PTRACE_O_EXITKILL,
            )
            .unwrap();
        let mut old = ToolMemory::new(stopped.generation().assume_stopped());
        let address = Addr::<u8>::from_raw(base).unwrap();
        let boundary = Addr::<u8>::from_raw(cross).unwrap();
        let writable = AddrMut::<u8>::from_raw(base).unwrap();
        let cross_value = AddrMut::<u64>::from_raw(cross).unwrap();
        assert_eq!(old.read_value::<_, u64>(address.cast()).unwrap(), SENTINEL);
        assert_eq!(
            old.write_value(cross_value, &REPLACEMENT),
            Err(Errno::EFAULT)
        );
        let mut bytes = [0; 8];
        stopped.read_exact(boundary, &mut bytes).unwrap();
        assert_eq!(bytes, BOUNDARY, "the live EFAULT left a writable prefix");

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                // One original EXIT future owns publication and the later
                // resume. The old ToolMemory never polls or claims a wait.
                let exit = stopped.exit_event();
                let terminal = stopped.terminal_cleanup();
                terminal.request_sigkill().unwrap();
                let exit_stopped = tokio::time::timeout(WAIT, exit).await.unwrap().unwrap();
                assert!(old.0.superseded_by_observed_exit_stop());
                assert!(!exit_stopped.superseded_by_observed_exit_stop());
                assert!(terminal.exit_stop_observed());
                let queued_before = terminal.queued_raw_statuses();
                let observed_before = terminal.exit_stop_observed();
                let assert_unchanged = || {
                    assert_eq!(
                        exit_stopped.read_value::<_, u64>(address.cast()).unwrap(),
                        SENTINEL
                    );
                    let mut actual = [0; 8];
                    exit_stopped.read_exact(boundary, &mut actual).unwrap();
                    assert_eq!(actual, BOUNDARY);
                };
                assert_unchanged();

                // All eight overrides are called directly with nonempty,
                // valid operands. Refused reads leave local output intact.
                {
                    let source = unsafe { std::slice::from_raw_parts(base as *const u8, 8) };
                    let mut output = [0xa5; 8];
                    assert_eq!(
                        old.read_vectored(
                            &[io::IoSlice::new(source)],
                            &mut [io::IoSliceMut::new(&mut output)]
                        ),
                        Err(Errno::ESRCH)
                    );
                    assert_eq!(output, [0xa5; 8]);
                }
                let mut output = [0xa5; 8];
                assert_eq!(old.read(address, &mut output), Err(Errno::ESRCH));
                assert_eq!(output, [0xa5; 8]);
                assert_eq!(
                    old.read_exact_with_user_access(address, &mut output),
                    Err(Errno::ESRCH)
                );
                assert_eq!(output, [0xa5; 8]);
                let payload = REPLACEMENT.to_ne_bytes();
                {
                    let target = unsafe { std::slice::from_raw_parts_mut(base as *mut u8, 8) };
                    assert_eq!(
                        old.write_vectored(
                            &[io::IoSlice::new(&payload)],
                            &mut [io::IoSliceMut::new(target)]
                        ),
                        Err(Errno::ESRCH)
                    );
                }
                assert_unchanged();
                assert_eq!(old.write(writable, &payload), Err(Errno::ESRCH));
                assert_unchanged();
                assert_eq!(
                    old.write_with_user_access(writable, &payload),
                    Err(Errno::ESRCH)
                );
                assert_unchanged();
                assert_eq!(
                    old.write_native_user_vectored(
                        child,
                        &[io::IoSlice::new(&payload)],
                        &[RemoteIoVec::new(writable, payload.len()).unwrap()],
                    ),
                    Err(Errno::ESRCH)
                );
                assert_unchanged();
                // The live control returned EFAULT for these exact pages.
                // ESRCH must precede write_value's procfs permission probe.
                assert_eq!(
                    old.write_value(cross_value, &REPLACEMENT),
                    Err(Errno::ESRCH)
                );
                assert_unchanged();
                assert_eq!(terminal.queued_raw_statuses(), queued_before);
                assert_eq!(terminal.exit_stop_observed(), observed_before);

                // The actual marked owner is still a usable ToolMemory.
                // It retains permission semantics and alone resumes/reaps.
                let mut current = ToolMemory::new(exit_stopped);
                assert_eq!(
                    current.read_value::<_, u64>(address.cast()).unwrap(),
                    SENTINEL
                );
                assert_eq!(
                    current.write_value(cross_value, &REPLACEMENT),
                    Err(Errno::EFAULT)
                );
                current.0.read_exact(boundary, &mut bytes).unwrap();
                assert_eq!(bytes, BOUNDARY);
                assert_eq!(current.0.getevent().unwrap(), i64::from(libc::SIGKILL));
                let waited =
                    tokio::time::timeout(WAIT, current.0.resume(None).unwrap().next_state())
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    waited.assume_exited(),
                    (
                        pid,
                        safeptrace::ExitStatus::Signaled(safeptrace::Signal::SIGKILL, false)
                    )
                );
                assert!(terminal.wait(WAIT));
            });
        drop(mapping);
        unsafe { libc::alarm(0) };
    }

    #[test]
    fn private_page_overlap_is_exact() {
        let start = cp::PRIVATE_PAGE_OFFSET;
        let end = start + cp::PRIVATE_PAGE_SIZE;
        assert!(!overlaps_private_page(start - 8, 8));
        assert!(overlaps_private_page(start - 8, 9));
        assert!(overlaps_private_page(start + 2, 8));
        assert!(overlaps_private_page(end - 1, 1));
        assert!(!overlaps_private_page(end, 8));
        assert!(!overlaps_private_page(start, 0));
        assert!(overlaps_private_page(usize::MAX - 3, 8));
    }

    #[test]
    fn page_crossing_is_exact() {
        assert!(!crosses_page(PAGE_SIZE - 8, 8));
        assert!(crosses_page(PAGE_SIZE - 4, 8));
        assert!(crosses_page(PAGE_SIZE - 1, 2));
        assert!(!crosses_page(PAGE_SIZE - 1, 1));
        assert!(!crosses_page(PAGE_SIZE, 8));
    }
}
