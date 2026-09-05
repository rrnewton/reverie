/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Protects a set of file descriptors from getting closed.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::AcqRel;
use std::sync::atomic::Ordering::Acquire;

use parking_lot::Mutex;
use syscalls::Sysno;
use syscalls::SysnoSet;

struct ProtectedFiles {
    // We have to use Vec here to ensure `new` can be a const fn, which is
    // required for global static variables. This should be fine, since we don't
    // expect to be protecting more than a handful of file descriptors.
    files: Vec<RawFd>,
}

impl ProtectedFiles {
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }

    pub fn contains<Fd: AsRawFd>(&self, fd: &Fd) -> bool {
        self.files.contains(&fd.as_raw_fd())
    }

    pub fn insert<Fd: AsRawFd>(&mut self, fd: &Fd) -> bool {
        if self.contains(fd) {
            true
        } else {
            self.files.push(fd.as_raw_fd());
            false
        }
    }

    pub fn remove<Fd: AsRawFd>(&mut self, fd: &Fd) -> bool {
        let fd = fd.as_raw_fd();
        if let Some(index) = self.files.iter().position(|item| item == &fd) {
            self.files.swap_remove(index);
            true
        } else {
            false
        }
    }
}
struct RegistryValue {
    pid: libc::pid_t,
    files: Mutex<ProtectedFiles>,
}

struct ProcessRegistry {
    current: AtomicPtr<RegistryValue>,
}

