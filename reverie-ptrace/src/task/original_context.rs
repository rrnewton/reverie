//! Shared custody of an original native SECCOMP entry and its real EXIT.
//! Operand updates can change seccomp policy: Linux may recheck the new tuple.
use super::*;

fn raw_arguments(args: SyscallArgs) -> [u64; 6] {
    [
        args.arg0 as u64,
        args.arg1 as u64,
        args.arg2 as u64,
        args.arg3 as u64,
        args.arg4 as u64,
        args.arg5 as u64,
    ]
}
pub(super) fn check_entry(
    task: &Stopped,
    nr: Sysno,
    args: SyscallArgs,
    ip: u64,
    sp: u64,
    seccomp: bool,
) -> Result<(), TraceError> {
    let entry = task.syscall_entry()?;
    if entry.arch != 0xc000003e
        || entry.number != nr as u64
        || entry.arguments != raw_arguments(args)
        || entry.instruction_pointer != ip
        || entry.stack_pointer != sp
        || entry.seccomp != seccomp
    {
        return Err(Errno::EPROTO.into());
    }
    Ok(())
}
impl<L: Tool + 'static> TracedTask<L> {
    /// Take only this same native syscall, authenticating the complete original
    /// tuple before changing operands and the requested tuple afterward. No
    /// resume, signal dispatch, or helper occurs between these observations.
    pub(super) fn take_original_entry(
        &mut self,
        nr: Sysno,
        requested: SyscallArgs,
        expected_original: Option<SyscallArgs>,
    ) -> Result<Option<(Stopped, Option<libc::user_regs_struct>)>, TraceError> {
        if self.injected_syscall_frame.is_some() || self.pending_syscall_already_skipped {
            return Ok(None);
        }
        let Some((pending_nr, original_args)) = self.pending_syscall else {
            return Ok(None);
        };
        if pending_nr != nr {
            return Ok(None);
        }
        if expected_original.is_some_and(|expected| expected != original_args) {
            return Err(Errno::EPROTO.into());
        }
        let task = self.assume_stopped();
        let original = task.getregs()?;
        check_entry(
            &task,
            nr,
            original_args,
            original.ip(),
            original.stack_ptr(),
            true,
        )?;
        let context = if original_args != requested {
            let mut actual = original;
            actual.set_args((
                requested.arg0 as u64,
                requested.arg1 as u64,
                requested.arg2 as u64,
                requested.arg3 as u64,
                requested.arg4 as u64,
                requested.arg5 as u64,
            ));
            task.setregs(&actual)?;
            check_entry(
                &task,
                nr,
                requested,
                original.ip(),
                original.stack_ptr(),
                true,
            )?;
            Some(original)
        } else {
            None
        };
        self.pending_syscall = None;
        self.original_read_entry = None;
        Ok(Some((task, context)))
    }

    /// This is an original-entry operation, not ordinary helper injection.
    /// A missing/wrong/skipped/rewritten entry is a backend error. The provider
    /// separately authenticates the actual post-filter copy and same-Call result;
    /// raw EBADF alone supplies no copy receipt. DEL performs no event copy.
    pub(super) async fn inject_epoll_ctl_copy_entry(
        &mut self,
        call: reverie::syscalls::EpollCtl,
    ) -> Result<Result<i64, Errno>, TraceError> {
        if self.interrupted_read.is_some() || self.pending_signal.is_some() {
            return Err(Errno::EPROTO.into());
        }
        let (nr, original) = call.into_parts();
        let mut requested = original;
        requested.arg0 = -1isize as usize;
        requested.arg2 = -1isize as usize;
        let (task, context) = self
            .take_original_entry(nr, requested, Some(original))?
            .ok_or(Errno::EPROTO)?;
        let observe = L::observe_injected_syscalls(&self.global_state.cfg);
        let observation = observe.then_some((nr, requested));
        let preparation = (observe
            && L::observe_injected_syscall_preparation(&self.global_state.cfg))
        .then_some((nr, requested));
        self.observe_injected_syscall(preparation, InjectedSyscallEvent::Prepared);
        self.observe_injected_syscall(observation, InjectedSyscallEvent::Entered);
        self.finish_entered_original(task, nr, requested, context, observation)
            .await
    }

    pub(super) async fn finish_entered_original(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
        context: Option<libc::user_regs_struct>,
        observation: Option<(Sysno, SyscallArgs)>,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let entered = task.getregs()?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Syscall) => {
                let raw = stopped.syscall_exit_result()?;
                let regs = stopped.getregs()?;
                if regs.orig_syscall() != nr as u64
                    || regs.args() != entered.args()
                    || regs.ip() != entered.ip()
                    || regs.stack_ptr() != entered.stack_ptr()
                    || regs.ret() as i64 != raw
                {
                    return Err(Errno::EPROTO.into());
                }
                self.observe_injected_syscall(observation, InjectedSyscallEvent::Returned(raw));
                if let Some(context) = context {
                    if self.injected_syscall_frame.is_some() {
                        stopped.setregs(&context)?;
                    } else {
                        restore_context(&stopped, context, None, false)?;
                    }
                }
                let result = Errno::from_ret(raw as usize).map(|value| value as i64);
                self.observe_liteinst_mapping_result(nr, args, result);
                Ok(result)
            }
            Wait::Exited(_, status) => self.exit(status).await,
            // An unexpected stop is not a raw result and never becomes a
            // synthetic restart errno. Actual final task cleanup retains it.
            other => self.abort(Ok(other)).await,
        }
    }
}

/// Privately retained at the actual unconverted seccomp callback. Numeric
/// operands obtained later from a Tool are never a replacement for this stop.
pub(super) struct OriginalReadEntry {
    nr: Sysno,
    entry: safeptrace::SyscallEntry,
    failure: StdMutex<Option<String>>,
}

