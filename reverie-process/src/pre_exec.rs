/// Best-effort failure-only output from a pre-exec callback.
///
/// Uses no allocation, formatting or locks. Preserves errno and the calling
/// thread's signal mask; a broken diagnostic pipe must not replace the original
/// spawn failure with SIGPIPE. Does not retry a failed or partial write.
pub fn report_pre_exec_failure(fd: i32, message: &[u8]) {
    unsafe {
        let saved_errno = *libc::__errno_location();
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        let mut pending: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGPIPE);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) == 0 {
            if libc::sigpending(&mut pending) == 0 {
                let was_pending = libc::sigismember(&pending, libc::SIGPIPE) == 1;
                let written = libc::write(fd, message.as_ptr().cast(), message.len());
                if written == -1 && *libc::__errno_location() == libc::EPIPE && !was_pending {
                    let timeout = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    };
                    while libc::sigtimedwait(&blocked, std::ptr::null_mut(), &timeout) == -1
                        && *libc::__errno_location() == libc::EINTR
                    {}
                }
            }
            libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        }
        *libc::__errno_location() = saved_errno;
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;
    use std::os::unix::process::CommandExt;

    use super::*;

    #[test]
    fn pre_exec_broken_diagnostic_pipe_preserves_errno_mask_and_pending_signal() {
        for already_pending in [false, true] {
            let mut pipe = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            drop(read);
            let mut command = std::process::Command::new("/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let mut signal: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut signal);
                    libc::sigaddset(&mut signal, libc::SIGPIPE);
                    if already_pending {
                        assert_eq!(
                            libc::pthread_sigmask(libc::SIG_BLOCK, &signal, std::ptr::null_mut()),
                            0
                        );
                        assert_eq!(libc::raise(libc::SIGPIPE), 0);
                    }
                    let mut before: libc::sigset_t = std::mem::zeroed();
                    let mut after: libc::sigset_t = std::mem::zeroed();
                    let mut pending: libc::sigset_t = std::mem::zeroed();
                    assert_eq!(
                        libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut before),
                        0
                    );
                    *libc::__errno_location() = libc::EINVAL;
                    report_pre_exec_failure(write.as_raw_fd(), b"failure\n");
                    assert_eq!(*libc::__errno_location(), libc::EINVAL);
                    assert_eq!(
                        libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut after),
                        0
                    );
                    for number in 1..=64 {
                        assert_eq!(
                            libc::sigismember(&before, number),
                            libc::sigismember(&after, number)
                        );
                    }
                    assert_eq!(libc::sigpending(&mut pending), 0);
                    assert_eq!(
                        libc::sigismember(&pending, libc::SIGPIPE),
                        i32::from(already_pending)
                    );
                    Err(io::Error::from_raw_os_error(libc::EINVAL))
                });
            }
            assert_eq!(
                command.spawn().unwrap_err().raw_os_error(),
                Some(libc::EINVAL)
            );
        }
    }
}