impl ProcessRegistry {
    const fn new() -> Self {
        Self {
            current: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn get(&'static self) -> &'static Mutex<ProtectedFiles> {
        let pid = unsafe { libc::getpid() };
        loop {
            let current = self.current.load(Acquire);
            if !current.is_null() && unsafe { (*current).pid == pid } {
                return unsafe { &(*current).files };
            }

            let candidate = Box::into_raw(Box::new(RegistryValue {
                pid,
                files: Mutex::new(ProtectedFiles::new()),
            }));
            match self
                .current
                .compare_exchange(current, candidate, AcqRel, Acquire)
            {
                Ok(_) => return unsafe { &(*candidate).files },
                Err(actual) => {
                    unsafe { drop(Box::from_raw(candidate)) };
                    if !actual.is_null() && unsafe { (*actual).pid == pid } {
                        return unsafe { &(*actual).files };
                    }
                }
            }
        }
    }
}

static PROTECTED_FILES: ProcessRegistry = ProcessRegistry::new();

fn protected_files() -> &'static Mutex<ProtectedFiles> {
    PROTECTED_FILES.get()
}

/// A file descriptor that is internal to the plugin and not visible to the
/// client. These file descriptors cannot be closed by the client.
pub struct ProtectedFd<T: AsRawFd>(T);

impl<T: AsRawFd> Drop for ProtectedFd<T> {
    fn drop(&mut self) {
        protected_files().lock().remove(&self.0);
    }
}

impl<T: AsRawFd> AsRef<T> for ProtectedFd<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T: AsRawFd> AsMut<T> for ProtectedFd<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// Takes a closure `f` that creates and returns a file descriptor. The file
/// descriptor that is returned is protected from getting closed. This is safe
/// even if another thread is trying to close this same file descriptor.
pub fn protect_with<F, T, E>(f: F) -> Result<ProtectedFd<T>, E>
where
    F: FnOnce() -> Result<T, E>,
    T: AsRawFd,
{
    let mut protected_files = protected_files().lock();

    f().map(|fd| {
        protected_files.insert(&fd);
        ProtectedFd(fd)
    })
}

/// Returns true if a file descriptor is protected and shouldn't be closed.
pub fn is_protected<Fd: AsRawFd>(fd: &Fd) -> bool {
    protected_files().lock().contains(fd)
}

/// Registers an already-open inherited descriptor as private to this plugin
/// process.
///
/// This function does not take ownership of, duplicate, close, or change the
/// descriptor. The caller must keep the same descriptor open for the lifetime
/// of the current plugin process. A fork child receives a fresh protected-file
/// registry, so process-local plugin initialization must call this function
/// again before the child handles guest syscalls.
///
/// Registration is idempotent. A negative or closed descriptor is rejected
/// without adding it to the protected-file registry.
#[doc(hidden)]
pub fn protect_inherited_fd(fd: RawFd) -> io::Result<()> {
    let mut protected = protected_files().lock();
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    protected.insert(&fd);
    Ok(())
}

pub(crate) fn protect_raw_fd(fd: RawFd) {
    protected_files().lock().insert(&fd);
}

pub(crate) fn protect_raw_pair_with<F, E>(create: F) -> Result<(RawFd, RawFd), E>
where
    F: FnOnce() -> Result<(RawFd, RawFd), E>,
{
    let mut protected = protected_files().lock();
    let pair = create()?;
    protected.insert(&pair.0);
    protected.insert(&pair.1);
    Ok(pair)
}

pub(crate) fn unprotect_raw_fd(fd: RawFd) {
    protected_files().lock().remove(&fd);
}

fn close_ranges_around_protected(first: u32, last: u32, protected: &[RawFd]) -> Vec<(u32, u32)> {
    if first > last {
        return vec![(first, last)];
    }
    let mut protected: Vec<u32> = protected
        .iter()
        .filter_map(|fd| u32::try_from(*fd).ok())
        .filter(|fd| (first..=last).contains(fd))
        .collect();
    protected.sort_unstable();
    protected.dedup();

    let mut ranges = Vec::new();
    let mut next = first;
    for fd in protected {
        if next < fd {
            ranges.push((next, fd - 1));
        }
        let Some(after) = fd.checked_add(1) else {
            return ranges;
        };
        next = after;
    }
    if next <= last {
        ranges.push((next, last));
    }
    ranges
}

pub(crate) fn sys_close_range(
    first: usize,
    last: usize,
    flags: usize,
) -> Result<usize, syscalls::Errno> {
    const CLOSE_RANGE_UNSHARE: usize = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: usize = 1 << 2;
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return Err(syscalls::Errno::EINVAL);
    }

    let first = first as u32;
    let last = last as u32;
    if first > last {
        return Err(syscalls::Errno::EINVAL);
    }
    let mut flags = flags;
    if flags & CLOSE_RANGE_UNSHARE != 0 {
        unsafe { syscalls::syscall1(Sysno::unshare, libc::CLONE_FILES as usize)? };
        flags &= !CLOSE_RANGE_UNSHARE;
    }
    let protected = protected_files().lock();
    for (range_first, range_last) in close_ranges_around_protected(first, last, &protected.files) {
        unsafe {
            syscalls::syscall3(
                Sysno::close_range,
                range_first as usize,
                range_last as usize,
                flags,
            )?;
        }
    }
    Ok(0)
}

/// All of these syscalls take the input file descriptor as the first argument.
/// Some syscalls, like mmap, don't conform to this pattern and need to be
/// handled in a special way.
static FD_ARG0_SYSCALLS: SysnoSet = SysnoSet::new(&[
    Sysno::close,
    Sysno::dup,
    Sysno::dup2,
    Sysno::openat,
    Sysno::fstat,
    Sysno::read,
    Sysno::write,
    Sysno::lseek,
    Sysno::ioctl,
    Sysno::pread64,
    Sysno::pwrite64,
    Sysno::readv,
    Sysno::writev,
    Sysno::connect,
    Sysno::accept,
    Sysno::sendto,
    Sysno::recvfrom,
    Sysno::sendmsg,
    Sysno::recvmsg,
    Sysno::shutdown,
    Sysno::bind,
    Sysno::listen,
    Sysno::getsockname,
    Sysno::getpeername,
    Sysno::getsockopt,
    Sysno::fcntl,
    Sysno::flock,
    Sysno::fsync,
    Sysno::fdatasync,
    Sysno::ftruncate,
    Sysno::getdents,
    Sysno::getdents64,
    Sysno::fchdir,
    Sysno::fchmod,
    Sysno::fchown,
    Sysno::fstatfs,
    Sysno::readahead,
    Sysno::fsetxattr,
    Sysno::fgetxattr,
    Sysno::flistxattr,
    Sysno::fremovexattr,
    Sysno::fadvise64,
    Sysno::epoll_wait,
    Sysno::epoll_ctl,
    Sysno::inotify_add_watch,
    Sysno::inotify_rm_watch,
    Sysno::mkdirat,
    Sysno::mknodat,
    Sysno::fchownat,
    Sysno::futimesat,
    Sysno::newfstatat,
    Sysno::unlinkat,
    Sysno::renameat,
    Sysno::linkat,
    Sysno::readlinkat,
    Sysno::fchmodat,
    Sysno::faccessat,
    Sysno::sync_file_range,
    Sysno::vmsplice,
    Sysno::utimensat,
    Sysno::epoll_pwait,
    Sysno::signalfd,
    Sysno::fallocate,
    Sysno::timerfd_settime,
    Sysno::timerfd_gettime,
    Sysno::accept4,
    Sysno::signalfd4,
    Sysno::dup3,
    Sysno::preadv,
    Sysno::pwritev,
    Sysno::recvmmsg,
    Sysno::fanotify_mark,
    Sysno::name_to_handle_at,
    Sysno::open_by_handle_at,
    Sysno::syncfs,
    Sysno::sendmmsg,
    Sysno::setns,
    Sysno::finit_module,
    Sysno::renameat2,
    Sysno::kexec_file_load,
    Sysno::execveat,
    Sysno::preadv2,
    Sysno::pwritev2,
    Sysno::statx,
    Sysno::pidfd_send_signal,
    Sysno::io_uring_enter,
    Sysno::io_uring_register,
    Sysno::open_tree,
    Sysno::move_mount,
    Sysno::fsconfig,
    Sysno::fsmount,
    Sysno::fspick,
    Sysno::openat2,
    Sysno::pidfd_getfd,
]);

static FD_ARG1_SYSCALLS: SysnoSet = SysnoSet::new(&[Sysno::dup2, Sysno::dup3]);

/// Returns true if the given syscall operates on a protected file descriptor.
pub fn uses_protected_fd(sysno: Sysno, arg0: usize, arg1: usize) -> bool {
    (FD_ARG0_SYSCALLS.contains(sysno) && is_protected(&(arg0 as i32)))
        || (FD_ARG1_SYSCALLS.contains(sysno) && is_protected(&(arg1 as i32)))
}
#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::time::Duration;