#[cfg(test)]
std::thread_local! {
    static ENTRY_REGISTER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The syscall ABI and NT_PRSTATUS view are separate kernel facts. A changed
/// saved CS can select the 68-byte compat view even at a native syscall stop.
/// Never decode a short register set, in debug or release builds.
fn checked_entry_registers(task: &Stopped) -> Result<libc::user_regs_struct, String> {
    let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    let mut iov = libc::iovec {
        iov_base: (&mut regs as *mut libc::user_regs_struct).cast(),
        iov_len: std::mem::size_of_val(&regs),
    };
    #[cfg(test)]
    ENTRY_REGISTER_READS.with(|reads| reads.set(reads.get() + 1));
    let raw = unsafe {
        libc::syscall(
            libc::SYS_ptrace,
            libc::PTRACE_GETREGSET,
            task.pid().as_raw(),
            libc::NT_PRSTATUS,
            &mut iov as *mut libc::iovec,
        )
    };
    let error = (raw == -1).then(std::io::Error::last_os_error);
    if raw != 0 {
        return Err(format!(
            "original native Read register acquisition failed: raw={raw}, error={error:?}"
        ));
    }
    if iov.iov_len != std::mem::size_of_val(&regs) {
        return Err(format!(
            "original native Read register view has {} bytes, expected {}",
            iov.iov_len,
            std::mem::size_of_val(&regs)
        ));
    }
    Ok(regs)
}

impl OriginalReadEntry {
    pub(super) fn capture(task: &Stopped, nr: Sysno, args: SyscallArgs) -> Result<Self, String> {
        let entry = task.syscall_entry().map_err(|error| error.to_string())?;
        if entry.arch != 0xc000003e {
            return Err(format!(
                "original Read range backend does not support syscall ABI {:#x}",
                entry.arch
            ));
        }
        let supported = nr == Sysno::read
            || (nr == Sysno::recvfrom && args.arg3 == 0 && args.arg4 == 0 && args.arg5 == 0);
        if !supported
            || entry.number != nr as u64
            || entry.arguments != raw_arguments(args)
            || !entry.seccomp
        {
            return Err("original native range inspection requires the same seccomp Read".into());
        }
        // Preserve Read's eager independent register-view qualification. For
        // recvfrom, the immutable safeptrace entry above is retained before
        // Tool dispatch and `check` performs the independent register read at
        // the first actual inspection. Ordinary recvfrom calls that no Tool
        // inspects therefore do not perturb Read's observation accounting.
        if nr == Sysno::read {
            let regs = checked_entry_registers(task)?;
            check_entry(task, nr, args, regs.ip(), regs.stack_ptr(), true)
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            nr,
            entry,
            failure: StdMutex::new(None),
        })
    }

    fn check(&self, task: &Stopped, nr: Sysno, args: SyscallArgs) -> Result<(), String> {
        if nr != self.nr {
            return Err("original native Read syscall kind changed".into());
        }
        let actual = task.syscall_entry().map_err(|error| error.to_string())?;
        if actual != self.entry || actual.arguments != raw_arguments(args) {
            return Err("original native Read entry changed".into());
        }
        let regs = checked_entry_registers(task)?;
        check_entry(
            task,
            nr,
            args,
            self.entry.instruction_pointer,
            self.entry.stack_pointer,
            true,
        )
        .map_err(|error| error.to_string())?;
        if regs.orig_syscall() != nr as u64
            || regs.ip() != self.entry.instruction_pointer
            || regs.stack_ptr() != self.entry.stack_pointer
            || regs.args()
                != (
                    args.arg0 as u64,
                    args.arg1 as u64,
                    args.arg2 as u64,
                    args.arg3 as u64,
                    args.arg4 as u64,
                    args.arg5 as u64,
                )
        {
            return Err("original native Read registers changed".into());
        }
        Ok(())
    }
}

const HOST_STATUS_LIMIT: usize = 64 * 1024;

fn check_range_host_status(bytes: &[u8], tid: libc::pid_t, pid: libc::pid_t) -> Result<(), String> {
    if bytes.len() > HOST_STATUS_LIMIT {
        return Err("native range host status exceeds its bound".into());
    }
    if bytes.last() != Some(&b'\n') || bytes.contains(&0) {
        return Err("native range host status framing is incomplete".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "native range host status is not UTF-8".to_owned())?;
    for (field, expected) in [
        ("Pid:", tid as u64),
        ("Tgid:", pid as u64),
        ("Seccomp:", 0),
        ("Seccomp_filters:", 0),
    ] {
        let mut values = text.lines().filter_map(|line| line.strip_prefix(field));
        let value = values
            .next()
            .ok_or_else(|| format!("native range host status lacks {field}"))?;
        let value = value.trim();
        if values.next().is_some()
            || value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || value.parse::<u64>().ok() != Some(expected)
        {
            return Err(format!(
                "native range host status has wrong or repeated {field}"
            ));
        }
    }
    Ok(())
}

/// Corroborate the actual calling thread through bounded kernel status bytes.
/// This is a supported-host precondition, not protection from a hostile host
/// able to fabricate every syscall or rewrite the control process itself.
fn require_unfiltered_range_host() -> Result<(), String> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    if unsafe { libc::prctl(libc::PR_GET_SECCOMP) } != 0 {
        return Err("native range oracle cannot classify a seccomp-filtered host query".into());
    }
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
    let pid = unsafe { libc::getpid() };
    if tid <= 0 || pid <= 0 {
        return Err("native range host identity unavailable".into());
    }
    let status = std::fs::File::open("/proc/thread-self/status")
        .map_err(|error| format!("native range host status unavailable: {error}"))?;
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(status.as_raw_fd(), &mut fs) } != 0
        || fs.f_type != libc::PROC_SUPER_MAGIC
    {
        return Err("native range host status is not an authenticated procfs file".into());
    }
    let before = status
        .metadata()
        .map_err(|error| format!("native range host status metadata: {error}"))?;
    if before.mode() & libc::S_IFMT != libc::S_IFREG {
        return Err("native range host status is not a regular procfs file".into());
    }
    let mut bytes = Vec::new();
    (&status)
        .take(HOST_STATUS_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("native range host status unreadable: {error}"))?;
    let after = status
        .metadata()
        .map_err(|error| format!("native range host status metadata: {error}"))?;
    if (before.dev(), before.ino(), before.mode()) != (after.dev(), after.ino(), after.mode()) {
        return Err("native range host status identity changed".into());
    }
    check_range_host_status(&bytes, tid, pid)?;
    if unsafe { libc::syscall(libc::SYS_gettid) } != tid as libc::c_long
        || unsafe { libc::getpid() } != pid
        || unsafe { libc::prctl(libc::PR_GET_SECCOMP) } != 0
    {
        return Err("native range host changed during qualification".into());
    }
    Ok(())
}

fn range_metadata_matches(local: &[libc::iovec; 2], address: usize, length: usize) -> bool {
    // Inspect only our initialized metadata, never the guest pointee. Volatile
    // readback makes the before/after custody check explicit at the boundary.
    let actual = unsafe { std::ptr::read_volatile(local) };
    actual[0].iov_base as usize == address
        && actual[0].iov_len == length
        && actual[1].iov_base.is_null()
        && actual[1].iov_len == 0
}

