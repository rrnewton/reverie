/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Actual memory effects under a genuine consumed-stop acquisition.
//! These tests do not qualify numeric PID reuse or source-history observation.
use std::time::Duration;

use reverie_memory::RemoteIoVec;
use reverie_process::Pid;

use super::*;
use crate::ExitStatus;
use crate::Running;
use crate::Signal;
use crate::TerminalCleanup;
use crate::Wait;

const PAYLOAD: &[u8; 8] = b"GUARDED!";

struct Mapping(*mut u8);
impl Drop for Mapping {
    fn drop(&mut self) {
        if unsafe { libc::munmap(self.0.cast(), 4096) } != 0 {
            std::process::abort();
        }
    }
}

// Declared before the first real wait, and before the acquisition whose Drop
// releases the interlock on assertion unwind. Never discard failed custody.
struct ChildCleanup(TerminalCleanup);
impl ChildCleanup {
    fn finish(&self) -> Result<(), Errno> {
        if !self.0.is_reaped()? {
            match self.0.request_sigkill() {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error),
            }
        }
        if !self.0.wait(Duration::from_secs(2)) {
            return Err(Errno::ETIMEDOUT);
        }
        let status = self.0.observed_exit_status()?.ok_or(Errno::ENODATA)?;
        futures::executor::block_on(self.0.reap_parent_terminal())?;
        if !self.0.is_reaped()? {
            return Err(Errno::EBUSY);
        }
        eprintln!("memory source fixture: actual terminal={status:?}, notifier retired, reaped");
        Ok(())
    }
}
impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("memory source fixture lost exact cleanup: {error}");
            std::process::abort();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Raw,
    WaitStatus,
}
impl Route {
    fn alias(self, pid: Pid) -> Stopped {
        let wait = match self {
            Self::Raw => Wait::from_raw(pid, (libc::SIGSTOP << 8) | 0x7f),
            Self::WaitStatus => Wait::try_from(nix::sys::wait::WaitStatus::Stopped(
                pid.into(),
                Signal::SIGSTOP,
            )),
        };
        let (alias, event) = wait.unwrap().assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
        assert!(
            alias.source_stop().is_err(),
            "raw decoding never issues a receipt"
        );
        alias
    }
}

#[derive(Clone, Copy, Debug)]
enum Writer {
    Poke,
    Vectored,
    NativeVectored,
    UserAccess,
}
impl Writer {
    fn write(self, stopped: &mut Stopped, mapping: &Mapping) -> Result<usize, Errno> {
        let address = unsafe { mapping.0.add(8) } as usize;
        let remote = AddrMut::from_raw(address).unwrap();
        match self {
            Self::Poke => stopped.write(remote, PAYLOAD),
            Self::Vectored => {
                // This is a real writable parent mapping at the same virtual
                // address after fork. The kernel operand targets the CHILD;
                // the parent view exists solely for the legacy iovec API.
                let parent_view = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, 8) };
                stopped.write_vectored(
                    &[io::IoSlice::new(PAYLOAD)],
                    &mut [io::IoSliceMut::new(parent_view)],
                )
            }
            Self::NativeVectored => stopped.write_native_user_vectored(
                stopped.pid().as_raw(),
                &[io::IoSlice::new(PAYLOAD)],
                &[RemoteIoVec::new(remote, 8).unwrap()],
            ),
            Self::UserAccess => stopped.write_with_user_access(remote, PAYLOAD),
        }
    }
}

fn read_child(stopped: &Stopped, mapping: &Mapping) -> [u8; 32] {
    let mut bytes = [0; 32];
    assert_eq!(
        stopped.read(Addr::from_raw(mapping.0 as usize).unwrap(), &mut bytes),
        Ok(32),
        "actual complete child memory readback"
    );
    bytes
}

fn actual_exclusion(writer: Writer, route: Route) {
    let ptr = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED);
    let mapping = Mapping(ptr.cast());
    unsafe { core::ptr::write_bytes(mapping.0, 0xa5, 32) };
    let parent = unsafe { libc::getpid() };
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                libc::_exit(121);
            }
            if libc::getppid() != parent {
                libc::_exit(122);
            }
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                libc::_exit(120);
            }
            if libc::raise(libc::SIGSTOP) != 0 {
                libc::_exit(123);
            }
            for i in 0..32 {
                let expected = if (8..16).contains(&i) {
                    PAYLOAD[i - 8]
                } else {
                    0xa5
                };
                if core::ptr::read_volatile(mapping.0.add(i)) != expected {
                    libc::_exit(124);
                }
            }
            libc::_exit(0);
        }
    }
    let pid = Pid::from_raw(child);
    let running = Running::new(pid);
    let cleanup = ChildCleanup(running.terminal_cleanup());
    let (stopped, event) = running.wait().unwrap().assume_stopped();
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
    let source = stopped.source_stop().expect("original consumed stop");
    let acquisition = source.begin_acquisition().unwrap();
    let mut alias = route.alias(pid);
    assert_eq!(read_child(&stopped, &mapping), [0xa5; 32]);
    let result = writer.write(&mut alias, &mapping);
    let observed = read_child(&stopped, &mapping);
    eprintln!("actual {writer:?}/{route:?}: result={result:?}, child bytes={observed:?}");
    assert_eq!(
        result,
        Err(Errno::EBUSY),
        "write bypassed actual acquisition"
    );
    assert_eq!(observed, [0xa5; 32], "refusal changed child bytes");
    source.validate_current().unwrap();
    acquisition.finish_binding();
    source.validate_current().unwrap();
    assert_eq!(
        writer.write(&mut alias, &mapping),
        Ok(8),
        "ordinary write after release"
    );
    let mut expected = [0xa5; 32];
    expected[8..16].copy_from_slice(PAYLOAD);
    assert_eq!(read_child(&stopped, &mapping), expected);
    assert_eq!(source.validate_current(), Err(Errno::ESTALE));
    assert!(source.begin_acquisition().is_err());
    assert!(alias.source_stop().is_err());
    assert_eq!(
        unsafe { core::slice::from_raw_parts(mapping.0, 32) },
        &[0xa5; 32]
    );
    let final_wait = stopped.resume_retaining(None).unwrap().wait().unwrap();
    assert_eq!(final_wait.assume_exited(), (pid, ExitStatus::Exited(0)));
    cleanup.finish().unwrap();
}

macro_rules! cases {
    ($raw:ident, $status:ident, $writer:ident) => {
        #[test]
        fn $raw() {
            actual_exclusion(Writer::$writer, Route::Raw);
        }
        #[test]
        fn $status() {
            actual_exclusion(Writer::$writer, Route::WaitStatus);
        }
    };
}
cases!(
    poke_raw_excludes_source,
    poke_waitstatus_excludes_source,
    Poke
);
cases!(
    vectored_raw_excludes_source,
    vectored_waitstatus_excludes_source,
    Vectored
);
cases!(
    native_raw_excludes_source,
    native_waitstatus_excludes_source,
    NativeVectored
);
cases!(
    user_access_raw_excludes_source,
    user_access_waitstatus_excludes_source,
    UserAccess
);
