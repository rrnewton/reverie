/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::write_proc_mem_prefix` through a real `TracedTask` at a real
//! ptrace stop: an empty store succeeds at address 0, a store starting in
//! the trampoline is `EIO` with its bytes unchanged, a straddling store is
//! clipped at the trampoline, scratch is ordinary guest memory while the
//! backend does not own it, and the same scratch address is refused once
//! the repair page is sealed.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::Errno;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::syscalls::Addr;
use reverie::syscalls::MemoryAccess;
use safeptrace::Stopped;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::SCRATCH_SEALED;
use super::TracedTask;
use super::TracedTaskOptions;
use crate::cp;

const CELL_ENV: &str = "REVERIE_WRITE_PROC_MEM_CELL";
const TEST_NAME: &str =
    "task::write_proc_mem_tests::write_proc_mem_prefix_clips_at_owned_pages_at_a_real_stop";

/// Each run is in a new libtest process, as the other live `TracedTask`
/// tests are: the fork, fixed mappings, and ptracer-thread state stay out
/// of the shared runner.
#[test]
fn write_proc_mem_prefix_clips_at_owned_pages_at_a_real_stop() {
    if let Ok(cell) = std::env::var(CELL_ENV) {
        assert_eq!(cell, "stops");
        run_cell();
        println!("WRITE_PROC_MEM_CELL_PASSED {cell}");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--nocapture"])
        .env(CELL_ENV, "stops")
        .env("RUST_LOG", "warn")
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
            // Only this freshly spawned owned process group is signaled.
            // SAFETY: kill takes plain values.
            assert_eq!(
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) },
                0
            );
            let status = child.wait().unwrap();
            eprintln!("write_proc_mem cell exceeded its 15s bound: {status}");
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
    let stdout = String::from_utf8_lossy(&stdout);
    println!("write_proc_mem cell, status {status}:\n{stdout}");
    eprintln!("{}", String::from_utf8_lossy(&stderr));
    assert!(
        status.success()
            && stdout
                .lines()
                .filter(|line| *line == "WRITE_PROC_MEM_CELL_PASSED stops")
                .count()
                == 1,
        "the cell must pass"
    );
}

/// Reaps the traced child on every exit from the test.
struct Child(libc::pid_t);

impl Drop for Child {
    fn drop(&mut self) {
        if self.0 > 0 {
            // SAFETY: kill and waitpid take plain values and a local status.
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
                loop {
                    let mut status = 0;
                    let actual = libc::waitpid(self.0, &mut status, libc::__WALL);
                    if actual == self.0 && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status)) {
                        break;
                    }
                    if actual < 0 && *libc::__errno_location() != libc::EINTR {
                        break;
                    }
                }
            }
        }
    }
}

fn page_size() -> usize {
    // SAFETY: sysconf takes a plain name and writes nothing.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(page > 0);
    page as usize
}

fn read_bytes(memory: &Stopped, address: usize) -> [u8; 16] {
    let mut bytes = [0; 16];
    memory
        .read_exact(Addr::from_raw(address).unwrap(), &mut bytes)
        .unwrap();
    bytes
}

fn run_cell() {
    // The alarm is confined to this re-executed cell, never the shared runner.
    // SAFETY: alarm takes a plain value.
    unsafe { libc::alarm(10) };
    let page = page_size();
    let before = cp::TRAMPOLINE_BASE - page;
    let trampoline = cp::TRAMPOLINE_BASE;
    let scratch = cp::SCRATCH_BASE;

    // SAFETY: the child only calls async-signal-safe functions before _exit.
    let raw = unsafe { libc::fork() };
    assert!(raw >= 0);
    if raw == 0 {
        // SAFETY: async-signal-safe calls in the forked child. Each fixed
        // page is freshly mapped with NOREPLACE and filled with a canary.
        unsafe {
            for base in [before, trampoline, scratch] {
                let mapped = libc::mmap(
                    base as *mut libc::c_void,
                    page,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                    -1,
                    0,
                );
                if mapped != base as *mut libc::c_void {
                    libc::_exit(2);
                }
                libc::memset(mapped, 0xa5, page);
            }
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 || libc::raise(libc::SIGSTOP) != 0 {
                libc::_exit(2);
            }
            libc::_exit(0);
        }
    }
    let _child = Child(raw);
    let mut status = 0;
    // SAFETY: waitpid on our own child with a local status.
    assert_eq!(unsafe { libc::waitpid(raw, &mut status, 0) }, raw);
    assert!(libc::WIFSTOPPED(status));
    assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);

    let pid = Pid::from_raw(raw);
    let original = Stopped::new_unchecked_on_ptracer_thread(pid).unwrap();
    let events = Subscription::none();
    let (orphanage, _orphans) = mpsc::channel(1);
    let (daemon_kill, _) = broadcast::channel(1);
    let mut traced = TracedTask::<()>::new(
        pid,
        (),
        Arc::new(()),
        TracedTaskOptions {
            reserved_input: None,
            command_bootstrap: false,
            events: &events,
            injected_syscall_trap: None,
            backend_stats: None,
            final_resume_signal_for_test: None,
            pre_syscall_for_test: None,
            preinit_point_for_test: None,
        },
        orphanage,
        daemon_kill,
        None,
    );
    traced.ptracer_waits.bind_stopped(&original);

    // Empty input succeeds before address conversion, at 0 and at an owned
    // page alike.
    assert_eq!(traced.write_proc_mem_prefix(0, &[]), Ok(0));
    assert_eq!(traced.write_proc_mem_prefix(trampoline as u64, &[]), Ok(0));

    // A store starting inside the trampoline is EIO and changes nothing.
    assert_eq!(
        traced.write_proc_mem_prefix(trampoline as u64, b"owned"),
        Err(Errno::EIO)
    );
    assert_eq!(read_bytes(&original, trampoline), [0xa5; 16]);

    // A straddling store is clipped to the three bytes before the
    // trampoline; the trampoline's canary is unchanged.
    assert_eq!(
        traced.write_proc_mem_prefix((trampoline - 3) as u64, b"12345678"),
        Ok(3)
    );
    let mut straddled = [0xa5; 16];
    straddled[5..8].copy_from_slice(b"123");
    assert_eq!(read_bytes(&original, trampoline - 8), straddled);

    // Scratch is still pending setup in this fixture, so the backend does
    // not own it: the store there is ordinary guest memory and succeeds.
    assert_eq!(
        traced.write_proc_mem_prefix(scratch as u64, b"guest"),
        Ok(5)
    );
    let mut guest_scratch = [0xa5; 16];
    guest_scratch[..5].copy_from_slice(b"guest");
    assert_eq!(read_bytes(&original, scratch), guest_scratch);

    // Once the repair page is sealed, the backend owns scratch: a store
    // starting there is EIO and changes nothing.
    traced
        .sigtrap_scratch_state
        .store(SCRATCH_SEALED, Ordering::SeqCst);
    assert_eq!(
        traced.write_proc_mem_prefix((scratch + 8) as u64, b"owned"),
        Err(Errno::EIO)
    );
    assert_eq!(read_bytes(&original, scratch + 8), [0xa5; 16]);
}