#[derive(Default)]
struct ReadRangeOracle {
    failure: Option<String>,
    #[cfg(test)]
    native_queries: usize,
    #[cfg(test)]
    last_query: Option<(usize, usize, usize)>,
}

impl ReadRangeOracle {
    fn retain_failure(&mut self, failure: String) -> String {
        self.failure.get_or_insert(failure).clone()
    }

    fn finish_native_result(
        &mut self,
        raw: libc::c_long,
        errno: Option<i32>,
        entire_range: bool,
    ) -> Result<reverie::OriginalReadRangeVerdict, String> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        match (raw, errno, entire_range) {
            (0, None, true) => Ok(reverie::OriginalReadRangeVerdict::Allowed),
            (-1, Some(libc::EFAULT), _) => Ok(reverie::OriginalReadRangeVerdict::Fault),
            // An accepted prefix never qualifies a longer range. EINVAL and
            // all other outcomes are unknown, not guest address faults.
            other => Err(self.retain_failure(format!("unexpected native range result: {other:?}"))),
        }
    }

    fn inspect(
        &mut self,
        address: usize,
        count: usize,
    ) -> Result<reverie::OriginalReadRangeVerdict, String> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        match self.inspect_inner(address, count) {
            Ok(verdict) => Ok(verdict),
            Err(error) => Err(self.retain_failure(error)),
        }
    }

    fn inspect_inner(
        &mut self,
        address: usize,
        count: usize,
    ) -> Result<reverie::OriginalReadRangeVerdict, String> {
        require_unfiltered_range_host()?;
        let length = count.min(isize::MAX as usize);
        let local = [
            libc::iovec {
                iov_base: address as *mut libc::c_void,
                iov_len: length,
            },
            libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            },
        ];
        if !range_metadata_matches(&local, address, length) {
            return Err("native range local metadata changed before query".into());
        }
        #[cfg(test)]
        {
            self.native_queries += 1;
            self.last_query = Some((address, count, local[0].iov_len));
        }
        // Two vectors select generic import, which checks each original length
        // before MAX_RW_COUNT clamping (the single-vector path does not).
        // Zero remote vectors return after import, before target lookup, MM
        // access, LSM checks or any payload copy. No guest slice/reference or
        // file/socket descriptor is used. The original Read remains stopped.
        let raw = unsafe {
            libc::syscall(
                libc::SYS_process_vm_readv,
                0,
                local.as_ptr(),
                local.len(),
                std::ptr::null::<libc::iovec>(),
                0usize,
                0usize,
            )
        };
        let errno = (raw == -1).then(|| unsafe { *libc::__errno_location() });
        if !range_metadata_matches(&local, address, length) {
            return Err("native range local metadata changed during query".into());
        }
        require_unfiltered_range_host()?;
        // Generic import rejects signed-negative iov_len before access_ok.
        // For a larger raw count, query its representable prefix instead:
        // actual prefix failure implies full-range failure; prefix success
        // supplies no verdict about the longer range and refuses above.
        self.finish_native_result(raw, errno, length == count)
    }
}

static READ_RANGE_ORACLE: StdOnceLock<StdMutex<ReadRangeOracle>> = StdOnceLock::new();

impl<L: Tool + 'static> TracedTask<L> {
    fn inspect_native_scalar_receive_range(
        &self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<reverie::OriginalReadRangeVerdict, reverie::Error> {
        let check = || -> Result<_, String> {
            let entry = self
                .original_read_entry
                .as_ref()
                .ok_or_else(|| "no retained original native Read context".to_owned())?
                .as_ref()
                .map_err(Clone::clone)?;
            // A refusal belongs to this original entry, including wrong caller
            // operands and changed IP/SP. Restoring them cannot erase its first
            // failure or submit another range query. A fresh entry is separate.
            let mut failure = entry
                .failure
                .lock()
                .map_err(|_| "original native Read inspection lock poisoned".to_owned())?;
            if let Some(primary) = &*failure {
                return Err(primary.clone());
            }
            let inspect = || -> Result<_, String> {
                if self.injected_syscall_frame.is_some()
                    || self.pending_syscall_already_skipped
                    || self.interrupted_read.is_some()
                    || self.pending_signal.is_some()
                    || self.pending_syscall != Some((nr, args))
                {
                    return Err("no matching unconsumed original native Read entry".into());
                }
                let task = self.assume_stopped();
                entry.check(&task, nr, args)?;
                let mut oracle = READ_RANGE_ORACLE
                    .get_or_init(|| StdMutex::new(ReadRangeOracle::default()))
                    .lock()
                    .map_err(|_| "native range oracle lock poisoned".to_owned())?;
                let verdict = oracle.inspect(args.arg1, args.arg2);
                let unchanged = entry.check(&task, nr, args);
                match verdict {
                    Err(primary) => Err(primary),
                    Ok(verdict) => {
                        unchanged?;
                        Ok(verdict)
                    }
                }
            };
            match inspect() {
                Ok(verdict) => Ok(verdict),
                Err(primary) => Err(failure.get_or_insert(primary).clone()),
            }
        };
        check().map_err(|error| reverie::Error::Tool(anyhow::anyhow!(error)))
    }

    pub(super) fn inspect_native_read_range(
        &self,
        read: reverie::syscalls::Read,
    ) -> Result<reverie::OriginalReadRangeVerdict, reverie::Error> {
        let (nr, args) = read.into_parts();
        self.inspect_native_scalar_receive_range(nr, args)
    }

    pub(super) fn inspect_native_recvfrom_range(
        &self,
        receive: reverie::syscalls::Recvfrom,
    ) -> Result<reverie::OriginalReadRangeVerdict, reverie::Error> {
        let (nr, args) = receive.into_parts();
        self.inspect_native_scalar_receive_range(nr, args)
    }
}

#[cfg(test)]
mod tests {
    use reverie::OriginalReadRangeVerdict;
    use serde::Deserialize;
    use serde::Serialize;

    use super::*;

    type RangeObservation = (
        Result<OriginalReadRangeVerdict, String>,
        bool,
        Result<i64, Errno>,
    );

    #[derive(Default)]
    struct RangeLog {
        observations: StdMutex<Vec<RangeObservation>>,
        entry_refusals: AtomicUsize,
        after_completion_refusals: AtomicUsize,
        foreign_fd_refusals: AtomicUsize,
        entry_initial_allowed: AtomicUsize,
        recvfrom_allowed: AtomicUsize,
        recvfrom_refusals: AtomicUsize,
    }

