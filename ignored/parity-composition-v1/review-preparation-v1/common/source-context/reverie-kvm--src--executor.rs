// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 46..75; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
use crate::signal::ProcessSignalState;
use crate::signal::SS_AUTODISARM;
#[cfg(any(test, feature = "native-test-support"))]
use crate::signal::SharedThreadSignalState;
use crate::signal::event_for_process;
use crate::signal::event_for_thread;
#[cfg(test)]
use crate::signal::signal_info_user;

#[path = "process_signal_publication.rs"]
mod process_signal_publication;

#[path = "capture_identity.rs"]
mod capture_identity;

use capture_identity::CaptureMetadata;
use capture_identity::CaptureObjectIdentity;
use capture_identity::CapturedPipeIdentities;
use process_signal_publication::ProcessBinding;
use process_signal_publication::ProcessSignalRegistry;

const MAX_HOST_IO: usize = 16 * 1024 * 1024;
const MAX_CAPTURED_OUTPUT: usize = 64 * 1024 * 1024;
const PAGE_SIZE: u64 = 4096;
const MAX_RW_COUNT: usize = (i32::MAX as usize) & !(PAGE_SIZE as usize - 1);
const X86_64_GUEST_USER_LIMIT: u64 = (1_u64 << 47) - PAGE_SIZE;
const GUEST_NOFILE_LIMIT: libc::c_int = 1 << 20;
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-235): Review the single virtual network namespace identity.
const GUEST_NETNS_COOKIE: u64 = 1;

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 183..235; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
        result: i64,
        segment: Option<(SegmentBase, u64)>,
    },
    Exit(ExitStatus),
}

#[derive(Default)]
struct CapturedOutputInner {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone)]
// TODO-HUMAN-REVIEW(PR-172): Review cross-thread captured-output ownership.
pub(crate) struct CapturedOutput {
    inner: Arc<Mutex<CapturedOutputInner>>,
    identities: Arc<CapturedPipeIdentities>,
}

#[cfg(test)]
impl Default for CapturedOutput {
    fn default() -> Self {
        Self::try_new().expect("prepare test capture identities")
    }
}

impl CapturedOutput {
    pub(crate) fn try_new() -> std::io::Result<Self> {
        // Root setup calls this before consuming the image or initializing the
        // Tool. Failure never publishes partial output or a fallback identity.
        let identities = Arc::new(CapturedPipeIdentities::try_new()?);
        Ok(Self {
            inner: Arc::new(Mutex::new(CapturedOutputInner::default())),
            identities,
        })
    }

    fn metadata(&self) -> CaptureMetadata {
        self.identities.metadata()
    }

    pub(crate) fn take(&mut self) -> (Vec<u8>, Vec<u8>) {
        let mut inner = self.inner.lock().expect("captured output lock poisoned");
        (
            std::mem::take(&mut inner.stdout),
            std::mem::take(&mut inner.stderr),
        )
    }