    use reverie_syscalls::Syscall;
    use syscalls::SyscallArgs;

    use super::*;
    use crate::tool::SyscallExt;

    const EXEC_TEST_FD_ENV: &str = "REVERIE_SABRE_TEST_INHERITED_FD";

    fn guest_syscall(sysno: Sysno, arg0: usize, arg1: usize) -> Result<usize, syscalls::Errno> {
        let syscall = Syscall::from_raw(sysno, SyscallArgs::new(arg0, arg1, 0, 0, 0, 0));
        unsafe { syscall.call() }
    }

    fn raw_write(fd: RawFd, bytes: &[u8]) -> Result<usize, syscalls::Errno> {
        unsafe {
            syscalls::syscall3(
                Sysno::write,
                fd as usize,
                bytes.as_ptr() as usize,
                bytes.len(),
            )
        }
    }

    fn assert_guest_access_is_refused(fd: RawFd) {
        for sysno in [Sysno::close, Sysno::read, Sysno::write, Sysno::dup] {
            assert_eq!(
                guest_syscall(sysno, fd as usize, 0),
                Err(syscalls::Errno::EBADF)
            );
        }
        for sysno in [Sysno::dup2, Sysno::dup3] {
            let target = unsafe { syscalls::syscall1(Sysno::dup, fd as usize) }.unwrap() as RawFd;
            assert_eq!(
                guest_syscall(sysno, fd as usize, target as usize),
                Err(syscalls::Errno::EBADF)
            );
            assert_eq!(
                unsafe { syscalls::syscall1(Sysno::close, target as usize) },
                Ok(0)
            );

            let source = unsafe { syscalls::syscall1(Sysno::dup, fd as usize) }.unwrap() as RawFd;
            assert_eq!(
                guest_syscall(sysno, source as usize, fd as usize),
                Err(syscalls::Errno::EBADF)
            );
            assert_eq!(
                unsafe { syscalls::syscall1(Sysno::close, source as usize) },
                Ok(0)
            );
        }
    }