    #[reverie::global_tool]
    impl GlobalTool for RangeLog {
        type Config = ();
        type Request = ();
        type Response = ();
        async fn receive_rpc(&self, _: Pid, _: ()) {}
    }

    #[derive(Default, Serialize, Deserialize)]
    struct RangeState;
    impl AsRef<RangeState> for RangeState {
        fn as_ref(&self) -> &Self {
            self
        }
    }
    impl AsMut<RangeState> for RangeState {
        fn as_mut(&mut self) -> &mut Self {
            self
        }
    }
    #[derive(Default)]
    struct RangeTool;
    impl AsMut<RangeTool> for RangeTool {
        fn as_mut(&mut self) -> &mut Self {
            self
        }
    }

    fn registers(r: libc::user_regs_struct) -> [u64; 27] {
        [
            r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx,
            r.rdx, r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base,
            r.gs_base, r.ds, r.es, r.fs, r.gs,
        ]
    }

    // Native-tracer GETREGS/SETREGS use the fixed 216-byte view on x86-64,
    // even when saved CS selects a compat NT_PRSTATUS view. Test-only: all
    // product acquisition uses the fallible exact-size GETREGSET above.
    fn native_view_registers(pid: libc::pid_t) -> libc::user_regs_struct {
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_GETREGS,
                    pid,
                    std::ptr::null_mut::<libc::c_void>(),
                    &mut regs,
                )
            },
            0
        );
        regs
    }

    fn restore_native_view_registers(pid: libc::pid_t, regs: &libc::user_regs_struct) {
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_SETREGS,
                    pid,
                    std::ptr::null_mut::<libc::c_void>(),
                    regs,
                )
            },
            0
        );
    }

    fn native_entry_bytes(pid: libc::pid_t) -> ([u8; 88], libc::c_long) {
        let mut bytes = [0u8; 88];
        let size = unsafe {
            libc::ptrace(
                libc::PTRACE_GET_SYSCALL_INFO,
                pid,
                bytes.len(),
                bytes.as_mut_ptr(),
            )
        };
        assert!(size >= 84 && size <= bytes.len() as libc::c_long);
        (bytes, size)
    }

    struct CompatTracee {
        pid: libc::pid_t,
        pidfd: i32,
        reaped: bool,
    }

    impl CompatTracee {
        fn wait(&mut self) -> i32 {
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(self.pid, &mut status, 0) };
                if waited == -1 && unsafe { *libc::__errno_location() } == libc::EINTR {
                    continue;
                }
                assert_eq!(waited, self.pid);
                self.reaped = libc::WIFEXITED(status) || libc::WIFSIGNALED(status);
                return status;
            }
        }
    }
    impl Drop for CompatTracee {
        fn drop(&mut self) {
            if !self.reaped {
                // Atomic clone3 PIDFD custody: no numeric PID signal, and no
                // unowned child interval even if a later fixture assertion fails.
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        self.pidfd,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0usize,
                    );
                }
                loop {
                    let mut status = 0;
                    let waited = unsafe { libc::waitpid(self.pid, &mut status, 0) };
                    if waited == -1 && unsafe { *libc::__errno_location() } == libc::EINTR {
                        continue;
                    }
                    if waited != self.pid || libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                        break;
                    }
                }
            }
            unsafe {
                libc::close(self.pidfd);
            }
        }
    }

    fn real_compat_read_capture_refusal() {
        let mut pidfd = -1;
        let mut clone: libc::clone_args = unsafe { std::mem::zeroed() };
        clone.flags = libc::CLONE_PIDFD as u64;
        clone.pidfd = (&mut pidfd as *mut i32) as u64;
        clone.exit_signal = libc::SIGCHLD as u64;
        let child =
            unsafe { libc::syscall(libc::SYS_clone3, &clone, std::mem::size_of_val(&clone)) };
        assert!(child >= 0, "clone3 with atomic pidfd custody failed");
        if child == 0 {
            // Child-only, stack-owned filter. No Rust allocation or unwinding
            // after cloning. This traces actual i386 Read via int80; all other
            // child syscalls remain allowed, and the parent is never filtered.
            unsafe {
                if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                    libc::_exit(71);
                }
                if libc::kill(libc::getpid(), libc::SIGSTOP) != 0 {
                    libc::_exit(72);
                }
                let mut instructions = [
                    libc::sock_filter {
                        code: 0x20,
                        jt: 0,
                        jf: 0,
                        k: 4,
                    },
                    libc::sock_filter {
                        code: 0x15,
                        jt: 0,
                        jf: 3,
                        k: 0x40000003,
                    },
                    libc::sock_filter {
                        code: 0x20,
                        jt: 0,
                        jf: 0,
                        k: 0,
                    },
                    libc::sock_filter {
                        code: 0x15,
                        jt: 0,
                        jf: 1,
                        k: 3,
                    },
                    libc::sock_filter {
                        code: 0x06,
                        jt: 0,
                        jf: 0,
                        k: libc::SECCOMP_RET_TRACE,
                    },
                    libc::sock_filter {
                        code: 0x06,
                        jt: 0,
                        jf: 0,
                        k: libc::SECCOMP_RET_ALLOW,
                    },
                ];
                let program = libc::sock_fprog {
                    len: instructions.len() as u16,
                    filter: instructions.as_mut_ptr(),
                };
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    libc::_exit(73);
                }
                if libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0 {
                    libc::_exit(74);
                }
                let raw: i32;
                std::arch::asm!("push rbx", "mov ebx, -1", "int 0x80", "pop rbx",
                    inlateout("eax") 3i32 => raw,
                    inlateout("ecx") 0usize => _, inlateout("edx") 0usize => _,
                    lateout("r8") _, lateout("r9") _, lateout("r10") _, lateout("r11") _);
                libc::_exit(if raw == -libc::EBADF { 0 } else { 75 });
            }
        }
        let mut child = CompatTracee {
            pid: child as libc::pid_t,
            pidfd,
            reaped: false,
        };
        assert!(child.pidfd >= 0);
        let initial = child.wait();
        assert!(libc::WIFSTOPPED(initial));
        assert_eq!(libc::WSTOPSIG(initial), libc::SIGSTOP);
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_SETOPTIONS,
                    child.pid,
                    0,
                    libc::PTRACE_O_TRACESECCOMP | libc::PTRACE_O_EXITKILL,
                )
            },
            0
        );
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_CONT, child.pid, 0, 0) },
            0
        );
        let status = child.wait();
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGTRAP);
        assert_eq!(status >> 16, libc::PTRACE_EVENT_SECCOMP);
        let stopped = Stopped::new_unchecked(Pid::from_raw(child.pid));
        let entry = stopped.syscall_entry().unwrap();
        assert_eq!(entry.arch, 0x40000003);
        assert_eq!(entry.number, 3);
        assert!(entry.seccomp);
        assert_eq!(entry.arguments[0], u32::MAX as u64);
        assert_eq!(entry.arguments[1], 0);
        assert_eq!(entry.arguments[2], 0);
        let before_entry = native_entry_bytes(child.pid);
        let before_regs = registers(native_view_registers(child.pid));
        let queries = range_queries();
        let register_reads = ENTRY_REGISTER_READS.with(|reads| reads.get());
        let args = SyscallArgs {
            arg0: entry.arguments[0] as usize,
            arg1: entry.arguments[1] as usize,
            arg2: entry.arguments[2] as usize,
            arg3: entry.arguments[3] as usize,
            arg4: entry.arguments[4] as usize,
            arg5: entry.arguments[5] as usize,
        };
        // This directly qualifies the shared original-entry capture boundary,
        // not tracer-wide support for executing a compat Guest.
        let primary = OriginalReadEntry::capture(&stopped, Sysno::read, args)
            .err()
            .unwrap();
        assert_eq!(
            primary,
            "original Read range backend does not support syscall ABI 0x40000003"
        );
        assert_eq!(
            ENTRY_REGISTER_READS.with(|reads| reads.get()),
            register_reads
        );
        assert_eq!(range_queries(), queries);
        assert_eq!(stopped.syscall_entry().unwrap(), entry);
        assert_eq!(native_entry_bytes(child.pid), before_entry);
        assert_eq!(registers(native_view_registers(child.pid)), before_regs);
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_CONT, child.pid, 0, 0) },
            0
        );
        let exit = child.wait();
        assert!(libc::WIFEXITED(exit));
        assert_eq!(
            libc::WEXITSTATUS(exit),
            0,
            "actual int80 Read must return exactly -EBADF"
        );
        eprintln!(
            "actual compat entry arch={:#x} nr={} refusal={primary:?} register_reads=0 range_queries=0 original_read=-EBADF natural_exit=0",
            entry.arch, entry.number
        );
    }

    fn range_test_logging() -> std::sync::MutexGuard<'static, ()> {
        static OWNER: StdOnceLock<StdMutex<()>> = StdOnceLock::new();
        let owner = OWNER.get_or_init(|| StdMutex::new(())).lock().unwrap();
        // Preserve testing::run_tokio_test's exact filter and reentrancy. Keep
        // diagnostic events off libtest's named-result stdout, without dropping
        // any event or relaxing the outside owner's exact result comparator.
        let collector = tracing_subscriber::fmt()
            .with_env_filter("reverie=trace")
            .with_writer(std::io::stderr)
            .finish();
        tracing::subscriber::set_global_default(collector).unwrap_or(());
        owner
    }

    fn range_queries() -> usize {
        READ_RANGE_ORACLE
            .get()
            .map_or(0, |oracle| oracle.lock().unwrap().native_queries)
    }

    fn tool_refusal(result: Result<OriginalReadRangeVerdict, reverie::Error>) -> String {
        match result {
            Err(reverie::Error::Tool(error)) => error.to_string(),
            other => panic!("expected typed backend refusal, got {other:?}"),
        }
    }

    #[reverie::tool]
    impl Tool for RangeTool {
        type GlobalState = RangeLog;
        type ThreadState = RangeState;
        fn subscriptions(_: &()) -> Subscription {
            let mut subscriptions = Subscription::none();
            subscriptions.syscalls([Sysno::read, Sysno::recvfrom]);
            subscriptions
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            if let Syscall::Read(read) = call
                && [900, 901, 902].contains(&read.fd())
            {
                let original = guest.regs().await;
                let before = registers(original);
                let (_, original_args) = read.into_parts();
                let verdict = if read.fd() == 901 {
                    assert_eq!(
                        guest.inspect_original_read_range(read).unwrap(),
                        OriginalReadRangeVerdict::Allowed
                    );
                    guest
                        .local_global_state()
                        .unwrap()
                        .entry_initial_allowed
                        .fetch_add(1, Ordering::SeqCst);
                    let queries = range_queries();
                    let variant = guest
                        .local_global_state()
                        .unwrap()
                        .entry_refusals
                        .fetch_add(1, Ordering::SeqCst);
                    assert!(variant < 13);
                    let requested = match variant {
                        0 => read.with_fd(900),
                        1 => read.with_len(9),
                        2 => read.with_buf(None),
                        _ => {
                            let mut changed = original;
                            match variant {
                                3 => changed.orig_rax = Sysno::write as u64,
                                4 => changed.rdi ^= 1,
                                5 => changed.rsi ^= 1,
                                6 => changed.rdx ^= 1,
                                7 => changed.r10 ^= 1,
                                8 => changed.rip += 1,
                                9 => changed.rsp += 8,
                                10 => changed.cs = 0x23,
                                11 => changed.r8 ^= 1,
                                12 => changed.r9 ^= 1,
                                _ => unreachable!(),
                            }
                            guest.set_regs(changed).await?;
                            let applied = if variant == 10 {
                                // CS controls the register view, not syscall_info.arch.
                                let stop = Stopped::new_unchecked(guest.tid());
                                let entry = stop.syscall_entry().unwrap();
                                assert_eq!(entry.arch, 0xc000003e);
                                assert_eq!(entry.number, Sysno::read as u64);
                                assert_eq!(entry.arguments, raw_arguments(original_args));
                                assert!(entry.seccomp);
                                native_view_registers(guest.tid().as_raw())
                            } else {
                                guest.regs().await
                            };
                            assert_eq!(
                                registers(applied),
                                registers(changed),
                                "controlled variant {variant} must actually be applied"
                            );
                            read
                        }
                    };
                    let primary = tool_refusal(guest.inspect_original_read_range(requested));
                    assert_eq!(range_queries(), queries);
                    if variant == 10 {
                        assert_eq!(
                            primary,
                            "original native Read register view has 68 bytes, expected 216"
                        );
                        restore_native_view_registers(guest.tid().as_raw(), &original);
                    } else {
                        guest.set_regs(original).await?;
                    }
                    assert_eq!(registers(guest.regs().await), before);
                    assert_eq!(
                        tool_refusal(guest.inspect_original_read_range(read)),
                        primary
                    );
                    assert_eq!(
                        tool_refusal(guest.inspect_original_read_range(read.with_len(9))),
                        primary
                    );
                    assert_eq!(
                        range_queries(),
                        queries,
                        "restored entry must not submit another query"
                    );
                    eprintln!(
                        "entry variant={variant} primary={primary:?} queries_retained={queries}"
                    );
                    Err(primary)
                } else if read.fd() == 902 {
                    let queries = range_queries();
                    let primary =
                        tool_refusal(guest.inspect_original_read_range(read.with_fd(900)));
                    assert_eq!(primary, "no matching unconsumed original native Read entry");
                    assert_eq!(
                        tool_refusal(guest.inspect_original_read_range(read)),
                        primary
                    );
                    assert_eq!(
                        range_queries(),
                        queries,
                        "foreign descriptor must fail before metadata query"
                    );
                    guest
                        .local_global_state()
                        .unwrap()
                        .foreign_fd_refusals
                        .fetch_add(1, Ordering::SeqCst);
                    Err(primary)
                } else {
                    guest
                        .inspect_original_read_range(read)
                        .map_err(|error| error.to_string())
                };
                let queries_before_forward = range_queries();
                let forwarded = {
                    let wrapped = guest.into_guest();
                    Guest::<RangeTool>::inspect_original_read_range(&wrapped, read)
                        .map_err(|error| error.to_string())
                };
                assert_eq!(forwarded, verdict);
                if verdict.is_err() {
                    assert_eq!(range_queries(), queries_before_forward);
                }
                let query = READ_RANGE_ORACLE
                    .get()
                    .and_then(|oracle| oracle.lock().unwrap().last_query);
                if read.fd() != 902 {
                    assert_eq!(
                        query,
                        Some((
                            original_args.arg1,
                            original_args.arg2,
                            original_args.arg2.min(isize::MAX as usize)
                        ))
                    );
                }
                let unchanged = before == registers(guest.regs().await);
                // Continue the actual original Read even on a refusal.
                let native = guest.inject(read).await;
                eprintln!(
                    "native range actual pointer={:#x} original_count={} query={query:?} verdict={verdict:?} original_read={native:?}",
                    original_args.arg1, original_args.arg2
                );
                if read.fd() == 901 {
                    let missing = tool_refusal(guest.inspect_original_read_range(read));
                    assert_eq!(missing, "no retained original native Read context");
                    guest
                        .local_global_state()
                        .unwrap()
                        .after_completion_refusals
                        .fetch_add(1, Ordering::SeqCst);
                }
                guest
                    .local_global_state()
                    .unwrap()
                    .observations
                    .lock()
                    .unwrap()
                    .push((verdict, unchanged, native));
                return Ok(native?);
            }
            if let Syscall::Recvfrom(receive) = call
                && receive.fd() == 903
            {
                let global = guest.local_global_state().unwrap();
                if receive.flags() == 0 && receive.len() == 1 {
                    assert_eq!(
                        guest.inspect_original_recvfrom_range(receive).unwrap(),
                        OriginalReadRangeVerdict::Allowed
                    );
                    global.recvfrom_allowed.fetch_add(1, Ordering::SeqCst);
                } else {
                    let result = if receive.flags() == 0 {
                        guest.inspect_original_recvfrom_range(receive.with_len(receive.len() + 1))
                    } else {
                        guest.inspect_original_recvfrom_range(receive)
                    };
                    let refusal = tool_refusal(result);
                    if receive.flags() == 0 {
                        assert_eq!(refusal, "no matching unconsumed original native Read entry");
                    } else {
                        assert_eq!(
                            refusal,
                            "original native range inspection requires the same seccomp Read"
                        );
                    }
                    global.recvfrom_refusals.fetch_add(1, Ordering::SeqCst);
                }
                return Ok(guest.inject(receive).await?);
            }
            Ok(guest.inject(call).await?)
        }
    }

    #[test]
    fn original_recvfrom_range_authenticates_only_the_read_equivalent_shape() {
        let _owner = range_test_logging();
        let (output, log) = crate::testing::test_fn::<RangeTool, _>(|| unsafe {
            let mut sockets = [-1; 2];
            assert_eq!(
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                    0,
                    sockets.as_mut_ptr(),
                ),
                0
            );
            assert_eq!(libc::dup3(sockets[0], 903, libc::O_CLOEXEC), 903);
            assert_eq!(libc::close(sockets[0]), 0);
            assert_eq!(libc::send(sockets[1], b"abcd".as_ptr().cast(), 4, 0), 4);
            let mut bytes = [0u8; 4];
            assert_eq!(
                libc::recvfrom(
                    903,
                    bytes.as_mut_ptr().cast(),
                    1,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                1
            );
            assert_eq!(
                libc::recvfrom(
                    903,
                    bytes.as_mut_ptr().add(1).cast(),
                    2,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                2
            );
            assert_eq!(
                libc::recvfrom(
                    903,
                    bytes.as_mut_ptr().add(3).cast(),
                    1,
                    libc::MSG_PEEK,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                1
            );
            assert_eq!(&bytes, b"abcd");
            assert_eq!(libc::close(903), 0);
            assert_eq!(libc::close(sockets[1]), 0);
        })
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(log.recvfrom_allowed.load(Ordering::SeqCst), 1);
        assert_eq!(log.recvfrom_refusals.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn original_read_range_uses_actual_null_device_without_payload_access() {
        let _owner = range_test_logging();
        let (output, log) = crate::testing::test_fn::<RangeTool, _>(|| unsafe {
            let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
            assert!(fd >= 0);
            assert_eq!(libc::dup3(fd, 900, libc::O_CLOEXEC), 900);
            assert_eq!(libc::close(fd), 0);
            let mut bytes = [0xa5u8; 32];
            assert_eq!(
                libc::syscall(libc::SYS_read, 900, bytes.as_mut_ptr().add(8), 8usize),
                0
            );
            assert_eq!(bytes, [0xa5; 32]);
            assert_eq!(libc::close(900), 0);
        })
        .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        let observations = log.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert!(observations[0].1, "inspection must preserve every register");
        assert_eq!(
            observations[0].2,
            Ok(0),
            "actual original Read must continue unchanged"
        );
        assert_eq!(observations[0].0, Ok(OriginalReadRangeVerdict::Allowed));
    }
    // Extend the retained RED RangeLog/RangeTool with these fields/actions; the
    // original first test body and exact final Allowed assertion stay intact.
    // All pointer constants below are probes, never assumed address limits.

    unsafe fn null_at(fd: i32) {
        unsafe {
            let original = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
            assert!(original >= 0);
            assert_eq!(libc::dup3(original, fd, libc::O_CLOEXEC), fd);
            assert_eq!(libc::close(original), 0);
        }
    }

    #[test]
    fn original_read_range_preserves_raw_count_and_sticky_unknown_refusal() {
        let _owner = range_test_logging();
        let (output, log) = crate::testing::test_fn::<RangeTool, _>(|| unsafe {
            null_at(900);
            null_at(904);
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            assert!(page >= 4096);
            let arena = libc::mmap(
                std::ptr::null_mut(),
                page * 3,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(arena, libc::MAP_FAILED);
            let bytes = arena.cast::<u8>();
            std::ptr::write_bytes(bytes, 0xa5, page * 3);
            assert_eq!(
                libc::mprotect(bytes.add(page).cast(), page, libc::PROT_NONE),
                0
            );
            let addresses = [
                0usize,
                1,
                bytes as usize,
                bytes.add(page) as usize,
                bytes.add(page - 4) as usize,
                (1usize << 47) - 1,
                1usize << 47,
                (1usize << 56) - 1,
                1usize << 56,
                0x0100_0000_0000_0001,
                0xffff_ffff_ffff_f000,
                usize::MAX,
            ];
            let counts = [
                0usize,
                1,
                8,
                512,
                0x7fff_f000,
                0x8000_0000,
                isize::MAX as usize,
                isize::MAX as usize + 1,
                usize::MAX,
            ];
            for address in addresses {
                for count in counts {
                    let raw = libc::syscall(libc::SYS_read, 900, address, count);
                    assert!(raw == 0 || raw == -1 && *libc::__errno_location() == libc::EFAULT);
                }
            }
            // Discover the native zero-count boundary through actual original Read
            // continuations on a separate null FD. Constants above are only probes.
            // Then require a pair where premature MAX_RW_COUNT clamping would
            // change Fault to Allowed; this catches the one-iovec fast-path bug.
            let mut low = 0usize;
            let mut high = usize::MAX;
            while low < high {
                let middle = low + (high - low) / 2 + 1;
                let raw = libc::syscall(libc::SYS_read, 904, middle, 0usize);
                if raw == 0 {
                    low = middle;
                } else {
                    assert_eq!(raw, -1);
                    assert_eq!(*libc::__errno_location(), libc::EFAULT);
                    high = middle - 1;
                }
            }
            let max_rw_count = 0x7fff_f000usize;
            assert!(low > max_rw_count);
            let address = low - max_rw_count;
            assert_eq!(libc::syscall(libc::SYS_read, 900, address, max_rw_count), 0);
            assert_eq!(
                libc::syscall(libc::SYS_read, 900, address, max_rw_count + 1),
                -1
            );
            assert_eq!(*libc::__errno_location(), libc::EFAULT);
            assert_eq!(libc::close(904), 0);
            assert_eq!(
                libc::mprotect(
                    bytes.add(page).cast(),
                    page,
                    libc::PROT_READ | libc::PROT_WRITE
                ),
                0
            );
            assert!(
                std::slice::from_raw_parts(bytes, page * 3)
                    .iter()
                    .all(|byte| *byte == 0xa5)
            );
            assert_eq!(libc::munmap(arena, page * 3), 0);
            assert_eq!(libc::close(900), 0);
        })
        .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        let observations = log.observations.lock().unwrap();
        assert_eq!(observations.len(), 110);
        let mut allowed = 0;
        let mut fault = 0;
        for (verdict, unchanged, native) in observations.iter() {
            assert!(unchanged, "inspection changed a real original context");
            match native {
                Ok(0) => {
                    assert_eq!(*verdict, Ok(OriginalReadRangeVerdict::Allowed));
                    allowed += 1;
                }
                Err(Errno::EFAULT) => {
                    assert_eq!(*verdict, Ok(OriginalReadRangeVerdict::Fault));
                    fault += 1;
                }
                other => panic!("unexpected actual original null Read: {other:?}"),
            }
        }
        assert!(allowed > 0 && fault > 0);
        // Explicitly controlled unexpected-result premise. No claimed native
        // positive return/EPERM/EINTR from read_null; exercise the actual one-way
        // production result latch and prove no subsequent native submission.
        for (raw, errno, entire_range) in [
            (1, None, true),
            (-1, Some(libc::EPERM), true),
            (-1, Some(libc::EINTR), true),
            (-1, Some(libc::EINVAL), true),
            (-2, None, true),
            (0, None, false),
        ] {
            let mut oracle = ReadRangeOracle::default();
            assert_eq!(oracle.inspect(0, 0), Ok(OriginalReadRangeVerdict::Allowed));
            let calls = oracle.native_queries;
            let first = oracle
                .finish_native_result(raw, errno, entire_range)
                .unwrap_err();
            assert!(first.contains("unexpected native range result"));
            assert_eq!(
                oracle.finish_native_result(0, None, true),
                Err(first.clone())
            );
            assert_eq!(oracle.inspect(0, 0), Err(first));
            assert_eq!(oracle.native_queries, calls);
        }
    }

    #[test]
    fn original_read_range_refuses_foreign_original_fd_and_observed_host_filter() {
        let _owner = range_test_logging();
        // The metadata oracle has no payload descriptor. An actual different Guest
        // FD still must not replace the retained original Read tuple.
        let (output, log) = crate::testing::test_fn::<RangeTool, _>(|| unsafe {
            null_at(900);
            null_at(902);
            let mut bytes = [0xa5u8; 32];
            assert_eq!(
                libc::syscall(libc::SYS_read, 902, bytes.as_mut_ptr().add(8), 8usize),
                0
            );
            assert_eq!(bytes, [0xa5; 32]);
            assert_eq!(libc::close(900), 0);
            assert_eq!(libc::close(902), 0);
        })
        .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(log.foreign_fd_refusals.load(Ordering::SeqCst), 1);
        let observations = log.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].0,
            Err("no matching unconsumed original native Read entry".into())
        );
        assert!(observations[0].1);
        assert_eq!(observations[0].2, Ok(0));

        // Controlled parser refusals supplement (and do not replace) the real
        // thread-local filter below. No supplied bytes grant kernel provenance.
        let valid = b"Pid:\t11\nTgid:\t7\nSeccomp:\t0\nSeccomp_filters:\t0\n";
        assert_eq!(check_range_host_status(valid, 11, 7), Ok(()));
        for malformed in [
            b"Pid: 11\nTgid: 7\nSeccomp: 0\n".as_slice(),
            b"Pid: 12\nTgid: 7\nSeccomp: 0\nSeccomp_filters: 0\n".as_slice(),
            b"Pid: 11\nTgid: 8\nSeccomp: 0\nSeccomp_filters: 0\n".as_slice(),
            b"Pid: 11\nTgid: 7\nSeccomp: 0\nSeccomp: 0\nSeccomp_filters: 0\n".as_slice(),
            b"Pid: 11\nTgid: 7\nSeccomp: 2\nSeccomp_filters: 1\n".as_slice(),
            b"Pid: 11\nTgid: 7\nSeccomp: zero\nSeccomp_filters: 0\n".as_slice(),
        ] {
            assert!(check_range_host_status(malformed, 11, 7).is_err());
        }
        assert!(check_range_host_status(&vec![b'x'; HOST_STATUS_LIMIT + 1], 11, 7).is_err());
        assert!(check_range_host_status(&valid[..valid.len() - 1], 11, 7).is_err());
        let mut nul = valid.to_vec();
        nul.insert(0, 0);
        assert!(check_range_host_status(&nul, 11, 7).is_err());
        assert!(
            check_range_host_status(
                b"Pid: +11\nTgid: 7\nSeccomp: 0\nSeccomp_filters: 0\n",
                11,
                7
            )
            .is_err()
        );
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_SECCOMP) }, 0);
        for filter_kind in 0..4 {
            std::thread::spawn(move || {
            let ret = |value| libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16, jt: 0, jf: 0, k: value,
            };
            let mut instructions = if filter_kind == 0 {
                vec![ret(libc::SECCOMP_RET_ALLOW)]
            } else {
                // Real controlled policy: PVM EFAULT, PVM zero, or prctl zero.
                // The last case demonstrates why prctl alone cannot qualify an
                // unfiltered thread; the independently read procfs fields do.
                let number = if filter_kind == 3 { libc::SYS_prctl } else { libc::SYS_process_vm_readv };
                let errno = if filter_kind == 1 { libc::EFAULT as u32 } else { 0 };
                vec![
                    libc::sock_filter { code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, jt: 0, jf: 0, k: 0 },
                    libc::sock_filter { code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16, jt: 0, jf: 1, k: number as u32 },
                    ret(libc::SECCOMP_RET_ERRNO | errno),
                    ret(libc::SECCOMP_RET_ALLOW),
                ]
            };
            let program = libc::sock_fprog { len: instructions.len() as u16, filter: instructions.as_mut_ptr() };
            let local = [
                libc::iovec {
                    iov_base: if filter_kind == 2 { usize::MAX as *mut libc::c_void } else { std::ptr::null_mut() },
                    iov_len: if filter_kind == 2 { 1 } else { 0 },
                },
                libc::iovec { iov_base: std::ptr::null_mut(), iov_len: 0 },
            ];
            // Retain the exact private metadata across both calls. Kind2 proves
            // real EFAULT becomes synthetic zero; kind1 proves real zero becomes
            // synthetic EFAULT. No payload access occurs in either baseline.
            let baseline = unsafe { libc::syscall(libc::SYS_process_vm_readv, 0,
                local.as_ptr(), 2usize, std::ptr::null::<libc::iovec>(), 0usize, 0usize) };
            let baseline_errno = (baseline == -1).then(|| unsafe { *libc::__errno_location() });
            assert_eq!((baseline, baseline_errno), if filter_kind == 2 { (-1, Some(libc::EFAULT)) } else { (0, None) });
            // Actual per-thread filter, without TSYNC. This thread owns the
            // entire mutation and exits normally; the parent stays unfiltered.
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }, 0);
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) }, 0);
            assert_eq!(unsafe { libc::prctl(libc::PR_GET_SECCOMP) }, if filter_kind == 3 { 0 } else { 2 });
            let controlled = unsafe { libc::syscall(libc::SYS_process_vm_readv, 0,
                local.as_ptr(), 2usize, std::ptr::null::<libc::iovec>(), 0usize, 0usize) };
            let errno = (controlled == -1).then(|| unsafe { *libc::__errno_location() });
            assert_eq!((controlled, errno), if filter_kind == 1 { (-1, Some(libc::EFAULT)) } else { (0, None) });
            let mut oracle = ReadRangeOracle::default();
            let first = oracle.inspect(usize::MAX, usize::MAX).unwrap_err();
            assert!(first.contains(if filter_kind == 3 { "wrong or repeated Seccomp:" } else { "seccomp-filtered host query" }));
            assert_eq!(oracle.native_queries, 0);
            assert_eq!(oracle.finish_native_result(0, None, true), Err(first.clone()));
            assert_eq!(oracle.inspect(0, 0), Err(first.clone()));
            assert_eq!(oracle.native_queries, 0);
            eprintln!("actual host filter kind={filter_kind} baseline={baseline} baseline_errno={baseline_errno:?} controlled_result={controlled} errno={errno:?} refusal={first:?} oracle_queries=0");
        }).join().unwrap();
        }
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_SECCOMP) }, 0);
    }

    #[test]
    fn original_read_range_requires_the_same_real_pending_native_entry() {
        let _owner = range_test_logging();
        real_compat_read_capture_refusal();
        let (output, log) = crate::testing::test_fn::<RangeTool, _>(|| unsafe {
            null_at(901);
            let mut bytes = [0xa5u8; 32];
            for _ in 0..13 {
                assert_eq!(
                    libc::syscall(libc::SYS_read, 901, bytes.as_mut_ptr().add(8), 8usize),
                    0
                );
                assert_eq!(bytes, [0xa5; 32]);
            }
            assert_eq!(libc::close(901), 0);
        })
        .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(log.entry_refusals.load(Ordering::SeqCst), 13);
        assert_eq!(log.after_completion_refusals.load(Ordering::SeqCst), 13);
        assert_eq!(log.entry_initial_allowed.load(Ordering::SeqCst), 13);
        let observations = log.observations.lock().unwrap();
        assert_eq!(observations.len(), 13);
        for observation in observations.iter() {
            assert!(observation.0.is_err());
            assert!(observation.1);
            assert_eq!(observation.2, Ok(0));
        }
    }
}