    fn append(&self, stderr: bool, bytes: &[u8]) -> bool {
        let mut inner = self.inner.lock().expect("captured output lock poisoned");
        let destination = if stderr {
            &mut inner.stderr

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 375..960; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn execute_basic_syscall_with_output(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    request: &SyscallRequest,
    current_user_stack_pointer: Option<u64>,
    output: Option<&mut CapturedOutput>,
) -> SyscallAction {
    let args = request.args();
    let number = request.number();
    let capture_output = output.is_some();
    let capture_metadata = output.as_ref().map(|output| output.metadata());

    if number == libc::SYS_exit as u64 || number == libc::SYS_exit_group as u64 {
        return SyscallAction::Exit(ExitStatus::Exited((args[0] as i32) & 0xff));
    }
    if let Some(error) = fdinfo_private_carrier_error(state, number, args) {
        return continue_with(error);
    }
    if let Some(error) = virtual_signalfd_write_error(state, number, args) {
        return continue_with(error);
    }

    let result = if number == libc::SYS_write as u64 {
        write(memory, state, args, output)
    } else if number == libc::SYS_read as u64 {
        read(memory, state, args)
    } else if number == libc::SYS_writev as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_writev, output)
    } else if number == libc::SYS_readv as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_readv, output)
    } else if number == libc::SYS_preadv as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_preadv, output)
    } else if number == libc::SYS_preadv2 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_preadv2, output)
    } else if number == libc::SYS_pwritev as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_pwritev, output)
    } else if number == libc::SYS_pwritev2 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        vectored_io(memory, state, args, libc::SYS_pwritev2, output)
    } else if number == libc::SYS_pread64 as u64 {
        pread64(memory, state, args)
    } else if number == libc::SYS_pwrite64 as u64 {
        pwrite64(memory, state, args)
    } else if number == libc::SYS_sendfile as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sendfile(memory, state, args, output)
    } else if number == libc::SYS_lseek as u64 {
        lseek(state, args, capture_output)
    } else if number == libc::SYS_ftruncate as u64 {
        ftruncate(state, args)
    } else if number == libc::SYS_truncate as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        truncate(memory, state, args)
    } else if number == libc::SYS_fallocate as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fallocate(state, args)
    } else if number == libc::SYS_fsync as u64 {
        sync_file(state, args[0], false)
    } else if number == libc::SYS_fdatasync as u64 {
        sync_file(state, args[0], true)
    } else if number == libc::SYS_readahead as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review translated host readahead semantics.
        readahead(state, args)
    } else if number == libc::SYS_sync_file_range as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review translated host range-sync semantics.
        sync_file_range(state, args)
    } else if number == libc::SYS_pipe as u64 {
        pipe2(memory, state, args[0], 0)
    } else if number == libc::SYS_pipe2 as u64 {
        pipe2(memory, state, args[0], args[1])
    } else if number == libc::SYS_select as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        select(memory, state, args)
    } else if number == libc::SYS_poll as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        poll(memory, state, args)
    } else if number == libc::SYS_ppoll as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        ppoll(memory, state, args)
    } else if number == libc::SYS_epoll_create1 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        epoll_create1(state, args[0])
    } else if number == libc::SYS_epoll_ctl as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        epoll_ctl(memory, state, args)
    } else if number == libc::SYS_epoll_wait as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        epoll_wait(memory, state, args, false)
    } else if number == libc::SYS_epoll_pwait as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        epoll_wait(memory, state, args, true)
    } else if number == libc::SYS_eventfd as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        eventfd2(state, args[0], 0)
    } else if number == libc::SYS_eventfd2 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        eventfd2(state, args[0], args[1])
    } else if number == libc::SYS_signalfd as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        signalfd(memory, state, args, 0)
    } else if number == libc::SYS_signalfd4 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        signalfd(memory, state, args, args[3])
    } else if number == libc::SYS_timerfd_create as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        timerfd_create(state, args[0], args[1])
    } else if number == libc::SYS_timerfd_settime as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        timerfd_settime(memory, state, args)
    } else if number == libc::SYS_timerfd_gettime as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        timerfd_gettime(memory, state, args)
    } else if number == libc::SYS_pidfd_open as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-235): Review self-only virtual pidfd translation.
        pidfd_open(state, args)
    } else if number == libc::SYS_socket as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        socket(state, args)
    } else if number == libc::SYS_setsockopt as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        setsockopt(memory, state, args)
    } else if number == libc::SYS_getsockopt as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-230): Review bounded SO_TYPE copy-out semantics.
        getsockopt(memory, state, args)
    } else if number == libc::SYS_bind as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        bind(memory, state, args)
    } else if number == libc::SYS_listen as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        listen(state, args)
    } else if number == libc::SYS_getsockname as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        getsockname(memory, state, args)
    } else if number == libc::SYS_socketpair as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        socketpair(memory, state, args)
    } else if number == libc::SYS_connect as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        connect(memory, state, args)
    } else if number == libc::SYS_shutdown as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        shutdown(state, args)
    } else if number == libc::SYS_sendto as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sendto(memory, state, args)
    } else if number == libc::SYS_recvfrom as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        recvfrom(memory, state, args)
    } else if number == libc::SYS_sendmsg as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sendmsg(memory, state, args)
    } else if number == libc::SYS_recvmsg as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        recvmsg(memory, state, args)
    } else if number == libc::SYS_recvmmsg as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        recvmmsg(memory, state, args)
    } else if number == libc::SYS_ioctl as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        ioctl(memory, state, args, capture_output)
    } else if number == libc::SYS_dup as u64 {
        duplicate_fd(state, args[0], None, 0, false)
    } else if number == libc::SYS_dup2 as u64 {
        duplicate_fd(state, args[0], Some(args[1]), 0, false)
    } else if number == libc::SYS_dup3 as u64 {
        duplicate_fd(state, args[0], Some(args[1]), args[2], true)
    } else if number == libc::SYS_fcntl as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fcntl(memory, state, args)
    } else if number == libc::SYS_open as u64 {
        open(memory, state, args, capture_output)
    } else if number == libc::SYS_openat as u64 {
        openat(memory, state, args, capture_output)
    } else if number == libc::SYS_creat as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        creat(memory, state, args, capture_output)
    } else if number == libc::SYS_memfd_create as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        memfd_create(memory, state, args)
    } else if number == libc::SYS_fstat as u64 {
        fstat(memory, state, args, capture_metadata)
    } else if number == libc::SYS_stat as u64 {
        path_stat(memory, state, args, 0, capture_metadata)
    } else if number == libc::SYS_lstat as u64 {
        path_stat(
            memory,
            state,
            args,
            libc::AT_SYMLINK_NOFOLLOW,
            capture_metadata,
        )
    } else if number == libc::SYS_newfstatat as u64 {
        newfstatat(memory, state, args, capture_metadata)
    } else if number == libc::SYS_statx as u64 {
        statx(memory, state, args, capture_metadata)
    } else if number == libc::SYS_statfs as u64 {
        statfs(memory, state, args)
    } else if number == libc::SYS_fstatfs as u64 {
        fstatfs(memory, state, args)
    } else if number == libc::SYS_access as u64 {
        access(memory, state, args)
    } else if number == libc::SYS_faccessat as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        faccessat(memory, state, args)
    } else if number == libc::SYS_faccessat2 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        faccessat2(memory, state, args)
    } else if number == libc::SYS_mkdir as u64 {
        mkdir_at(memory, state, libc::AT_FDCWD, args[0], args[1])
    } else if number == libc::SYS_mkdirat as u64 {
        mkdir_at(memory, state, args[0] as libc::c_int, args[1], args[2])
    } else if number == libc::SYS_unlink as u64 {
        unlink_at(memory, state, libc::AT_FDCWD, args[0], 0)
    } else if number == libc::SYS_rmdir as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-111): Review rmdir->unlinkat(AT_REMOVEDIR) mapping.
        // rmdir(path) is unlinkat(AT_FDCWD, path, AT_REMOVEDIR). Without this
        // arm the guest's bare rmdir(2) fell through to ENOSYS, so coreutils
        // `rmdir` (and `mktemp -d` cleanup) failed under the KVM backend even
        // though `rm -rf`, which uses unlinkat(AT_REMOVEDIR), worked.
        unlink_at(
            memory,
            state,
            libc::AT_FDCWD,
            args[0],
            libc::AT_REMOVEDIR as u64,
        )
    } else if number == libc::SYS_unlinkat as u64 {
        unlink_at(memory, state, args[0] as libc::c_int, args[1], args[2])
    } else if number == libc::SYS_rename as u64 {
        rename_at(
            memory,
            state,
            libc::AT_FDCWD,
            args[0],
            libc::AT_FDCWD,
            args[1],
            0,
        )
    } else if number == libc::SYS_renameat as u64 {
        rename_at(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[2] as libc::c_int,
            args[3],
            0,
        )
    } else if number == libc::SYS_renameat2 as u64 {
        rename_at(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[2] as libc::c_int,
            args[3],
            args[4],
        )
    } else if number == libc::SYS_link as u64 {
        link_at(
            memory,
            state,
            libc::AT_FDCWD,
            args[0],
            libc::AT_FDCWD,
            args[1],
            0,
        )
    } else if number == libc::SYS_linkat as u64 {
        link_at(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[2] as libc::c_int,
            args[3],
            args[4],
        )
    } else if number == libc::SYS_symlink as u64 {
        symlink_at(memory, state, args[0], libc::AT_FDCWD, args[1])
    } else if number == libc::SYS_symlinkat as u64 {
        symlink_at(memory, state, args[0], args[1] as libc::c_int, args[2])
    } else if number == libc::SYS_getcwd as u64 {
        getcwd(memory, state, args)
    } else if number == libc::SYS_chdir as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        chdir(memory, state, args)
    } else if number == libc::SYS_fchdir as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchdir(state, args)
    } else if number == libc::SYS_getdents64 as u64 {
        getdents64(memory, state, args)
    } else if number == libc::SYS_getpid as u64 {
        i64::from(state.pid)
    } else if number == libc::SYS_gettid as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-132): Review distinct KVM thread IDs.
        i64::from(state.tid)
    } else if number == libc::SYS_getppid as u64 {
        i64::from(state.ppid)
    } else if number == libc::SYS_getpgrp as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-92): Review virtual process-group identity.
        i64::from(state.pgid)
    } else if number == libc::SYS_wait4 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        wait4(memory, state, args)
    } else if number == libc::SYS_waitid as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review serialized-child waitid emulation.
        waitid(memory, state, args)
    } else if number == libc::SYS_getuid as u64
        || number == libc::SYS_geteuid as u64
        || number == libc::SYS_getgid as u64
        || number == libc::SYS_getegid as u64
    {
        0
    } else if number == libc::SYS_getresuid as u64 || number == libc::SYS_getresgid as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        get_fixed_root_ids(memory, args)
    } else if is_credential_identity_noop_syscall(number) {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(rrnewton/reverie#354): the credential-setting
        // family (setuid/setgid and their re-/res-/fs- variants, and setgroups) is
        // a deterministic no-op success under Detcore's fixed virtual-root
        // identity -- getuid/geteuid/getgid/getegid are virtualized to 0 and no
        // credential change is ever tracked -- so each returns 0: the value a real
        // root process gets for a permitted change, and the previous virtual-0
        // fs-id for setfsuid/setfsgid. This mirrors detcore's golden
        // is_credential_identity_noop_syscall / Ok(0) arm so privilege-dropping
        // guests proceed identically under KVM and the ptrace backend instead of
        // seeing EPERM (the old setres* arm) or ENOSYS (the rest). The result is
        // never forwarded to the host and is bitwise-identical across --verify and
        // record/replay.
        0
    } else if number == libc::SYS_getgroups as u64 {
        getgroups(memory, args)
    } else if number == libc::SYS_capget as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        capget(memory, state, args)
    } else if number == libc::SYS_capset as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        capset(memory, state, args)
    } else if number == libc::SYS_flock as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        flock(state, args)
    } else if number == libc::SYS_fchown as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchown(state, args[0])
    } else if number == libc::SYS_chown as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchownat(memory, state, libc::AT_FDCWD, args[0], 0)
    } else if number == libc::SYS_lchown as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchownat(
            memory,
            state,
            libc::AT_FDCWD,
            args[0],
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } else if number == libc::SYS_fchownat as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchownat(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[4] as libc::c_int,
        )
    } else if number == libc::SYS_listxattr as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        path_xattr_list(state, memory, args[0], false)
    } else if number == libc::SYS_llistxattr as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        path_xattr_list(state, memory, args[0], true)
    } else if number == libc::SYS_flistxattr as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fd_xattr_list(state, args[0])
    } else if number == libc::SYS_umask as u64 {
        let previous = state.umask;
        state.umask = args[0] as libc::mode_t & 0o777;
        i64::from(previous)
    } else if number == libc::SYS_fchmod as u64 {
        fchmod(state, args)
    } else if number == libc::SYS_chmod as u64 {
        fchmodat(memory, state, libc::AT_FDCWD, args[0], args[1], 0)
    } else if number == libc::SYS_fchmodat as u64 {
        // SYS_fchmodat has three arguments; r10 is unspecified guest state.
        fchmodat(memory, state, args[0] as libc::c_int, args[1], args[2], 0)
    } else if number == libc::SYS_fchmodat2 as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        fchmodat(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[2],
            args[3],
        )
    } else if number == libc::SYS_mknod as u64 {
        mknod_at(memory, state, libc::AT_FDCWD, args[0], args[1], args[2])
    } else if number == libc::SYS_mknodat as u64 {
        mknod_at(
            memory,
            state,
            args[0] as libc::c_int,
            args[1],
            args[2],
            args[3],
        )
    } else if number == libc::SYS_utimensat as u64 {
        utimensat(memory, state, args)
    } else if number == libc::SYS_arch_prctl as u64 {
        return arch_prctl(memory, state, args);
    } else if number == libc::SYS_prctl as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        prctl(memory, state, args)
    } else if number == libc::SYS_getxattr as u64
        || number == libc::SYS_lgetxattr as u64
        || number == libc::SYS_fgetxattr as u64
    {
        // AUTONOMOUS-BOT-IMPLEMENTED
        getxattr(memory, state, number, args)
    } else if number == libc::SYS_setxattr as u64
        || number == libc::SYS_lsetxattr as u64
        || number == libc::SYS_fsetxattr as u64
    {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review deterministic xattr mutation refusal.
        setxattr(memory, state, number, args)
    } else if number == libc::SYS_removexattr as u64
        || number == libc::SYS_lremovexattr as u64
        || number == libc::SYS_fremovexattr as u64
    {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review deterministic xattr removal result.
        removexattr(memory, state, number, args)
    } else if number == libc::SYS_brk as u64 {
        brk(memory, state, args[0])
    } else if number == libc::SYS_mmap as u64 {
        mmap(memory, state, args)
    } else if number == libc::SYS_munmap as u64 {
        munmap(memory, args[0], args[1])
    } else if number == libc::SYS_msync as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-227): Review in-memory mapping synchronization semantics.
        msync(memory, args)
    } else if number == libc::SYS_mremap as u64 {
        mremap(memory, state, args)
    } else if number == libc::SYS_mprotect as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        mprotect(memory, args)
    } else if number == libc::SYS_madvise as u64 {
        validate_range(memory, args[0], args[1])
    } else if number == libc::SYS_munlock as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        munlock_guest_range(memory, args[0], args[1])
    } else if number == libc::SYS_munlockall as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-145): Review the deterministic no-op unlock-all model.
        0
    } else if number == libc::SYS_mincore as u64 {
        mincore(memory, args)
    } else if number == libc::SYS_getcpu as u64 {
        getcpu(memory, args)
    } else if number == libc::SYS_sched_getaffinity as u64 {
        sched_getaffinity(memory, state, args)
    } else if number == libc::SYS_sched_getscheduler as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_getscheduler(state, args)
    } else if number == libc::SYS_sched_setscheduler as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_setscheduler(memory, state, args)
    } else if number == libc::SYS_sched_getparam as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_getparam(memory, state, args)
    } else if number == libc::SYS_sched_setparam as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_setparam(memory, state, args)
    } else if number == libc::SYS_sched_getattr as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_getattr(memory, state, args)
    } else if number == libc::SYS_sched_get_priority_min as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_priority_bound(args[0], false)
    } else if number == libc::SYS_sched_get_priority_max as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        sched_priority_bound(args[0], true)
    } else if number == libc::SYS_getpriority as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        getpriority(state, args)
    } else if number == libc::SYS_setpriority as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        setpriority(state, args)
    } else if number == libc::SYS_ioprio_get as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        ioprio_get(state, args)
    } else if number == libc::SYS_ioprio_set as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        ioprio_set(state, args)
    } else if number == libc::SYS_membarrier as u64 {
        match (args[0] as libc::c_int, args[1] as libc::c_uint) {
            (0, 0) => i64::from(MEMBARRIER_SUPPORTED),
            (1, 0) => 0,
            _ => negative_errno(libc::EINVAL),
        }
    } else if number == libc::SYS_getrandom as u64 {
        getrandom(memory, state.tid, args[0], args[1])
    } else if number == libc::SYS_clock_gettime as u64 {
        clock_gettime(memory, state, args)
    } else if number == libc::SYS_nanosleep as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        nanosleep(memory, args)
    } else if number == libc::SYS_clock_nanosleep as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        clock_nanosleep(memory, args)
    } else if number == libc::SYS_gettimeofday as u64 {
        gettimeofday(memory, args)
    } else if number == libc::SYS_readlink as u64 {
        readlink(memory, state, args, capture_metadata)
    } else if number == libc::SYS_readlinkat as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        readlinkat(memory, state, args, capture_metadata)
    } else if number == libc::SYS_uname as u64 {
        uname(memory, args[0])
    } else if number == libc::SYS_prlimit64 as u64 {
        prlimit64(memory, args)
    } else if number == libc::SYS_rt_sigaction as u64 {
        rt_sigaction(memory, state, args)
    } else if number == libc::SYS_rt_sigprocmask as u64 {
        rt_sigprocmask(memory, state, args)
    } else if number == libc::SYS_rt_sigpending as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        rt_sigpending(memory, state, args)
    } else if number == libc::SYS_sigaltstack as u64 {
        sigaltstack(memory, state, current_user_stack_pointer, args)
    } else if number == libc::SYS_rt_sigtimedwait as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        rt_sigtimedwait(memory, state, args)
    } else if number == libc::SYS_kill as u64
        || number == libc::SYS_tkill as u64
        || number == libc::SYS_tgkill as u64
    {
        // AUTONOMOUS-BOT-IMPLEMENTED
        return kill_signal(state, number, args);
    } else if number == libc::SYS_close as u64 {
        close(state, args[0])
    } else if number == libc::SYS_close_range as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        close_range(state, args)
    } else if number == libc::SYS_futex as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        futex(memory, args)
    } else if number == libc::SYS_rseq as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-172): Review deterministic rseq feature refusal.
        negative_errno(libc::ENOSYS)
    } else if number == libc::SYS_get_robust_list as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-232): Review task-local robust-list queries.
        get_robust_list(memory, state, args)
    } else if number == libc::SYS_set_robust_list as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-232): Review task-local robust-list registration.
        set_robust_list(state, args)
    } else if number == libc::SYS_seccomp as u64 {
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-344): Review deterministic seccomp unavailability.
        seccomp(args)
    } else {
        negative_errno(libc::ENOSYS)
    };

    continue_with(result)
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 1650..1780; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    fn try_from_elf(state: &LoadedStaticElf) -> std::io::Result<Self> {
        Self::prepare_from_elf(state, false)
    }

    fn same_stdin_entry(&self, state: &LoadedStaticElf) -> bool {
        self.stdin.is_some() == state.stdin.is_some()
            && Arc::ptr_eq(&self.stdin_entry_id, &state.stdin_entry_id)
    }

    fn update_from_elf(&mut self, state: &LoadedStaticElf) -> std::io::Result<()> {
        let same_stdin = self.same_stdin_entry(state);
        // Stage every required clone before taking an authoritative handle.
        // A failed clone leaves the entire previous table intact.
        let mut next = Self::prepare_from_elf(state, same_stdin)?;
        if same_stdin {
            next.stdin = self.stdin.take();
        }
        let retired = std::mem::replace(self, next);
        retired.retire(&state.file_retirement);
        Ok(())
    }

    fn prepare_from_elf(state: &LoadedStaticElf, preserve_stdin: bool) -> std::io::Result<Self> {
        let stdin = if preserve_stdin {
            None
        } else {
            state
                .stdin
                .as_ref()
                .map(|file| state.file_retirement.stage_clone(file))
                .transpose()?
        };
        let files = state
            .files
            .iter()
            .map(|(&fd, file)| Ok((fd, state.file_retirement.stage_clone(file)?)))
            .collect::<std::io::Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(Self {
            stdin: stdin.map(crate::elf::StagedFile::into_file),
            stdin_entry_id: state.stdin_entry_id.clone(),
            files: files
                .into_iter()
                .map(|(fd, file)| (fd, file.into_file()))
                .collect(),
            fd_entry_ids: state.fd_entry_ids.clone(),
            random_device_fds: state.random_device_fds.clone(),
            stdout_alias_fds: state.stdout_alias_fds.clone(),
            stderr_alias_fds: state.stderr_alias_fds.clone(),
            cloexec_fds: state.cloexec_fds.clone(),
            closed_standard_fds: state.closed_standard_fds.clone(),
            proc_files: state.proc_files.clone(),
            fdinfo_files: state.fdinfo_files.clone(),
            signalfd_fds: state
                .process_signals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .signalfd_masks
                .keys()
                .copied()
                .collect(),
            fd_object_inodes: state.fd_object_inodes.clone(),
        })
    }

    // TODO-HUMAN-REVIEW(PR-235): Review stable host-fd preservation during table sync.
    fn install(&self, state: &mut LoadedStaticElf) -> std::io::Result<()> {
        // Finish every required fallible clone before replacing state.
        // An unchanged entry keeps its actual host fd for epoll MOD/DEL. Inode
        // equality cannot prove this: closing and reopening a linked file keeps
        // its filesystem identity but installs a different open description.
        let same_stdin = self.same_stdin_entry(state);
        let stdin = if same_stdin {
            None
        } else {
            self.stdin
                .as_ref()
                .map(|file| state.file_retirement.stage_clone(file))
                .transpose()?
        };
        let mut installed_files = std::collections::BTreeMap::new();
        for (&fd, shared_file) in &self.files {
            let same_entry = state.files.contains_key(&fd)
                && state
                    .fd_entry_ids
                    .get(&fd)
                    .zip(self.fd_entry_ids.get(&fd))
                    .is_some_and(|(previous, shared)| Arc::ptr_eq(previous, shared));
            if !same_entry {
                installed_files.insert(fd, state.file_retirement.stage_clone(shared_file)?);
            }
        }
        let mut installed_files: std::collections::BTreeMap<_, _> = installed_files
            .into_iter()
            .map(|(fd, file)| (fd, file.into_file()))
            .collect();
        let mut previous_files = std::mem::take(&mut state.files);
        for &fd in self.files.keys() {
            installed_files.entry(fd).or_insert_with(|| {
                previous_files
                    .remove(&fd)
                    .expect("unchanged descriptor entry has its original host fd")
            });
        }
        let retired_stdin = if same_stdin {
            None
        } else {
            std::mem::replace(
                &mut state.stdin,
                stdin.map(crate::elf::StagedFile::into_file),
            )
        };
        state.stdin_entry_id.clone_from(&self.stdin_entry_id);
        state.files = installed_files;
        state.file_retirement.retire(
            retired_stdin
                .into_iter()
                .chain(previous_files.into_values()),
        );
        state.fd_entry_ids.clone_from(&self.fd_entry_ids);
        state.random_device_fds.clone_from(&self.random_device_fds);
        state.stdout_alias_fds.clone_from(&self.stdout_alias_fds);
        state.stderr_alias_fds.clone_from(&self.stderr_alias_fds);
        state.cloexec_fds.clone_from(&self.cloexec_fds);
        state
            .closed_standard_fds
            .clone_from(&self.closed_standard_fds);
        state.proc_files.clone_from(&self.proc_files);
        state.fdinfo_files.clone_from(&self.fdinfo_files);
        state.fd_object_inodes.clone_from(&self.fd_object_inodes);
        Ok(())
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 1862..1937; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    /// guest. `heap_base` is the initial program break (`align_up(main_end)`),
    /// so the returned range covers exactly the pages `brk()` has mapped; it is
    /// empty when the guest has never grown its break.
    pub(crate) fn heap_region(&self) -> (u64, u64) {
        (self.state.heap_base, self.state.program_break)
    }

    #[cfg(test)]
    pub(crate) fn new(state: LoadedStaticElf, capture_output: bool) -> Self {
        let output = capture_output.then(CapturedOutput::default);
        Self::with_output(state, output)
    }

    pub(crate) fn with_output(mut state: LoadedStaticElf, output: Option<CapturedOutput>) -> Self {
        let file_table;
        let _retirement = state.file_retirement.hold();
        let transaction = state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        let task_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ensure_registered_with_signals(
                state.tid,
                state.pid,
                state.pgid,
                state.dumpable,
                &state.thread_signals,
            );
        let process_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(state.tid)
            .expect("registered KVM task exists")
            .process_generation;
        let next_pid = state.pid.saturating_add(1);
        let sigchld_auto_reap = Arc::new(AtomicBool::new(sigchld_auto_reaps(&state)));
        let address_space = Arc::new(std::sync::Mutex::new(AddressSpaceState::from_elf(&state)));
        file_table = Arc::new(std::sync::Mutex::new(
            FileTableState::try_from_elf(&state).expect("clone initial KVM file table"),
        ));
        state.fdinfo_table = Arc::downgrade(&file_table);
        let signal_registry = Arc::new(ProcessSignalRegistry::default());
        let signal_binding =
            signal_registry.register(&state, &file_table, process_generation, None);
        let (child_completion_sender, child_completion_receiver) = std::sync::mpsc::channel();
        Self {
            state,
            task_generation,
            process_generation,
            signal_registry,
            signal_binding,
            signal_callback_nonce: 0,
            parked_signals: None,
            completed_signal_effects: Vec::new(),
            signal_effect_raw_result: None,
            address_space,
            file_table,
            output,
            owns_output: true,
            next_pid: Arc::new(AtomicI32::new(next_pid)),
            sigchld_auto_reap,
            pending_processes: std::collections::BTreeMap::new(),
            completed_processes: Vec::new(),
            transferred_processes: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            child_completion_sender,
            child_completion_receiver: Mutex::new(child_completion_receiver),
            process_action: None,
            pending_segment: None,
            current_user_stack_pointer: None,
            exit_status: None,
            exit_group: false,
            clear_child_tid: None,
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 2272..2467; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    pub(crate) fn fork_child(
        &self,
        child_pid: i32,
        clear_sighand: bool,
        share_address_space: bool,
    ) -> crate::Result<Self> {
        // Declare owned child state and its cleanup scope before either guard.
        let mut state;
        let file_table;
        let _child_retirement;
        let _retirement = self.state.file_retirement.hold();
        let transaction = self.state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        if !self
            .state
            .process_signals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .signalfd_masks
            .is_empty()
        {
            return Err(crate::Error::UnexpectedVcpuExit(
                "cannot fork a KVM process while a virtual signalfd is open".to_owned(),
            ));
        }
        state = self.state.try_clone_for_fork_locked(child_pid)?;
        _child_retirement = state.file_retirement.hold();
        if share_address_space {
            state.thp_disabled = self.state.thp_disabled.clone();
        }
        state.dumpable = self.current_dumpable();
        if clear_sighand {
            state
                .process_signals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .dispositions
                .retain(|_, action| action.is_ignored());
        }
        let address_space = Arc::new(std::sync::Mutex::new(AddressSpaceState::from_elf(&state)));
        file_table = Arc::new(std::sync::Mutex::new(FileTableState::try_from_elf(&state)?));
        state.fdinfo_table = Arc::downgrade(&file_table);
        let task_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .register_with_signals(
                state.tid,
                state.pid,
                state.pgid,
                state.dumpable,
                &state.thread_signals,
            );
        let process_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(state.tid)
            .expect("registered KVM task exists")
            .process_generation;
        let sigchld_auto_reap = sigchld_auto_reaps(&state);
        let signal_binding = self.signal_registry.register(
            &state,
            &file_table,
            process_generation,
            Some(reverie::SignalProcessId {
                tgid: reverie::Pid::from_raw(self.state.pid),
                generation: self.process_generation,
            }),
        );
        let (child_completion_sender, child_completion_receiver) = std::sync::mpsc::channel();
        let child = Self {
            state,
            task_generation,
            process_generation,
            signal_registry: self.signal_registry.clone(),
            signal_binding,
            signal_callback_nonce: 0,
            parked_signals: None,
            completed_signal_effects: Vec::new(),
            signal_effect_raw_result: None,
            address_space,
            file_table,
            output: self.output.clone(),
            owns_output: false,
            next_pid: self.next_pid.clone(),
            sigchld_auto_reap: Arc::new(AtomicBool::new(sigchld_auto_reap)),
            pending_processes: std::collections::BTreeMap::new(),
            completed_processes: Vec::new(),
            transferred_processes: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            child_completion_sender,
            child_completion_receiver: Mutex::new(child_completion_receiver),
            process_action: None,
            pending_segment: None,
            current_user_stack_pointer: None,
            exit_status: None,
            exit_group: false,
            clear_child_tid: None,
        };
        Ok(child)
    }

    // TODO-HUMAN-REVIEW(PR-172): Review shared address-space and output ownership.
    pub(crate) fn thread_child(&self, child_tid: i32) -> crate::Result<Self> {
        let observe_ignored = self.state.thread_signals.lock().observe_ignored;
        self.thread_child_with_signal_observation(child_tid, observe_ignored)
    }

    pub(crate) fn thread_child_with_signal_observation(
        &self,
        child_tid: i32,
        observe_ignored: bool,
    ) -> crate::Result<Self> {
        // Declare owned child state and its cleanup scope before either guard.
        let mut state;
        let _child_retirement;
        let _retirement = self.state.file_retirement.hold();
        let transaction = self.state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        if self.has_shared_pending_signal()
            || !self
                .state
                .process_signals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .signalfd_masks
                .is_empty()
        {
            return Err(crate::Error::UnexpectedVcpuExit(
                "cannot create a KVM guest thread while a process-directed signal lacks a selected recipient"
                    .to_owned(),
            ));
        }
        state = self.state.try_clone_for_fork_locked(child_tid)?;
        _child_retirement = state.file_retirement.hold();
        state.pid = self.state.pid;
        state.ppid = self.state.ppid;
        // A CLONE_THREAD worker stays inside the same process, so it inherits the
        // thread group leader's position in the traced process tree.
        state.is_traced_tree_root = self.state.is_traced_tree_root;
        state.dumpable = self.current_dumpable();
        state.thread_group_leader_name = self.state.thread_group_leader_name.clone();
        state.thp_disabled = self.state.thp_disabled.clone();
        state.process_signals = self.state.process_signals.clone();
        state.signal_transaction = self.state.signal_transaction.clone();
        state.thread_signals = self.state.thread_signals.for_clone_thread();
        state.thread_signals.lock().observe_ignored = observe_ignored;
        let task_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .register_with_signals(
                state.tid,
                state.pid,
                state.pgid,
                state.dumpable,
                &state.thread_signals,
            );
        let process_generation = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(state.tid)
            .expect("registered KVM task exists")
            .process_generation;
        let (child_completion_sender, child_completion_receiver) = std::sync::mpsc::channel();
        let child = Self {
            state,
            task_generation,
            process_generation,
            signal_registry: self.signal_registry.clone(),
            signal_binding: self.signal_binding.clone(),
            signal_callback_nonce: 0,
            parked_signals: None,
            completed_signal_effects: Vec::new(),
            signal_effect_raw_result: None,
            address_space: self.address_space.clone(),
            file_table: self.file_table.clone(),
            output: self.output.clone(),
            owns_output: false,
            next_pid: self.next_pid.clone(),
            sigchld_auto_reap: self.sigchld_auto_reap.clone(),
            pending_processes: std::collections::BTreeMap::new(),
            completed_processes: Vec::new(),
            transferred_processes: self.transferred_processes.clone(),
            child_completion_sender,
            child_completion_receiver: Mutex::new(child_completion_receiver),
            process_action: None,
            pending_segment: None,
            current_user_stack_pointer: None,
            exit_status: None,
            exit_group: false,
            clear_child_tid: None,
        };
        Ok(child)
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 2627..2652; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    /// Release only this executor's descriptor references. A live CLONE_FILES
    /// sibling retains the old shared table; clearing its contents would close
    /// that sibling's descriptors too.
    pub(crate) fn release_files_on_exit(&mut self) {
        let _retirement = self.state.file_retirement.hold();
        let files = self.file_table.clone();
        let _files = files.lock().unwrap_or_else(|p| p.into_inner());
        let transaction = self.state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        let stdin = self.state.take_stdin();
        let retired = std::mem::take(&mut self.state.files);
        self.state.fd_entry_ids.clear();
        self.file_table = Arc::new(Mutex::new(FileTableState::default()));
        let action = self.process_action.take();
        drop(_transaction);
        drop(_files);
        self.state
            .file_retirement
            .retire(retired.into_values().chain(stdin));
        if let Some(ProcessAction::Exec {
            executable_file, ..
        }) = action
        {
            self.state.file_retirement.retire_shared(executable_file);
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 2939..2980; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    pub(crate) fn replace_after_exec(&mut self, state: LoadedStaticElf) {
        let _retirement = self.state.file_retirement.hold();
        let file_table = self.file_table.clone();
        let mut files = file_table.lock().expect("KVM file-table lock poisoned");
        let transaction = self.state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        let previous = std::mem::replace(&mut self.state, state);
        let retired = self.state.inherit_process_state_locked(previous);
        self.state.file_retirement.retire(retired);
        let identity = self
            .state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(self.state.tid)
            .expect("execing KVM task is absent from its lifecycle table");
        self.task_generation = identity.generation;
        self.process_generation = identity.process_generation;
        *self
            .address_space
            .lock()
            .expect("KVM address-space lock poisoned") = AddressSpaceState::from_elf(&self.state);
        files
            .update_from_elf(&self.state)
            .expect("clone post-exec KVM file table");
        self.signal_binding.rebind(&self.state, &file_table);
        self.sigchld_auto_reap
            .store(sigchld_auto_reaps(&self.state), Ordering::SeqCst);
        self.pending_segment = None;
        self.exit_status = None;
        self.exit_group = false;
        self.clear_child_tid = None;
        drop(_transaction);
        drop(files);
    }

    pub(crate) fn append_output(&mut self, stdout: Vec<u8>, stderr: Vec<u8>) {
        if let Some(output) = self.output.as_mut() {
            let _ = output.append(false, &stdout);
            let _ = output.append(true, &stderr);
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 4268..4281; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    /// Returns and clears a pending thread-local or group-wide exit.
    pub(crate) fn take_exit(&mut self) -> Option<ProcessExit> {
        let transaction = self.state.signal_transaction.clone();
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        let status = self.exit_status.take()?;
        let group = std::mem::take(&mut self.exit_group);
        let status = self
            .state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .exit(self.state.tid, self.task_generation, status, group);
        Some(ProcessExit { status, group })
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 4314..4323; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    pub(crate) fn take_output(&mut self) -> (Vec<u8>, Vec<u8>) {
        if self.owns_output {
            self.output
                .as_mut()
                .map(CapturedOutput::take)
                .unwrap_or_default()
        } else {
            (Vec::new(), Vec::new())
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 4343..4449; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        let _retirement = self.state.file_retirement.hold();
        if let Some(result) = self.synchronize_wait4(request) {
            return result;
        }
        if let Some(result) = self.synchronize_waitid(request, memory) {
            return result;
        }
        if let Some(result) = self.execute_accept(request, memory) {
            return result;
        }
        // TODO-HUMAN-REVIEW(PR-172): Review CLONE_FILES descriptor-table sharing.
        // Thread children retain private executor state, but synchronize their
        // descriptor mappings through this shared table before every syscall.
        // Descriptor-creating and descriptor-closing operations hold the lock
        // through the update; potentially blocking I/O releases it after the
        // snapshot so QEMU AIO workers can continue to make progress.
        let file_table = self.file_table.clone();
        let mut shared_files = Some(file_table.lock().expect("KVM file-table lock poisoned"));
        if let Err(error) = shared_files
            .as_ref()
            .expect("file-table guard disappeared")
            .install(&mut self.state)
        {
            return io_error(error);
        }
        // install cloned every fdinfo description Arc while holding the
        // current table. Scalar dispatch below therefore retains its entry
        // description even if another thread closes/reuses that guest fd after
        // the release. It may then lock seq and its ONE original target table.
        let mutating_file_table = mutates_file_table(request.number());
        if !mutating_file_table {
            shared_files.take();
            self.state.file_retirement.drain_unlocked();
        }
        if let Some(result) = self.execute_process_action(request, memory) {
            return result;
        }
        // Clones share the underlying MAP_SHARED mapping, so writes through this
        // handle reach the guest; `execute_basic_syscall` needs `&mut` access.
        let mut memory = memory.clone();
        let sigchld_action_before = (request.number() == libc::SYS_rt_sigaction as u64
            && request.args()[0] as libc::c_int == libc::SIGCHLD
            && request.args()[1] != 0)
            .then(|| installed_signal_action(&self.state, libc::SIGCHLD));
        let address_space_syscall = matches!(
            request.number(),
            number if number == libc::SYS_brk as u64
                || number == libc::SYS_mmap as u64
                || number == libc::SYS_mremap as u64
        );
        let action = if address_space_syscall {
            let address_space = self.address_space.clone();
            let mut shared = address_space
                .lock()
                .expect("KVM address-space lock poisoned");
            self.state.program_break = shared.program_break;
            self.state.mmap_base = shared.mmap_base;
            self.state.mmap_next = shared.mmap_next;
            self.state.mmap_limit = shared.mmap_limit;
            let action = execute_basic_syscall_with_output(
                &mut memory,
                &mut self.state,
                request,
                self.current_user_stack_pointer,
                self.output.as_mut(),
            );
            shared.program_break = self.state.program_break;
            shared.mmap_base = self.state.mmap_base;
            shared.mmap_next = self.state.mmap_next;
            shared.mmap_limit = self.state.mmap_limit;
            action
        } else {
            execute_basic_syscall_with_output(
                &mut memory,
                &mut self.state,
                request,
                self.current_user_stack_pointer,
                self.output.as_mut(),
            )
        };
        if let Some(before) = sigchld_action_before {
            let after = installed_signal_action(&self.state, libc::SIGCHLD);
            if after != before {
                self.sigchld_auto_reap
                    .store(sigchld_auto_reaps(&self.state), Ordering::SeqCst);
            }
        }
        if let Some(mut shared_files) = shared_files {
            shared_files
                .update_from_elf(&self.state)
                .expect("clone updated KVM file table");
        }
        match action {
            SyscallAction::Continue { result, segment } => {
                if segment.is_some() {
                    self.pending_segment = segment;
                }
                result
            }
            SyscallAction::Exit(code) => {
                self.exit_status = Some(code);
                self.exit_group = request.number() != libc::SYS_exit as u64;
                0
            }
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 6452..6468; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
// TODO-HUMAN-REVIEW(PR-92): Review this KVM compatibility implementation.
// TODO-HUMAN-REVIEW(PR-114): Review /proc/thread-self/fd guest descriptor resolution.
// TODO-HUMAN-REVIEW(PR-136): Review numeric guest-pid descriptor aliases.
fn guest_fd_path(state: &LoadedStaticElf, path: &[u8]) -> Option<libc::c_int> {
    let numeric_prefix = format!("/proc/{}/fd/", state.pid).into_bytes();
    let suffix = path
        .strip_prefix(b"/dev/fd/")
        .or_else(|| path.strip_prefix(b"/proc/self/fd/"))
        .or_else(|| path.strip_prefix(b"/proc/thread-self/fd/"))
        .or_else(|| path.strip_prefix(numeric_prefix.as_slice()))?;
    if suffix.is_empty() || !suffix.iter().all(u8::is_ascii_digit) {
        return None;
    }
    suffix.iter().try_fold(0_i32, |value, digit| {
        value.checked_mul(10)?.checked_add(i32::from(*digit - b'0'))
    })
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 6496..6616; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
// TODO-HUMAN-REVIEW(PR-136): Review guest descriptor link-target sanitization.
fn guest_fd_link_target(
    state: &LoadedStaticElf,
    guest_fd: libc::c_int,
    capture: Option<CaptureMetadata>,
) -> Result<Vec<u8>, i64> {
    if let (Some(capture), Some(alias)) = (capture, output_alias(state, guest_fd)) {
        return Ok(format!("pipe:[{}]", capture.identity(alias).inode).into_bytes());
    }
    let Some(host_fd) = host_fd(state, guest_fd) else {
        return Err(negative_errno(libc::ENOENT));
    };
    if let Some(description) = state.fdinfo_files.get(&guest_fd) {
        return Ok(description.path.clone());
    }
    if signalfd_mask(state, guest_fd).is_some() {
        return Ok(b"anon_inode:[signalfd]".to_vec());
    }
    if let Some(&inode) = state.proc_files.get(&guest_fd)
        && let Some(path) = synthetic_proc_path_for_inode(inode)
    {
        if let Some(suffix) = path.strip_prefix(b"/proc/self/") {
            let mut numeric = format!("/proc/{}/", state.pid).into_bytes();
            numeric.extend_from_slice(suffix);
            return Ok(numeric);
        }
        return Ok(path.to_vec());
    }
    let target = canonical_fd_path(host_fd)?;
    let target = target.as_os_str().as_bytes();

    // A followed proc-fd path names the same object as fstat. Keep anonymous
    // link targets consistent with that object's identity as well.
    for kind in ["pipe", "socket"] {
        let prefix = format!("{kind}:[");
        if target.starts_with(prefix.as_bytes()) && target.ends_with(b"]") {
            return Ok(format!(
                "{kind}:[{}]",
                guest_object_stat(state, guest_fd, capture)?.st_ino
            )
            .into_bytes());
        }
    }
    Ok(target.to_vec())
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-92): Review guest /dev/fd duplication without supervisor procfs exposure.
// TODO-HUMAN-REVIEW(PR-114): Review fresh open-file-description semantics.
// TODO-HUMAN-REVIEW(PR-136): Review procfd object-identity propagation.
fn open_guest_fd_path(
    state: &mut LoadedStaticElf,
    guest_fd: libc::c_int,
    flags: u64,
    close_on_exec: bool,
) -> i64 {
    if state.fdinfo_files.contains_key(&guest_fd) {
        return negative_errno(libc::ENOSYS);
    }
    let Some(source_host_fd) = host_fd(state, guest_fd) else {
        return negative_errno(libc::ENOENT);
    };
    if flags & libc::O_TMPFILE as u64 == libc::O_TMPFILE as u64 {
        // Keep the existing fail-closed behavior until guest mode/umask can be
        // applied to an anonymous file created through a descriptor path.
        return negative_errno(libc::EINVAL);
    }
    if flags & (libc::O_PATH | libc::O_NOFOLLOW) as u64 == (libc::O_PATH | libc::O_NOFOLLOW) as u64
    {
        // Linux would return an O_PATH handle to the magic link itself. Keeping
        // that real supervisor procfs descriptor would bypass the synthetic
        // guest-fd metadata model and expose host-specific procfs identity.
        return negative_errno(libc::ELOOP);
    }
    if flags & libc::O_PATH as u64 != 0 && signalfd_mask(state, guest_fd).is_some() {
        // Reopening the private eventfd carrier as O_PATH would discard the
        // virtual signalfd identity. Refuse before allocating a guest or host
        // descriptor; ordinary access modes retain Linux's EACCES result.
        return negative_errno(libc::ENOSYS);
    }
    let source_alias = output_alias(state, guest_fd);
    let source_proc_inode = state.proc_files.get(&guest_fd).copied();
    let source_object_inode = guest_fd_object_identity(state, guest_fd);

    // Opening a proc-fd magic link creates a fresh open file description. A
    // descriptor duplication would incorrectly share the source offset and
    // status flags, and would prevent Linux-supported access-mode changes.
    // Resolve only the already-mapped host descriptor, so no supervisor-private
    // descriptor can be named by a guest path.
    let proc_path =
        CString::new(format!("/proc/self/fd/{source_host_fd}")).expect("host fd path has no NUL");
    let host_flags = (flags | libc::O_CLOEXEC as u64) as libc::c_int;
    // SAFETY: proc_path is NUL-terminated and live for the call. The source fd
    // remains owned by state, and Linux validates the requested open flags.
    let reopened = unsafe {
        libc::syscall(
            libc::SYS_openat,
            libc::AT_FDCWD,
            proc_path.as_ptr(),
            host_flags,
            0,
        )
    };
    if reopened < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(reopened as RawFd) };
    let new_fd = insert_file_with_flags(state, file, close_on_exec, source_alias);
    if new_fd >= 0 {
        state
            .fd_object_inodes
            .insert(new_fd as libc::c_int, source_object_inode);
    }
    if new_fd >= 0
        && let Some(inode) = source_proc_inode
    {
        state.proc_files.insert(new_fd as libc::c_int, inode);
    }
    new_fd
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 6766..6802; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn output_alias(state: &LoadedStaticElf, fd: libc::c_int) -> Option<OutputAlias> {
    output_alias_from_sets(
        fd,
        &state.stdout_alias_fds,
        &state.stderr_alias_fds,
        is_open_standard(state, fd),
    )
}

fn output_alias_from_sets(
    fd: libc::c_int,
    stdout: &std::collections::BTreeSet<i32>,
    stderr: &std::collections::BTreeSet<i32>,
    standard: bool,
) -> Option<OutputAlias> {
    if stdout.contains(&fd) || (fd == libc::STDOUT_FILENO && standard) {
        Some(OutputAlias::Stdout)
    } else if stderr.contains(&fd) || (fd == libc::STDERR_FILENO && standard) {
        Some(OutputAlias::Stderr)
    } else {
        None
    }
}

fn set_output_alias(state: &mut LoadedStaticElf, fd: libc::c_int, alias: Option<OutputAlias>) {
    state.stdout_alias_fds.remove(&fd);
    state.stderr_alias_fds.remove(&fd);
    match alias {
        Some(OutputAlias::Stdout) => {
            state.stdout_alias_fds.insert(fd);
        }
        Some(OutputAlias::Stderr) => {
            state.stderr_alias_fds.insert(fd);
        }
        None => {}
    }
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 6896..6928; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
// TODO-HUMAN-REVIEW(PR-136): Review descriptor insertion and identity allocation.
fn insert_file_with_flags(
    state: &mut LoadedStaticElf,
    file: std::fs::File,
    close_on_exec: bool,
    output_alias: Option<OutputAlias>,
) -> i64 {
    let Some(fd) = (0..GUEST_NOFILE_LIMIT)
        .find(|fd| !is_open_standard(state, *fd) && !state.files.contains_key(fd))
    else {
        state.file_retirement.retire([file]);
        return negative_errno(libc::EMFILE);
    };
    let object_inode = match allocate_fd_object_inode(state, &file) {
        Ok(inode) => inode,
        Err(error) => {
            state.file_retirement.retire([file]);
            return error;
        }
    };
    // The selected slot is vacant; this cannot retire a description,
    // including when the caller holds signal_transaction.
    let retired = state.insert_file(fd, file);
    state.file_retirement.retire(retired);
    state.fd_object_inodes.insert(fd, object_inode);
    if close_on_exec {
        state.cloexec_fds.insert(fd);
    } else {
        state.cloexec_fds.remove(&fd);
    }
    set_output_alias(state, fd, output_alias);
    i64::from(fd)
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 7002..7169; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn duplicate_fd_at_or_above(
    state: &mut LoadedStaticElf,
    old_host_fd: RawFd,
    raw_minimum: u64,
    close_on_exec: bool,
    source: DuplicateFdSource,
) -> i64 {
    let _retirement = state.file_retirement.hold();
    let transaction = state.signal_transaction.clone();
    let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
    let minimum = raw_minimum as libc::c_int;
    if !(0..GUEST_NOFILE_LIMIT).contains(&minimum) {
        return negative_errno(libc::EINVAL);
    }
    let Some(fd) = (minimum..GUEST_NOFILE_LIMIT)
        .find(|fd| !is_open_standard(state, *fd) && !state.files.contains_key(fd))
    else {
        return negative_errno(libc::EMFILE);
    };

    // SAFETY: old_host_fd is live. F_DUPFD_CLOEXEC returns an owned
    // descriptor; guest CLOEXEC is modeled independently in cloexec_fds.
    let duplicated = unsafe { libc::fcntl(old_host_fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(duplicated) };
    let retired = state.insert_file(fd, file);
    state.fd_object_inodes.insert(fd, source.object_inode);
    if source.is_random {
        state.random_device_fds.insert(fd);
    }
    replace_signalfd_mask(state, fd, source.signalfd_mask);
    if close_on_exec {
        state.cloexec_fds.insert(fd);
    } else {
        state.cloexec_fds.remove(&fd);
    }
    if (0..=2).contains(&fd) {
        state.closed_standard_fds.insert(fd);
    } else {
        state.closed_standard_fds.remove(&fd);
    }
    set_output_alias(state, fd, source.output_alias);
    if let Some(inode) = source.proc_inode {
        state.proc_files.insert(fd, inode);
    }
    if let Some(description) = source.fdinfo {
        state.fdinfo_files.insert(fd, description);
    }
    drop(_transaction);
    state.file_retirement.retire(retired);
    i64::from(fd)
}

// TODO-HUMAN-REVIEW(PR-136): Review dup object-identity propagation.
fn duplicate_fd(
    state: &mut LoadedStaticElf,
    raw_old_fd: u64,
    raw_new_fd: Option<u64>,
    raw_flags: u64,
    is_dup3: bool,
) -> i64 {
    let _retirement = state.file_retirement.hold();
    let transaction = state.signal_transaction.clone();
    let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
    let flags = raw_flags as libc::c_int;
    if !is_dup3 && flags != 0 {
        return negative_errno(libc::EINVAL);
    }
    if is_dup3 && flags & !libc::O_CLOEXEC != 0 {
        return negative_errno(libc::EINVAL);
    }
    let old_fd = raw_old_fd as libc::c_int;
    let source_alias = output_alias(state, old_fd);
    let source_proc_inode = state.proc_files.get(&old_fd).copied();
    let source_fdinfo = state.fdinfo_files.get(&old_fd).cloned();
    let source_is_random = state.random_device_fds.contains(&old_fd);
    let source_signalfd_mask = signalfd_mask(state, old_fd);
    let Some(old_host_fd) = host_fd(state, old_fd) else {
        return negative_errno(libc::EBADF);
    };
    let source_object_inode = guest_fd_object_identity(state, old_fd);
    let close_on_exec = is_dup3 && flags & libc::O_CLOEXEC != 0;

    let new_fd = match raw_new_fd {
        Some(raw_new_fd) => {
            let new_fd = raw_new_fd as libc::c_int;
            if !(0..GUEST_NOFILE_LIMIT).contains(&new_fd) {
                return negative_errno(libc::EBADF);
            }
            if new_fd == old_fd {
                return if is_dup3 {
                    negative_errno(libc::EINVAL)
                } else {
                    i64::from(new_fd)
                };
            }
            Some(new_fd)
        }
        None => None,
    };

    // SAFETY: old_host_fd names a live descriptor. F_DUPFD_CLOEXEC returns a
    // new owned descriptor and prevents it leaking through a supervisor exec.
    let duplicated = unsafe { libc::fcntl(old_host_fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(duplicated) };
    if let Some(new_fd) = new_fd {
        let retired = state.insert_file(new_fd, file);
        state.fd_object_inodes.insert(new_fd, source_object_inode);
        if source_is_random {
            state.random_device_fds.insert(new_fd);
        } else {
            state.random_device_fds.remove(&new_fd);
        }
        replace_signalfd_mask(state, new_fd, source_signalfd_mask);
        cleanup_fd_object_inodes(state);
        if close_on_exec {
            state.cloexec_fds.insert(new_fd);
        } else {
            state.cloexec_fds.remove(&new_fd);
        }
        if (0..=2).contains(&new_fd) {
            state.closed_standard_fds.insert(new_fd);
        } else {
            state.closed_standard_fds.remove(&new_fd);
        }
        if let Some(description) = source_fdinfo {
            state.fdinfo_files.insert(new_fd, description);
        } else {
            state.fdinfo_files.remove(&new_fd);
        }
        set_output_alias(state, new_fd, source_alias);
        if let Some(inode) = source_proc_inode {
            state.proc_files.insert(new_fd, inode);
        } else {
            state.proc_files.remove(&new_fd);
        }
        drop(_transaction);
        state.file_retirement.retire(retired);
        i64::from(new_fd)
    } else {
        let new_fd = insert_file_with_flags(state, file, close_on_exec, source_alias);
        if new_fd >= 0 {
            state
                .fd_object_inodes
                .insert(new_fd as libc::c_int, source_object_inode);
            if source_is_random {
                state.random_device_fds.insert(new_fd as libc::c_int);
            }
            replace_signalfd_mask(state, new_fd as libc::c_int, source_signalfd_mask);
            if let Some(description) = source_fdinfo {
                state.fdinfo_files.insert(new_fd as i32, description);
            }
        }
        if new_fd >= 0
            && let Some(inode) = source_proc_inode
        {
            state.proc_files.insert(new_fd as libc::c_int, inode);
        }
        new_fd
    }
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 10336..10740; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn fstat(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    capture: Option<CaptureMetadata>,
) -> i64 {
    let Ok(fd) = i32::try_from(args[0]) else {
        return negative_errno(libc::EBADF);
    };
    match guest_object_stat(state, fd, capture) {
        Ok(stat) => write_struct(memory, args[1], &stat),
        Err(error) => error,
    }
}

fn guest_object_stat(
    state: &LoadedStaticElf,
    fd: libc::c_int,
    capture: Option<CaptureMetadata>,
) -> Result<libc::stat, i64> {
    if let (Some(capture), Some(alias)) = (capture, output_alias(state, fd)) {
        return Ok(synthetic_captured_output_stat(capture.identity(alias)));
    }
    let Some(host_fd) = host_fd(state, fd) else {
        return Err(negative_errno(libc::EBADF));
    };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat is writable and host_fd is a standard or owned descriptor.
    if unsafe { libc::fstat(host_fd, stat.as_mut_ptr()) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: fstat initialized stat on success.
    let mut stat = unsafe { stat.assume_init() };
    sanitize_guest_fd_stat(state, fd, &mut stat);
    Ok(stat)
}

// TODO-HUMAN-REVIEW(PR-205): Review synthetic metadata for in-memory captured output.
fn synthetic_captured_output_stat(identity: CaptureObjectIdentity) -> libc::stat {
    // Captured writes never reach the host descriptor, so exposing that
    // descriptor's type, size, or inode leaks the invoking shell into the
    // guest. Model the capture sink as the pipe used by process-based backends.
    // SAFETY: libc::stat is plain-old-data; a zeroed value is valid.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    stat.st_dev = identity.device;
    stat.st_ino = identity.inode;
    stat.st_mode = libc::S_IFIFO | 0o600;
    stat.st_nlink = 1;
    stat.st_uid = 0;
    stat.st_gid = 0;
    stat.st_blksize = PAGE_SIZE as libc::blksize_t;
    sanitize_stat_timestamps(&mut stat);
    stat
}

fn synthetic_captured_output_statx(identity: CaptureObjectIdentity) -> libc::statx {
    let stat = synthetic_captured_output_stat(identity);
    // SAFETY: libc::statx is plain-old-data; a zeroed value is valid.
    let mut extended = unsafe { std::mem::zeroed::<libc::statx>() };
    extended.stx_mask = libc::STATX_BASIC_STATS;
    extended.stx_blksize = stat.st_blksize as u32;
    extended.stx_nlink = stat.st_nlink as u32;
    extended.stx_uid = stat.st_uid;
    extended.stx_gid = stat.st_gid;
    extended.stx_mode = stat.st_mode as u16;
    extended.stx_ino = stat.st_ino;
    extended.stx_dev_major = libc::major(stat.st_dev);
    extended.stx_dev_minor = libc::minor(stat.st_dev);
    extended.stx_atime.tv_sec = stat.st_atime;
    extended.stx_mtime.tv_sec = stat.st_mtime;
    extended.stx_ctime.tv_sec = stat.st_ctime;
    extended
}

fn path_stat(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    flags: libc::c_int,
    capture: Option<CaptureMetadata>,
) -> i64 {
    fstatat_impl(
        memory,
        state,
        libc::AT_FDCWD,
        args[0],
        args[1],
        flags,
        capture,
    )
}

fn newfstatat(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    capture: Option<CaptureMetadata>,
) -> i64 {
    fstatat_impl(
        memory,
        state,
        args[0] as libc::c_int,
        args[1],
        args[2],
        args[3] as libc::c_int,
        capture,
    )
}

fn fstatat_impl(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    guest_dirfd: libc::c_int,
    path_address: u64,
    output_address: u64,
    flags: libc::c_int,
    capture: Option<CaptureMetadata>,
) -> i64 {
    let path = match read_c_string(memory, path_address, 4096) {
        Ok(path) => path,
        Err(error) => return read_c_string_errno(error),
    };
    let allowed_flags = libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT;
    if flags & !allowed_flags != 0 {
        return negative_errno(libc::EINVAL);
    }
    if path.is_empty() && flags & libc::AT_EMPTY_PATH == 0 {
        return negative_errno(libc::ENOENT);
    }
    if let Some(target) = fdinfo_path_target(state, &path) {
        let (tid, fd) = match target {
            Ok(target) => target,
            Err(error) => return error,
        };
        if let Err(error) = fdinfo_target_generation(state, tid, fd, capture.is_some()) {
            return error;
        }
        let path = format!("/proc/{tid}/fdinfo/{fd}");
        return write_struct(
            memory,
            output_address,
            &synthetic_proc_stat(synthetic_proc_inode(path.as_bytes()), 0),
        );
    }
    // Path-addressed synthetic /proc file: synthesize deterministic metadata.
    // synthetic_proc_content returns None for an empty path, so no explicit
    // AT_EMPTY_PATH guard is needed here.
    if let Some(content) = synthetic_proc_content(state, &path) {
        let normalized =
            normalize_proc_path(state, &path).expect("a synthesized /proc path always normalizes");
        let stat = synthetic_proc_stat(synthetic_proc_inode(&normalized), content.len());
        return write_struct(memory, output_address, &stat);
    }

    let guest_path = match guest_fd_metadata(state, &path, flags & libc::AT_SYMLINK_NOFOLLOW != 0) {
        Ok(metadata) => metadata,
        Err(error) => return error,
    };
    if let Some(metadata) = guest_path
        && metadata.no_follow
    {
        let stat = synthetic_guest_fd_symlink_stat(metadata.guest_fd);
        return write_struct(memory, output_address, &stat);
    }
    let descriptor = guest_path
        .map(|metadata| metadata.guest_fd)
        .or_else(|| (path.is_empty() && guest_dirfd != libc::AT_FDCWD).then_some(guest_dirfd));
    if let Some(fd) = descriptor
        && let Some(capture) = capture
        && let Some(alias) = output_alias(state, fd)
    {
        return write_struct(
            memory,
            output_address,
            &synthetic_captured_output_stat(capture.identity(alias)),
        );
    }
    let executable = if flags & libc::AT_SYMLINK_NOFOLLOW == 0 {
        match guest_proc_exe_path(state, &path) {
            Some(true) => match state.executable_file.as_ref() {
                Some(file) => Some(file.as_raw_fd()),
                None => return negative_errno(libc::ENOENT),
            },
            Some(false) => return negative_errno(libc::ENOENT),
            None => None,
        }
    } else {
        None
    };
    let opened_file;
    let host_fd = if let Some(metadata) = guest_path {
        metadata.host_fd
    } else if let Some(descriptor) = executable {
        descriptor
    } else if path.is_empty() {
        let Ok((host_fd, _)) = host_dirfd_and_path(state, guest_dirfd, &path) else {
            return negative_errno(libc::EBADF);
        };
        if !state.proc_files.contains_key(&guest_dirfd)
            && let Err(error) = ensure_fd_not_procfs(host_fd)
        {
            return error;
        }
        host_fd
    } else {
        opened_file = match open_metadata_path(
            state,
            guest_dirfd,
            &path,
            flags & libc::AT_SYMLINK_NOFOLLOW != 0,
        ) {
            Ok(file) => file,
            Err(error) => return error,
        };
        opened_file.as_raw_fd()
    };

    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat is writable and host_fd is live for the call.
    if unsafe { libc::fstat(host_fd, stat.as_mut_ptr()) } != 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: fstat initialized stat on success.
    let mut stat = unsafe { stat.assume_init() };
    if let Some(metadata) = guest_path {
        sanitize_guest_fd_stat(state, metadata.guest_fd, &mut stat);
    } else {
        // AT_EMPTY_PATH stat of a synthetic /proc descriptor.
        let empty_path_proc_inode = path
            .is_empty()
            .then(|| state.proc_files.get(&guest_dirfd).copied())
            .flatten();
        if let Some(inode) = empty_path_proc_inode {
            sanitize_proc_stat(&mut stat, inode);
        } else {
            sanitize_stat_timestamps(&mut stat);
        }
    }
    if executable.is_some() {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&stat).cast::<u8>(),
                std::mem::size_of::<libc::stat>(),
            )
        };
        match memory.copy_to_user(output_address, bytes) {
            Ok(()) => 0,
            Err(_) => negative_errno(libc::EFAULT),
        }
    } else {
        write_struct(memory, output_address, &stat)
    }
}

fn statx(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    capture: Option<CaptureMetadata>,
) -> i64 {
    let path = match read_c_string(memory, args[1], 4096) {
        Ok(path) => path,
        Err(error) => return read_c_string_errno(error),
    };
    let flags = args[2] as libc::c_int;
    let allowed_flags = libc::AT_EMPTY_PATH
        | libc::AT_SYMLINK_NOFOLLOW
        | libc::AT_NO_AUTOMOUNT
        | libc::AT_STATX_SYNC_TYPE;
    if flags & !allowed_flags != 0 {
        return negative_errno(libc::EINVAL);
    }
    if path.is_empty() && flags & libc::AT_EMPTY_PATH == 0 {
        return negative_errno(libc::ENOENT);
    }
    if let Some(target) = fdinfo_path_target(state, &path) {
        if args[3] as libc::c_uint & 0x8000_0000 != 0
            || flags & libc::AT_STATX_SYNC_TYPE == libc::AT_STATX_SYNC_TYPE
        {
            return negative_errno(libc::EINVAL);
        }
        let (tid, fd) = match target {
            Ok(target) => target,
            Err(error) => return error,
        };
        if let Err(error) = fdinfo_target_generation(state, tid, fd, capture.is_some()) {
            return error;
        }
        let path = format!("/proc/{tid}/fdinfo/{fd}");
        return write_struct(
            memory,
            args[4],
            &synthetic_proc_statx(synthetic_proc_inode(path.as_bytes()), 0),
        );
    }
    // Path-addressed synthetic /proc file: synthesize deterministic metadata.
    if !path.is_empty() {
        if let Some(content) = synthetic_proc_content(state, &path) {
            let normalized = normalize_proc_path(state, &path)
                .expect("a synthesized /proc path always normalizes");
            let stx = synthetic_proc_statx(synthetic_proc_inode(&normalized), content.len() as u64);
            return write_struct(memory, args[4], &stx);
        }
    } else if let Some(&inode) = state.proc_files.get(&(args[0] as libc::c_int)) {
        // AT_EMPTY_PATH statx of a synthetic /proc descriptor: report the memfd's
        // (deterministic) size with synthesized identity.
        let size = match host_fd(state, args[0] as libc::c_int) {
            Some(host_fd) => {
                let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
                // SAFETY: stat is writable and host_fd is a live memfd descriptor.
                if unsafe { libc::fstat(host_fd, stat.as_mut_ptr()) } != 0 {
                    return io_error(std::io::Error::last_os_error());
                }
                // SAFETY: fstat initialized stat on success.
                unsafe { stat.assume_init() }.st_size as u64
            }
            None => return negative_errno(libc::EBADF),
        };
        let stx = synthetic_proc_statx(inode, size);
        return write_struct(memory, args[4], &stx);
    }

    let guest_path = match guest_fd_metadata(state, &path, flags & libc::AT_SYMLINK_NOFOLLOW != 0) {
        Ok(metadata) => metadata,
        Err(error) => return error,
    };
    if let Some(metadata) = guest_path
        && metadata.no_follow
    {
        let stat = synthetic_guest_fd_symlink_statx(metadata.guest_fd);
        return write_struct(memory, args[4], &stat);
    }
    let descriptor = guest_path.map(|metadata| metadata.guest_fd).or_else(|| {
        (path.is_empty() && args[0] as libc::c_int != libc::AT_FDCWD)
            .then_some(args[0] as libc::c_int)
    });
    if let Some(fd) = descriptor
        && let Some(capture) = capture
        && let Some(alias) = output_alias(state, fd)
    {
        // Ordinary objects continue through the native statx call below. The
        // capture sink synthesizes fields beyond its reserved identity; validate the kernel's
        // reserved mask bit and mutually exclusive synchronization flags here.
        if args[3] as libc::c_uint & 0x8000_0000 != 0
            || flags & libc::AT_STATX_SYNC_TYPE == libc::AT_STATX_SYNC_TYPE
        {
            return negative_errno(libc::EINVAL);
        }
        return write_struct(
            memory,
            args[4],
            &synthetic_captured_output_statx(capture.identity(alias)),
        );
    }
    let opened_file;
    let host_fd = if let Some(metadata) = guest_path {
        metadata.host_fd
    } else if path.is_empty() {
        let Ok((host_fd, _)) = host_dirfd_and_path(state, args[0] as libc::c_int, &path) else {
            return negative_errno(libc::EBADF);
        };
        if let Err(error) = ensure_fd_not_procfs(host_fd) {
            return error;
        }
        host_fd
    } else {
        opened_file = match open_metadata_path(
            state,
            args[0] as libc::c_int,
            &path,
            flags & libc::AT_SYMLINK_NOFOLLOW != 0,
        ) {
            Ok(file) => file,
            Err(error) => return error,
        };
        opened_file.as_raw_fd()
    };

    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let empty_path = b"\0";
    let statx_flags = libc::AT_EMPTY_PATH | (flags & libc::AT_STATX_SYNC_TYPE);
    // SAFETY: empty_path is NUL-terminated, stat is writable, host_fd is live,
    // and the remaining scalar arguments are passed through unchanged.
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            host_fd,
            empty_path.as_ptr(),
            statx_flags,
            args[3] as libc::c_uint,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: statx initialized stat on success.
    let mut stat = unsafe { stat.assume_init() };
    if let Some(metadata) = guest_path {
        sanitize_guest_fd_statx(state, metadata.guest_fd, &mut stat);
    } else {
        sanitize_statx_timestamps(&mut stat);
    }
    write_struct(memory, args[4], &stat)
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 12189..12192; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
#[cfg(test)]
fn synthetic_guest_fd_object_inode(state: &LoadedStaticElf, guest_fd: libc::c_int) -> u64 {
    guest_fd_object_identity(state, guest_fd).inode
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 12304..12472; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn host_fd(state: &LoadedStaticElf, guest_fd: libc::c_int) -> Option<RawFd> {
    state
        .files
        .get(&guest_fd)
        .map(AsRawFd::as_raw_fd)
        .or_else(|| {
            if !is_open_standard(state, guest_fd) {
                None
            } else if guest_fd == libc::STDIN_FILENO {
                state.stdin.as_ref().map(AsRawFd::as_raw_fd)
            } else {
                Some(guest_fd)
            }
        })
}

// TODO-HUMAN-REVIEW(PR-52): Review KVM guest fcntl compatibility boundaries.
fn fcntl(memory: &GuestMemory, state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let guest_fd = args[0] as libc::c_int;
    let source_alias = output_alias(state, guest_fd);
    let source_proc_inode = state.proc_files.get(&guest_fd).copied();
    let source_fdinfo = state.fdinfo_files.get(&guest_fd).cloned();
    let source_is_random = state.random_device_fds.contains(&guest_fd);
    let source_signalfd_mask = signalfd_mask(state, guest_fd);
    let Some(host_fd) = host_fd(state, guest_fd) else {
        return negative_errno(libc::EBADF);
    };
    let source_object_inode = guest_fd_object_identity(state, guest_fd);
    match args[1] as libc::c_int {
        libc::F_DUPFD => duplicate_fd_at_or_above(
            state,
            host_fd,
            args[2],
            false,
            DuplicateFdSource {
                output_alias: source_alias,
                proc_inode: source_proc_inode,
                fdinfo: source_fdinfo,
                object_inode: source_object_inode,
                is_random: source_is_random,
                signalfd_mask: source_signalfd_mask,
            },
        ),
        libc::F_DUPFD_CLOEXEC => duplicate_fd_at_or_above(
            state,
            host_fd,
            args[2],
            true,
            DuplicateFdSource {
                output_alias: source_alias,
                proc_inode: source_proc_inode,
                fdinfo: source_fdinfo,
                object_inode: source_object_inode,
                is_random: source_is_random,
                signalfd_mask: source_signalfd_mask,
            },
        ),
        libc::F_GETFL => match fd_status_flags(host_fd) {
            Ok(flags) => i64::from(
                flags
                    | if source_fdinfo
                        .as_ref()
                        .is_some_and(|info| info.nofollow_status)
                    {
                        libc::O_NOFOLLOW
                    } else {
                        0
                    },
            ),
            Err(error) => error,
        },
        libc::F_GETFD => {
            if state.cloexec_fds.contains(&guest_fd) {
                i64::from(libc::FD_CLOEXEC)
            } else {
                0
            }
        }
        libc::F_SETFD => {
            if args[2] & libc::FD_CLOEXEC as u64 != 0 {
                state.cloexec_fds.insert(guest_fd);
            } else {
                state.cloexec_fds.remove(&guest_fd);
            }
            0
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(#120): guest F_SETFL applies the
        // kernel-settable file status flags (O_APPEND/O_ASYNC/O_DIRECT/
        // O_NOATIME/O_NONBLOCK) to the backing host descriptor. Access mode and
        // creation flags are silently ignored, matching fcntl(2). Without this,
        // programs that set O_NONBLOCK on a freshly created pipe (e.g. xz)
        // observe ENOSYS and abort.
        libc::F_SETFL => {
            let settable = libc::O_APPEND
                | libc::O_ASYNC
                | libc::O_DIRECT
                | libc::O_NOATIME
                | libc::O_NONBLOCK;
            let flags = args[2] as libc::c_int & settable;
            if source_fdinfo.is_some() && flags & libc::O_DIRECT != 0 {
                // proc fdinfo lacks FMODE_CAN_ODIRECT, unlike its private
                // memfd carrier. Reject before applying any requested status
                // changes, including changes shared by descriptor aliases.
                return negative_errno(libc::EINVAL);
            }
            if source_signalfd_mask.is_some() && flags & libc::O_NONBLOCK == 0 {
                // Blocking virtual signalfd waits require scheduler ownership;
                // reject before changing the shared open-file description.
                return negative_errno(libc::ENOSYS);
            }
            // SAFETY: host_fd names a live descriptor; F_SETFL consumes one int flag word.
            zero_or_errno(unsafe { libc::fcntl(host_fd, libc::F_SETFL, flags) })
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-211): Review KVM advisory-lock forwarding.
        command @ (libc::F_SETLK | libc::F_SETLKW | libc::F_OFD_SETLK | libc::F_OFD_SETLKW) => {
            let mut lock = match read_guest_struct::<libc::flock>(memory, args[2]) {
                Ok(lock) => lock,
                Err(error) => return error,
            };
            // FileTableState synchronizes descriptors with dup(2). Closing any
            // duplicate releases process-associated POSIX locks, whereas OFD
            // locks correctly remain attached to the shared open description.
            let host_command = match command {
                libc::F_SETLK => {
                    lock.l_pid = 0;
                    libc::F_OFD_SETLK
                }
                libc::F_SETLKW => {
                    lock.l_pid = 0;
                    libc::F_OFD_SETLKW
                }
                command => command,
            };
            // SAFETY: host_fd is live and lock is a fully initialized flock value.
            zero_or_errno(unsafe { libc::fcntl(host_fd, host_command, &lock) })
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(rrnewton/reverie#318): Review KVM guest pipe-capacity
        // fcntl forwarding. F_GETPIPE_SZ/F_SETPIPE_SZ read and set the capacity
        // of the real host pipe that backs the guest pipe, matching the golden
        // ptrace backend's syscall behavior. Initial capacity and permission to
        // grow are host-global properties affected by per-user pipe quotas, so
        // deterministic tools must establish their own capacity before exposing
        // it. Without this route the executor fell through to ENOSYS, so a guest
        // querying or setting pipe capacity could not implement that policy.
        libc::F_GETPIPE_SZ => {
            // SAFETY: host_fd names a live descriptor; F_GETPIPE_SZ takes no third argument.
            let result = unsafe { libc::fcntl(host_fd, libc::F_GETPIPE_SZ) };
            if result < 0 {
                io_error(std::io::Error::last_os_error())
            } else {
                i64::from(result)
            }
        }
        libc::F_SETPIPE_SZ => {
            let size = args[2] as libc::c_int;
            // SAFETY: host_fd names a live descriptor; F_SETPIPE_SZ consumes one int size.
            let result = unsafe { libc::fcntl(host_fd, libc::F_SETPIPE_SZ, size) };
            if result < 0 {
                io_error(std::io::Error::last_os_error())
            } else {
                i64::from(result)
            }
        }
        _ => negative_errno(libc::ENOSYS),
    }
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 12497..12535; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
// TODO-HUMAN-REVIEW(PR-136): Review descriptor object-identity cleanup.
fn close(state: &mut LoadedStaticElf, raw_fd: u64) -> i64 {
    let transaction = state.signal_transaction.clone();
    let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(fd) = i32::try_from(raw_fd) else {
        return negative_errno(libc::EBADF);
    };
    if let Some(retired) = state.remove_file(fd) {
        state.cloexec_fds.remove(&fd);
        state.random_device_fds.remove(&fd);
        replace_signalfd_mask(state, fd, None);
        state.proc_files.remove(&fd);
        state.fdinfo_files.remove(&fd);
        state.fd_object_inodes.remove(&fd);
        cleanup_fd_object_inodes(state);
        set_output_alias(state, fd, None);
        drop(_transaction);
        state.file_retirement.retire([retired]);
        return 0;
    }
    if is_open_standard(state, fd) {
        let retired = if fd == libc::STDIN_FILENO {
            state.take_stdin()
        } else {
            None
        };
        state.closed_standard_fds.insert(fd);
        state.cloexec_fds.remove(&fd);
        state.random_device_fds.remove(&fd);
        replace_signalfd_mask(state, fd, None);
        state.fd_object_inodes.remove(&fd);
        cleanup_fd_object_inodes(state);
        set_output_alias(state, fd, None);
        drop(_transaction);
        state.file_retirement.retire(retired);
        return 0;
    }
    negative_errno(libc::EBADF)
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 12587..12630; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
fn close_range(state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;

    let first = args[0];
    let last = args[1];
    let flags = args[2];
    if first > last {
        return negative_errno(libc::EINVAL);
    }
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return negative_errno(libc::EINVAL);
    }

    let in_range = |fd: libc::c_int| {
        let fd = u64::from(fd as u32);
        first <= fd && fd <= last
    };
    let mut targets: Vec<libc::c_int> = state
        .files
        .keys()
        .copied()
        .filter(|&fd| in_range(fd))
        .collect();
    for standard in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if in_range(standard) && is_open_standard(state, standard) {
            targets.push(standard);
        }
    }

    if flags & CLOSE_RANGE_CLOEXEC != 0 {
        for fd in targets {
            state.cloexec_fds.insert(fd);
        }
        return 0;
    }

    for fd in targets {
        // Reuse the single-fd close path so every descriptor side table stays
        // consistent. Each fd is known open here, so close never returns EBADF.
        close(state, u64::from(fd as u32));
    }
    0
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 13903..14058; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
// TODO-HUMAN-REVIEW(PR-136): Review shared readlink guest path handling.
fn readlink(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    capture: Option<CaptureMetadata>,
) -> i64 {
    readlink_at_impl(
        memory,
        state,
        libc::AT_FDCWD,
        args[0],
        args[1],
        args[2],
        capture,
    )
}

// TODO-HUMAN-REVIEW(PR-136): Review readlinkat guest-dirfd and procfd semantics.
fn readlinkat(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    args: &[u64; 6],
    capture: Option<CaptureMetadata>,
) -> i64 {
    readlink_at_impl(
        memory,
        state,
        args[0] as libc::c_int,
        args[1],
        args[2],
        args[3],
        capture,
    )
}

// TODO-HUMAN-REVIEW(PR-136): Review shared readlinkat implementation semantics.
fn readlink_at_impl(
    memory: &mut GuestMemory,
    state: &LoadedStaticElf,
    guest_dirfd: libc::c_int,
    path_address: u64,
    output_address: u64,
    raw_capacity: u64,
    capture: Option<CaptureMetadata>,
) -> i64 {
    let path = match read_c_string(memory, path_address, 4096) {
        Ok(path) => path,
        Err(error) => return read_c_string_errno(error),
    };
    let Ok(requested_capacity) = usize::try_from(raw_capacity) else {
        return negative_errno(libc::EINVAL);
    };
    if requested_capacity == 0 {
        return negative_errno(libc::EINVAL);
    }
    let capacity = requested_capacity.min(MAX_HOST_IO);

    if let Some(guest_fd) = guest_fd_path(state, &path) {
        let target = match guest_fd_link_target(state, guest_fd, capture) {
            Ok(target) => target,
            Err(error) => return error,
        };
        let count = capacity.min(target.len());
        return match memory.write(output_address, &target[..count]) {
            Ok(()) => count as i64,
            Err(_) => negative_errno(libc::EFAULT),
        };
    }

    let normalized = normalize_proc_path(state, &path);
    let executable_target = match guest_proc_exe_path(state, &path) {
        Some(true) => match state.executable_file.as_ref() {
            Some(file) => match canonical_fd_path(file.as_raw_fd()) {
                Ok(path) => Some(path.into_os_string().into_vec()),
                Err(error) => return error,
            },
            None => Some(state.executable_path.as_os_str().as_bytes().to_vec()),
        },
        Some(false) => return negative_errno(libc::ENOENT),
        None => None,
    };
    let proc_link_target: Option<&[u8]> = if let Some(target) = executable_target.as_deref() {
        Some(target)
    } else {
        match normalized.as_deref() {
            Some(b"/proc/self/cwd") => Some(state.cwd.as_os_str().as_bytes()),
            Some(b"/proc/self/root") => Some(b"/"),
            _ => None,
        }
    };
    if let Some(target) = proc_link_target {
        let count = capacity.min(target.len());
        let copied = if executable_target.is_some() {
            memory.copy_to_user(output_address, &target[..count])
        } else {
            memory.write(output_address, &target[..count])
        };
        return match copied {
            Ok(()) => count as i64,
            Err(_) => negative_errno(libc::EFAULT),
        };
    }
    // /proc/self (and /proc/<pid>) is a symlink to the numeric pid directory.
    if path == b"/proc/self" || path == format!("/proc/{}", state.pid).as_bytes() {
        let target = state.pid.to_string().into_bytes();
        let count = capacity.min(target.len());
        return match memory.write(output_address, &target[..count]) {
            Ok(()) => count as i64,
            Err(_) => negative_errno(libc::EFAULT),
        };
    }

    let opened_file;
    let link_fd = if path.is_empty() {
        if guest_dirfd == libc::AT_FDCWD {
            return negative_errno(libc::ENOENT);
        }
        let Some(host_fd) = host_fd(state, guest_dirfd) else {
            return negative_errno(libc::EBADF);
        };
        host_fd
    } else {
        opened_file = match open_metadata_path(state, guest_dirfd, &path, true) {
            Ok(file) => file,
            Err(error) => return error,
        };
        opened_file.as_raw_fd()
    };
    match fd_mode(link_fd) {
        Ok(mode) if mode & libc::S_IFMT == libc::S_IFLNK => {}
        Ok(_) if path.is_empty() => return negative_errno(libc::ENOENT),
        Ok(_) => return negative_errno(libc::EINVAL),
        Err(error) => return error,
    }
    let empty_path = b"\0";
    let mut bytes = vec![0; capacity];
    // SAFETY: empty_path is NUL-terminated, link_fd is an O_PATH descriptor for
    // the symlink itself, and bytes is writable for capacity bytes.
    let count = unsafe {
        libc::readlinkat(
            link_fd,
            empty_path.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if count < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    let count = count as usize;
    match memory.write(output_address, &bytes[..count]) {
        Ok(()) => count as i64,
        Err(_) => negative_errno(libc::EFAULT),
    }
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 15242..15266; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use reverie::syscalls::AddrMut;
    use reverie::syscalls::MemoryAccess;

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    include!("capture_identity_tests.rs");
    include!("pipe_fionread_tests.rs");
    include!("child_exit_signal_tests.rs");
    include!("process_alarm_signal_tests.rs");
    include!("signal_dequeue_tests.rs");

    #[test]
    fn proc_root_identity_requires_qualified_mount_and_directory() {
        let root = std::fs::File::open("/proc").unwrap();
        let mut stat: libc::statx = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::statx(

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 16569..16608; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    #[test]
    fn captured_output_fstat_is_synthetic_and_stable() {
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 0x4000).unwrap();
        let mut output = CapturedOutput::default();
        let mut stats = Vec::new();

        for (fd, address) in [(libc::STDOUT_FILENO, 0x1000), (libc::STDERR_FILENO, 0x1200)] {
            let action = execute_basic_syscall_with_output(
                &mut memory,
                &mut state,
                &SyscallRequest::new(libc::SYS_fstat as u64, [fd as u64, address, 0, 0, 0, 0]),
                None,
                Some(&mut output),
            );
            assert!(matches!(
                action,
                SyscallAction::Continue {
                    result: 0,
                    segment: None
                }
            ));
            let stat: libc::stat = read_struct(&memory, address);
            let identity = output
                .metadata()
                .identity(output_alias(&state, fd).unwrap());
            assert_eq!(
                (stat.st_dev, stat.st_ino),
                (identity.device, identity.inode)
            );
            assert_eq!(stat.st_mode, libc::S_IFIFO | 0o600);
            assert_eq!(stat.st_size, 0);
            assert_eq!(stat.st_blocks, 0);
            assert_eq!(stat.st_mtime, DETERMINISTIC_METADATA_SECONDS);
            stats.push(stat);
        }

        assert_ne!(stats[0].st_ino, stats[1].st_ino);
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 25053..25180; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    fn metadata_call(
        memory: &mut GuestMemory,
        state: &mut LoadedStaticElf,
        capture: Option<&mut CapturedOutput>,
        number: libc::c_long,
        args: [u64; 6],
    ) -> i64 {
        if let Some(output) = capture {
            syscall_result_with_output(memory, state, output, number, args)
        } else {
            syscall_result(memory, state, number, args)
        }
    }

    fn assert_descriptor_stat_routes(
        memory: &mut GuestMemory,
        state: &mut LoadedStaticElf,
        fd: libc::c_int,
        mut capture: Option<&mut CapturedOutput>,
    ) -> libc::stat {
        const PATH: u64 = 0x100;
        const STAT: u64 = 0x800;
        const STATX: u64 = 0x1000;
        assert_eq!(
            metadata_call(
                memory,
                state,
                capture.as_deref_mut(),
                libc::SYS_fstat,
                [fd as u64, STAT, 0, 0, 0, 0]
            ),
            0
        );
        let direct: libc::stat = read_struct(memory, STAT);
        for path in [
            format!("/dev/fd/{fd}"),
            format!("/proc/self/fd/{fd}"),
            format!("/proc/thread-self/fd/{fd}"),
            format!("/proc/{}/fd/{fd}", state.pid),
            String::new(),
        ] {
            write_c_string(memory, PATH, &path);
            let (dirfd, flags) = if path.is_empty() {
                (fd, libc::AT_EMPTY_PATH)
            } else {
                (libc::AT_FDCWD, 0)
            };
            assert_eq!(
                metadata_call(
                    memory,
                    state,
                    capture.as_deref_mut(),
                    libc::SYS_newfstatat,
                    [dirfd as u64, PATH, STAT, flags as u64, 0, 0]
                ),
                0,
                "{path}"
            );
            let via_path: libc::stat = read_struct(memory, STAT);
            assert_eq!(
                (
                    via_path.st_dev,
                    via_path.st_ino,
                    via_path.st_mode,
                    via_path.st_size
                ),
                (direct.st_dev, direct.st_ino, direct.st_mode, direct.st_size),
                "{path}"
            );
            assert_eq!(
                metadata_call(
                    memory,
                    state,
                    capture.as_deref_mut(),
                    libc::SYS_statx,
                    [
                        dirfd as u64,
                        PATH,
                        flags as u64,
                        libc::STATX_BASIC_STATS as u64,
                        STATX,
                        0
                    ]
                ),
                0
            );
            let extended: libc::statx = read_struct(memory, STATX);
            assert_eq!(
                extended.stx_mask & (libc::STATX_TYPE | libc::STATX_INO | libc::STATX_SIZE),
                libc::STATX_TYPE | libc::STATX_INO | libc::STATX_SIZE
            );
            assert_eq!(
                (
                    extended.stx_dev_major,
                    extended.stx_dev_minor,
                    extended.stx_ino,
                    extended.stx_mode,
                    extended.stx_size
                ),
                (
                    libc::major(direct.st_dev),
                    libc::minor(direct.st_dev),
                    direct.st_ino,
                    direct.st_mode as u16,
                    direct.st_size as u64
                ),
                "{path}"
            );
            if !path.is_empty() {
                assert_eq!(
                    metadata_call(
                        memory,
                        state,
                        capture.as_deref_mut(),
                        libc::SYS_stat,
                        [PATH, STAT, 0, 0, 0, 0]
                    ),
                    0
                );
                let legacy: libc::stat = read_struct(memory, STAT);
                assert_eq!(
                    (legacy.st_dev, legacy.st_ino),
                    (direct.st_dev, direct.st_ino)
                );
            }
        }
        direct
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 27233..27496; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    #[test]
    fn ordinary_proc_fd_stat_routes_preserve_objects_across_aliases_and_replacement() {
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 0x4000).unwrap();
        let first = root.0.join("first");
        let other = root.0.join("other");
        std::fs::write(&first, b"first object").unwrap();
        std::fs::write(&other, b"different object").unwrap();
        let fd = insert_file_with_flags(
            &mut state,
            std::fs::File::open(&first).unwrap(),
            false,
            None,
        ) as i32;
        let reopened = insert_file_with_flags(
            &mut state,
            std::fs::File::open(&first).unwrap(),
            false,
            None,
        ) as i32;
        let distinct = insert_file_with_flags(
            &mut state,
            std::fs::File::open(&other).unwrap(),
            false,
            None,
        ) as i32;
        let duplicate = syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_dup,
            [fd as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(fd >= 0 && reopened >= 0 && distinct >= 0 && duplicate >= 0);
        let native = file_identity_stat(state.files.get(&fd).unwrap()).unwrap();
        for alias in [fd, reopened, duplicate] {
            let stat = assert_descriptor_stat_routes(&mut memory, &mut state, alias, None);
            assert_eq!((stat.st_dev, stat.st_ino), (native.st_dev, native.st_ino));
        }
        let other_stat = assert_descriptor_stat_routes(&mut memory, &mut state, distinct, None);
        assert_ne!(
            (other_stat.st_dev, other_stat.st_ino),
            (native.st_dev, native.st_ino)
        );
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_dup2,
                [distinct as u64, fd as u64, 0, 0, 0, 0]
            ),
            i64::from(fd)
        );
        let replaced = assert_descriptor_stat_routes(&mut memory, &mut state, fd, None);
        assert_eq!(
            (replaced.st_dev, replaced.st_ino),
            (other_stat.st_dev, other_stat.st_ino)
        );
        let retained = assert_descriptor_stat_routes(&mut memory, &mut state, duplicate, None);
        assert_eq!(
            (retained.st_dev, retained.st_ino),
            (native.st_dev, native.st_ino)
        );
        assert_eq!(close(&mut state, fd as u64), 0);
        let reused = insert_file_with_flags(
            &mut state,
            std::fs::File::open(&first).unwrap(),
            false,
            None,
        ) as i32;
        assert_eq!(reused, fd);
        let after_close = assert_descriptor_stat_routes(&mut memory, &mut state, reused, None);
        assert_eq!(
            (after_close.st_dev, after_close.st_ino),
            (native.st_dev, native.st_ino)
        );
    }

    #[test]
    fn anonymous_proc_fd_links_match_direct_owned_object_stat() {
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 0x4000).unwrap();
        for (number, args, kind) in [
            (libc::SYS_pipe2, [0x1800, 0, 0, 0, 0, 0], "pipe"),
            (
                libc::SYS_socketpair,
                [
                    libc::AF_UNIX as u64,
                    libc::SOCK_STREAM as u64,
                    0,
                    0x1800,
                    0,
                    0,
                ],
                "socket",
            ),
        ] {
            assert_eq!(syscall_result(&mut memory, &mut state, number, args), 0);
            let fds: [i32; 2] = read_struct(&memory, 0x1800);
            for fd in fds {
                let stat = assert_descriptor_stat_routes(&mut memory, &mut state, fd, None);
                let native = file_identity_stat(state.files.get(&fd).unwrap()).unwrap();
                assert_eq!((stat.st_dev, stat.st_ino), (native.st_dev, native.st_ino));
                let expected = format!("{kind}:[{}]", native.st_ino).into_bytes();
                write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{fd}"));
                for capacity in [3, 256] {
                    let count = syscall_result(
                        &mut memory,
                        &mut state,
                        libc::SYS_readlink,
                        [0x100, 0x2000, capacity, 0, 0, 0],
                    );
                    assert_eq!(count as usize, expected.len().min(capacity as usize));
                    let mut bytes = vec![0; count as usize];
                    memory.read(0x2000, &mut bytes).unwrap();
                    assert_eq!(bytes, expected[..count as usize]);
                }
            }
        }
    }

    #[test]
    fn captured_output_stat_and_link_routes_preserve_the_capture_sink() {
        let mut output = CapturedOutput::default();
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 0x4000).unwrap();
        let path = root.0.join("host-output");
        std::fs::write(&path, b"this host file is not the captured pipe").unwrap();
        let alias = insert_file_with_flags(
            &mut state,
            std::fs::File::open(&path).unwrap(),
            false,
            Some(OutputAlias::Stdout),
        ) as i32;
        assert!(alias >= 0);
        let ordinary = assert_descriptor_stat_routes(&mut memory, &mut state, alias, None);
        assert_eq!(ordinary.st_mode & libc::S_IFMT, libc::S_IFREG);
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO, alias] {
            let captured =
                assert_descriptor_stat_routes(&mut memory, &mut state, fd, Some(&mut output));
            assert_eq!(captured.st_mode & libc::S_IFMT, libc::S_IFIFO);
            assert_eq!(captured.st_size, 0);
            let identity = output
                .metadata()
                .identity(output_alias(&state, fd).unwrap());
            assert_eq!(
                (captured.st_dev, captured.st_ino),
                (identity.device, identity.inode)
            );
            write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{fd}"));
            let expected = format!("pipe:[{}]", captured.st_ino).into_bytes();
            let count = metadata_call(
                &mut memory,
                &mut state,
                Some(&mut output),
                libc::SYS_readlinkat,
                [libc::AT_FDCWD as u64, 0x100, 0x2000, 256, 0, 0],
            );
            assert_eq!(count as usize, expected.len());
            let mut target = vec![0; count as usize];
            memory.read(0x2000, &mut target).unwrap();
            assert_eq!(target, expected);
            assert_eq!(
                metadata_call(
                    &mut memory,
                    &mut state,
                    Some(&mut output),
                    libc::SYS_lstat,
                    [0x100, 0x800, 0, 0, 0, 0]
                ),
                0
            );
            let link: libc::stat = read_struct(&memory, 0x800);
            assert_eq!(link.st_mode & libc::S_IFMT, libc::S_IFLNK);
        }
    }

    #[test]
    fn proc_fd_statx_preserves_native_masks_and_error_results() {
        let mut output = CapturedOutput::default();
        let root = TestDir::new();
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 0x4000).unwrap();
        let path = root.0.join("statx-object");
        std::fs::write(&path, b"object").unwrap();
        let fd = insert_file_with_flags(
            &mut state,
            std::fs::File::open(path).unwrap(),
            false,
            Some(OutputAlias::Stdout),
        ) as i32;
        let host_fd = host_fd(&state, fd).unwrap();
        write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{fd}"));
        for (flags, mask) in [
            (0, 0),
            (0, libc::STATX_INO),
            (libc::AT_STATX_FORCE_SYNC, libc::STATX_BASIC_STATS),
            (libc::AT_STATX_SYNC_TYPE, libc::STATX_BASIC_STATS),
            (0, 0x8000_0000),
        ] {
            let mut native = std::mem::MaybeUninit::<libc::statx>::zeroed();
            // SAFETY: the owned fd is live, the empty path is terminated, and
            // native is writable; Linux validates the actual flags and mask.
            let result = unsafe {
                libc::statx(
                    host_fd,
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH | flags,
                    mask,
                    native.as_mut_ptr(),
                )
            };
            let expected = if result == 0 {
                0
            } else {
                io_error(std::io::Error::last_os_error())
            };
            let actual = metadata_call(
                &mut memory,
                &mut state,
                None,
                libc::SYS_statx,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    flags as u64,
                    mask as u64,
                    0x1000,
                    0,
                ],
            );
            assert_eq!(actual, expected, "flags={flags:#x} mask={mask:#x}");
            if result == 0 {
                // SAFETY: the native statx call succeeded and initialized it.
                let native = unsafe { native.assume_init() };
                let guest: libc::statx = read_struct(&memory, 0x1000);
                assert_eq!(guest.stx_mask, native.stx_mask);
                assert_eq!(
                    (guest.stx_ino, guest.stx_dev_major, guest.stx_dev_minor),
                    (native.stx_ino, native.stx_dev_major, native.stx_dev_minor)
                );
            } else {
                assert_eq!(
                    metadata_call(
                        &mut memory,
                        &mut state,
                        Some(&mut output),
                        libc::SYS_statx,
                        [
                            libc::AT_FDCWD as u64,
                            0x100,
                            flags as u64,
                            mask as u64,
                            0x1000,
                            0
                        ]
                    ),
                    expected
                );
            }
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/executor.rs lines 39588..39634; full file SHA256 6e66a34ca883d7cc2e3409e8f99121f6d5f3f88a508631920d159cbaa63000c9
    #[test]
    fn proc_executable_link_marks_unlinked_and_replaced_files_deleted() {
        let proc_executable_link_target = |state: &LoadedStaticElf| {
            let mut memory = GuestMemory::new(0, 8192).unwrap();
            memory.write(0x100, b"/proc/self/exe\0").unwrap();
            memory.write(0x1000, &[0xa5; 4096]).unwrap();
            let count = readlink_at_impl(
                &mut memory,
                state,
                libc::AT_FDCWD,
                0x100,
                0x1000,
                4096,
                None,
            );
            assert!((0..4096).contains(&count));
            let mut output = [0; 4096];
            memory.read(0x1000, &mut output).unwrap();
            assert_eq!(&output[count as usize..], &[0xa5; 4096][count as usize..]);
            output[..count as usize].to_vec()
        };
        let dir = TestDir::new();
        let executable = dir.0.join("program");
        std::fs::write(&executable, b"first").unwrap();
        let mut state = test_state(&dir.0);
        state.executable_path = executable.clone();
        state.executable_file = Some(Arc::new(std::fs::File::open(&executable).unwrap()));
        assert_eq!(
            proc_executable_link_target(&state),
            executable.as_os_str().as_bytes(),
        );

        let replacement = dir.0.join("replacement");
        std::fs::write(&replacement, b"second").unwrap();
        std::fs::rename(&replacement, &executable).unwrap();
        let expected = format!("{} (deleted)", executable.display());
        assert_eq!(proc_executable_link_target(&state), expected.as_bytes());

        std::fs::remove_file(&executable).unwrap();
        assert_eq!(proc_executable_link_target(&state), expected.as_bytes());
        state.executable_file = None;
        assert_eq!(
            proc_executable_link_target(&state),
            executable.as_os_str().as_bytes(),
            "the embedding API's unknown initial identity stays best effort",
        );
    }