    #[test]
    fn ordinary_descriptor_guest_syscalls_remain_allowed() {
        let (_reader, writer) = UnixStream::pair().unwrap();
        let fd = writer.as_raw_fd();

        assert_eq!(guest_syscall(Sysno::read, fd as usize, 0), Ok(0));
        assert_eq!(guest_syscall(Sysno::write, fd as usize, 0), Ok(0));

        let duplicate = guest_syscall(Sysno::dup, fd as usize, 0).unwrap() as RawFd;
        assert!(duplicate >= 0);
        assert_eq!(
            unsafe { syscalls::syscall1(Sysno::close, duplicate as usize) },
            Ok(0)
        );

        for sysno in [Sysno::dup2, Sysno::dup3] {
            let target = unsafe { syscalls::syscall1(Sysno::dup, fd as usize) }.unwrap() as RawFd;
            assert_eq!(
                guest_syscall(sysno, fd as usize, target as usize),
                Ok(target as usize)
            );
            assert_eq!(
                unsafe { syscalls::syscall1(Sysno::close, target as usize) },
                Ok(0)
            );
        }

        let close_target = unsafe { syscalls::syscall1(Sysno::dup, fd as usize) }.unwrap() as RawFd;
        assert_eq!(guest_syscall(Sysno::close, close_target as usize, 0), Ok(0));

        let range_target = unsafe { syscalls::syscall1(Sysno::dup, fd as usize) }.unwrap() as RawFd;
        assert_eq!(
            guest_syscall(
                Sysno::close_range,
                range_target as usize,
                range_target as usize,
            ),
            Ok(0)
        );
        assert_eq!(unsafe { libc::fcntl(range_target, libc::F_GETFD) }, -1);
    }

    #[test]
    fn inherited_sink_is_plugin_writable_but_hidden_from_guest_syscalls() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let fd = writer.as_raw_fd();

        crate::protect_inherited_fd(fd).unwrap();
        crate::protect_inherited_fd(fd).unwrap();
        assert!(is_protected(&fd));
        assert_guest_access_is_refused(fd);

