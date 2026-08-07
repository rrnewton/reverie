/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use syscalls::Errno;

use super::Pid;

pub fn clone<F>(cb: F, flags: libc::c_int) -> Result<Pid, Errno>
where
    F: FnMut() -> i32,
{
    let mut stack = [0u8; 4096];
    clone_with_stack(cb, flags, &mut stack)
}

pub fn clone_with_stack<F>(cb: F, flags: libc::c_int, stack: &mut [u8]) -> Result<Pid, Errno>
where
    F: FnMut() -> i32,
{
    type CloneCb<'a> = Box<dyn FnMut() -> i32 + 'a>;

    extern "C" fn callback(data: *mut CloneCb) -> libc::c_int {
        let cb: &mut CloneCb = unsafe { &mut *data };
        // A panic must NOT unwind across this `extern "C"` frame: that hits
        // `panic_cannot_unwind` -> `abort()`, killing the cloned child by
        // signal and surfacing to the parent only as an opaque
        // "exited unexpectedly" crash. Catch it here and return the
        // conventional Rust panic exit code so the child exits cleanly and the
        // parent can observe and retry it. The default panic hook still runs
        // (before `catch_unwind` returns `Err`), preserving the child's stderr
        // diagnostic. `AssertUnwindSafe` is required because the closure holds
        // `&mut CloneCb`; nothing is observed after a caught panic.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (*cb)())) {
            Ok(code) => code as libc::c_int,
            Err(_) => 101,
        }
    }

    let mut cb: CloneCb = Box::new(cb);

    let res = unsafe {
        let stack = stack.as_mut_ptr().add(stack.len());
        let stack = stack.sub(stack as usize % 16);

        libc::clone(
            core::mem::transmute::<
                extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32,
                extern "C" fn(*mut libc::c_void) -> libc::c_int,
            >(callback as extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32),
            stack as *mut libc::c_void,
            flags,
            &mut cb as *mut _ as *mut libc::c_void,
        )
    };

    Errno::result(res).map(Pid::from_raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside the cloned child's callback must not unwind across the
    /// `extern "C"` frame. Unwinding across `extern "C"` hits
    /// `panic_cannot_unwind` -> `abort()`, which kills the cloned child by
    /// signal and surfaces to the parent only as "container exited
    /// unexpectedly" (a hard, non-retryable crash observed under concurrency).
    /// The guard must instead convert the panic into a clean, waitable exit
    /// code the parent can classify and retry.
    ///
    /// The child runs on a 2 MiB heap stack (matching `Container::run`'s stack
    /// size) so the fault we observe is the `extern "C"` panic-abort, not a
    /// stack overflow from the default panic hook on a tiny stack.
    #[test]
    fn clone_child_panic_is_a_clean_exit_not_a_signal_death() {
        let mut stack = vec![0u8; 2 * 1024 * 1024];
        let child = clone_with_stack(
            || {
                panic!("deliberate panic inside cloned child callback");
            },
            libc::SIGCHLD,
            &mut stack,
        )
        .expect("clone_with_stack failed");

        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(child.as_raw(), &mut status, 0) };
        assert_eq!(waited, child.as_raw(), "waitpid did not reap the child");

        assert!(
            libc::WIFEXITED(status),
            "cloned child died by signal {} instead of exiting cleanly \
             (a panic unwound across the extern \"C\" frame -> abort); status={:#x}",
            if libc::WIFSIGNALED(status) {
                libc::WTERMSIG(status)
            } else {
                -1
            },
            status,
        );
        assert_eq!(
            libc::WEXITSTATUS(status),
            101,
            "child should exit with the conventional Rust panic exit code 101",
        );
    }
}
