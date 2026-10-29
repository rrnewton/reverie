/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[cfg(all(not(sanitized), test))]
#[path = "tool_fixtures/vdso.rs"]
mod vdso_tool;

#[cfg(all(not(sanitized), test))]
use vdso_tool::LocalState;

#[cfg(all(not(sanitized), test))]
mod tests {
    use std::mem::MaybeUninit;
    use std::time;

    use reverie_ptrace::testing::check_fn;

    use super::*;

    #[test]
    #[should_panic]
    fn run_guest_vdso_tod_test() {
        check_fn::<LocalState, _>(|| {
            // this calls clock_gettime.
            let _now = time::Instant::now();
        });
    }

    #[test]
    fn run_guest_vdso_getcpu_test() {
        struct VdsoHandle(*mut libc::c_void);

        impl Drop for VdsoHandle {
            fn drop(&mut self) {
                unsafe { libc::dlclose(self.0) };
            }
        }

        type Getcpu = unsafe extern "C" fn(
            *mut libc::c_uint,
            *mut libc::c_uint,
            *mut libc::c_void,
        ) -> libc::c_long;

        // Resolve in the parent: loader operations are unsafe after a threaded
        // fork, and sched_getcpu may read rseq TLS without entering the vDSO.
        let vdso_base = unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } as usize;
        assert_ne!(vdso_base, 0);
        let handle = unsafe {
            libc::dlerror();
            libc::dlopen(
                c"linux-vdso.so.1".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        assert!(!handle.is_null());
        let handle = VdsoHandle(handle);
        assert!(unsafe { libc::dlerror() }.is_null());
        let symbol =
            unsafe { libc::dlvsym(handle.0, c"__vdso_getcpu".as_ptr(), c"LINUX_2.6".as_ptr()) };
        assert!(unsafe { libc::dlerror() }.is_null());
        assert!(!symbol.is_null());
        let mut info = MaybeUninit::uninit();
        assert_ne!(unsafe { libc::dladdr(symbol, info.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { info.assume_init() }.dli_fbase as usize, vdso_base);
        let getcpu = unsafe { std::mem::transmute::<*mut libc::c_void, Getcpu>(symbol) };

        check_fn::<LocalState, _>(move || {
            // This function guest inherits the parent's mapping; ptrace patches
            // the entry in place before the closure runs.
            assert_eq!(
                unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) } as usize,
                vdso_base
            );
            let mut cpu = libc::c_uint::MAX;
            let ret = unsafe { getcpu(&mut cpu, std::ptr::null_mut(), std::ptr::null_mut()) };
            assert_eq!(ret, 0);
            // NB: getcpu in vdso area always set cpu to 0.
            // see symbol __vdso_getcpu.
            assert_eq!(cpu, 0);
        });
        drop(handle);
    }

    #[test]
    fn run_guest_vdso_gettimeofday_test() {
        check_fn::<LocalState, _>(|| {
            let mut tod = MaybeUninit::zeroed();
            let mut tz = MaybeUninit::zeroed();
            assert_eq!(
                unsafe { libc::gettimeofday(tod.as_mut_ptr(), tz.as_mut_ptr()) },
                0
            );
            let tod = unsafe { tod.assume_init() };
            assert_eq!(tod.tv_usec % 1000, 345);
        });
    }

    #[test]
    fn run_guest_vdso_clock_getres_test() {
        check_fn::<LocalState, _>(|| {
            let mut res = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };

            let ret = unsafe { libc::clock_getres(libc::CLOCK_MONOTONIC, &mut res as *mut _) };

            assert_eq!(ret, 0);

            assert_eq!(res.tv_sec, 0);
            assert_eq!(res.tv_nsec, 42);
        });
    }
}

#[cfg(all(not(sanitized), test))]
#[path = "support/vdso_command.rs"]
mod command;

#[cfg(all(not(sanitized), test))]
fn assert_command_output(label: &str, output: &reverie::process::Output) {
    eprintln!(
        "vdso command {label}: status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(all(not(sanitized), test))]
#[test]
fn ptrace_vdso_command() {
    let guest = command::GuestBinary::compile();
    guest.assert_native();
    guest.record_before_launch("ptrace");
    let (output, ()) =
        reverie_test_support::test_cmd::<LocalState>(guest.program(), &["tool"]).unwrap();
    assert_command_output("ptrace", &output);
}