        assert_eq!(
            guest_syscall(Sysno::close_range, fd as usize, fd as usize),
            Ok(0)
        );
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);

        let message = b"authoritative packet";
        assert_eq!(raw_write(fd, message), Ok(message.len()));
        let mut received = vec![0; message.len()];
        reader.read_exact(&mut received).unwrap();
        assert_eq!(received, message);

        unprotect_raw_fd(fd);
    }

    #[test]
    fn inherited_sink_registration_rejects_invalid_descriptors() {
        let error = crate::protect_inherited_fd(-1).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
        assert!(!is_protected(&-1));
    }

    #[test]
    fn fork_child_requires_explicit_inherited_sink_registration() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let fd = writer.as_raw_fd();
        crate::protect_inherited_fd(fd).unwrap();

        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let exit_code = if is_protected(&fd) {
                10
            } else if crate::protect_inherited_fd(fd).is_err() {
                11
            } else if !is_protected(&fd) {
                12
            } else if raw_write(fd, b"f") != Ok(1) {
                13
            } else {
                assert_guest_access_is_refused(fd);
                0
            };
            unsafe { libc::_exit(exit_code) };
        }

        let mut received = [0];
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        reader.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"f");

        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert!(is_protected(&fd));
        unprotect_raw_fd(fd);
    }

    #[test]
    fn exec_child_registers_inherited_sink() {
        let Some(fd) = std::env::var_os(EXEC_TEST_FD_ENV) else {
            return;
        };
        let fd = fd.to_str().unwrap().parse::<RawFd>().unwrap();

        assert!(!is_protected(&fd));
        crate::protect_inherited_fd(fd).unwrap();
        assert!(is_protected(&fd));
        assert_eq!(raw_write(fd, b"e"), Ok(1));
        assert_guest_access_is_refused(fd);
    }

    #[test]
    fn inherited_sink_can_be_registered_after_exec() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let fd = writer.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
            0
        );

        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("protected_files::tests::exec_child_registers_inherited_sink")
            .env(EXEC_TEST_FD_ENV, fd.to_string())
            .status()
            .unwrap();
        assert!(status.success());

        let mut received = [0];
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        reader.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"e");
    }

    #[test]
    fn fork_child_does_not_reuse_locked_registry() {
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let locker = std::thread::spawn(move || {
            let _guard = protected_files().lock();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::alarm(5) };
            let _ = is_protected(&(-1_i32));
            unsafe { libc::_exit(0) };
        }

        release_tx.send(()).unwrap();
        locker.join().unwrap();
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert_eq!(status, 0);
    }

    /// ⚠️ THE DESCRIPTOR MUST STAY OPEN ACROSS THE LAST ASSERTION, AND THAT IS
    /// THE WHOLE REASON THIS TEST IS SHAPED THIS WAY. The registry is keyed on
    /// the descriptor NUMBER, and a number is reusable the instant it is closed.
    ///
    /// Written the obvious way -- protect the `File` itself, so dropping the
    /// guard both deregisters AND closes -- this test was red about half the
    /// time in the full suite and always green in isolation. Measured on reverie
    /// main b181b1bb, 40 runs of `cargo test -p reverie-sabre --lib`:
    /// 21 failed, always here, always `assertion failed: !is_protected(&fd)`.
    ///
    /// Traced by logging every registry insert and remove, one failing run:
    ///
    /// ```text
    ///   insert fd=14 set=[13, 10, 3, 6, 7, 8, 11, 12]
    ///   remove fd=14 set=[13, 10, 3, 6, 7, 8, 11, 12, 14]   <- this guard drops
    ///   insert fd=14 set=[13, 10, 3, 6, 7, 8, 11, 12, 9]    <- SOMEONE ELSE takes 14
    /// ```
    ///
    /// Closing the file released number 14, a concurrently running test was
    /// handed the same number for its own descriptor and protected it, and this
    /// test then asked whether 14 was protected. It was -- correctly, for a
    /// different file. The registry and its lock are not implicated; the reused
    /// identity is.
    ///
    /// So the guard here holds a bare `RawFd`, which has no destructor: dropping
    /// it deregisters WITHOUT closing, `file` keeps the number allocated to this
    /// test until the end, and no other test can be handed it in between. The
    /// property under test is unchanged -- a registration appears with the guard
    /// and is gone after it drops.
    #[test]
    fn protected_guard_hides_descriptor_until_drop() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = file.as_raw_fd();
        let protected = protect_with(|| Ok::<RawFd, std::convert::Infallible>(fd)).unwrap();

        assert!(is_protected(&fd));
        assert!(uses_protected_fd(Sysno::close, fd as usize, 0));

        drop(protected);
        assert!(
            !is_protected(&fd),
            "the guard deregistered, so this number must not be protected; \
             `file` still owns it, so nothing else can have taken it"
        );
        drop(file);
    }

    #[test]
    fn close_ranges_skip_every_protected_descriptor() {
        assert_eq!(
            close_ranges_around_protected(10, 20, &[12, 15, 20]),
            vec![(10, 11), (13, 14), (16, 19)]
        );
        assert_eq!(
            close_ranges_around_protected(10, 12, &[9, 13]),
            vec![(10, 12)]
        );
        assert!(
            close_ranges_around_protected(i32::MAX as u32, i32::MAX as u32, &[i32::MAX]).is_empty()
        );
    }

    #[test]
    fn close_range_waits_for_atomic_protected_pair_creation() {
        let allow_registration = Arc::new(Barrier::new(2));
        let (created_tx, created_rx) = mpsc::channel();
        let (protected_tx, protected_rx) = mpsc::channel();
        let creator_barrier = allow_registration.clone();
        let creator = std::thread::spawn(move || {
            let pair = protect_raw_pair_with(|| {
                let mut pipe = [-1i32; 2];
                unsafe { syscalls::syscall2(Sysno::pipe2, pipe.as_mut_ptr() as usize, 0) }.unwrap();
                created_tx.send((pipe[0], pipe[1])).unwrap();
                creator_barrier.wait();
                Ok::<_, std::convert::Infallible>((pipe[0], pipe[1]))
            })
            .unwrap();
            protected_tx.send(pair).unwrap();
        });

        let pair = created_rx.recv().unwrap();
        let (closing_tx, closing_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            let first = pair.0.min(pair.1) as usize;
            let last = pair.0.max(pair.1) as usize;
            started_tx.send(()).unwrap();
            let result = sys_close_range(first, last, 1 << 2);
            closing_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(matches!(
            closing_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        allow_registration.wait();
        let pair = protected_rx.recv().unwrap();
        assert_eq!(closing_rx.recv().unwrap(), Ok(0));
        creator.join().unwrap();
        closer.join().unwrap();

        for fd in [pair.0, pair.1] {
            let descriptor_flags =
                unsafe { syscalls::syscall3(Sysno::fcntl, fd as usize, libc::F_GETFD as usize, 0) }
                    .unwrap();
            assert_eq!(descriptor_flags & libc::FD_CLOEXEC as usize, 0);
            unprotect_raw_fd(fd);
            unsafe { syscalls::syscall1(Sysno::close, fd as usize) }.unwrap();
        }
    }
}
