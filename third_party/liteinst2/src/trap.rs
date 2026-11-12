//! Process-wide SIGTRAP routing for WordPatch++ guards.

/// Kernel-layout signal action retained across guard-router installation.
///
/// Linux x86-64 consumes exactly these four machine words from `rt_sigaction`.
/// The type deliberately avoids libc's larger userspace `sigset_t` layout so a
/// host runtime can install and restore it through an exact trusted syscall.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct GuardSignalAction {
    /// Signal handler address, including `SIG_DFL` and `SIG_IGN` sentinels.
    pub handler: usize,
    /// Linux signal-action flags.
    pub flags: libc::c_ulong,
    /// Signal-restorer entry address when `SA_RESTORER` is present.
    pub restorer: usize,
    /// The exact 64-bit Linux x86-64 signal mask.
    pub mask: u64,
}

/// Three-argument handler ABI used by the guard router.
pub type GuardSignalHandler =
    unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut core::ffi::c_void);

/// Install the guard router and return the prior exact kernel action.
///
/// The callback runs during single-threaded runtime preparation, before the
/// restrictive syscall filter is installed.
pub type GuardSignalInstaller = unsafe fn(
    libc::c_int,
    GuardSignalHandler,
    libc::c_int,
    *mut GuardSignalAction,
) -> Result<(), i32>;

/// Restore a prior default action and redeliver its signal through trusted raw
/// syscalls. Redelivery may remain pending until the current handler returns.
pub type GuardDefaultRestorer = unsafe fn(libc::c_int, &GuardSignalAction) -> Result<(), i32>;

/// Host-owned signal operations required by the guard router after a
/// restrictive syscall filter is active.
#[derive(Clone, Copy)]
pub struct GuardSignalRuntime {
    /// Exact-restorer installation callback.
    pub install: GuardSignalInstaller,
    /// Async-signal-safe default-action restoration and redelivery callback.
    pub restore_default: GuardDefaultRestorer,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod imp {
    use core::ffi::c_void;
    use core::mem::MaybeUninit;
    use core::ptr;
    use core::sync::atomic::AtomicPtr;
    use core::sync::atomic::AtomicU8;
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::sync::MutexGuard;
    use std::sync::OnceLock;

    const IDLE: u8 = 0;
    const WRITING: u8 = 1;

    static HEAD: AtomicPtr<TrapSite> = AtomicPtr::new(ptr::null_mut());
    static PENDING_HEAD: AtomicPtr<PendingReservation> = AtomicPtr::new(ptr::null_mut());
    static REGISTRY_LOCK: Mutex<()> = Mutex::new(());
    static INSTALL_RESULT: OnceLock<Result<(), i32>> = OnceLock::new();
    static SIGNAL_RUNTIME: OnceLock<super::GuardSignalRuntime> = OnceLock::new();
    static PREVIOUS_ACTION: OnceLock<PreviousAction> = OnceLock::new();

    struct PreviousAction(super::GuardSignalAction);

    // SAFETY: sigaction is immutable after publication through OnceLock.
    unsafe impl Send for PreviousAction {}
    // SAFETY: sigaction is immutable after publication through OnceLock.
    unsafe impl Sync for PreviousAction {}

    /// Process-lifetime entry for one executable patch site.
    pub(crate) struct TrapSite {
        execute_address: usize,
        reservation_start: usize,
        reservation_end: usize,
        guard_mask: u8,
        phase: AtomicU8,
        handled: AtomicU64,
        next: *mut TrapSite,
    }

    // SAFETY: immutable fields are published before HEAD's release store; phase
    // is atomic and nodes are never freed.
    unsafe impl Send for TrapSite {}
    // SAFETY: immutable fields are published before HEAD's release store; phase
    // is atomic and nodes are never freed.
    unsafe impl Sync for TrapSite {}

    struct PendingReservation {
        execute_address: usize,
        reservation_start: usize,
        reservation_end: usize,
        guard_mask: u8,
        next: AtomicPtr<PendingReservation>,
    }

    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(#7): Review transactional overlap ownership and lock scope.
    pub(crate) struct PendingTrapSite {
        pending: Option<Box<PendingReservation>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum TrapError {
        Contended,
        Overlap,
        Install(i32),
        #[allow(dead_code)]
        Unsupported,
    }

    impl TrapSite {
        pub(crate) fn begin(&self) -> Result<(), TrapError> {
            self.phase
                .compare_exchange(IDLE, WRITING, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .map_err(|_| TrapError::Contended)
        }

        pub(crate) fn finish(&self) {
            self.phase.store(IDLE, Ordering::Release);
        }

        pub(crate) fn handled_traps(&self) -> u64 {
            self.handled.load(Ordering::Relaxed)
        }
    }

    impl PendingTrapSite {
        pub(crate) fn commit(mut self) -> Result<&'static TrapSite, TrapError> {
            ensure_installed()?;
            let pending = self.pending.as_ref().expect("pending reservation missing");
            let mut site = Box::new(TrapSite {
                execute_address: pending.execute_address,
                reservation_start: pending.reservation_start,
                reservation_end: pending.reservation_end,
                guard_mask: pending.guard_mask,
                phase: AtomicU8::new(IDLE),
                handled: AtomicU64::new(0),
                next: ptr::null_mut(),
            });

            let _guard = REGISTRY_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut pending = self.pending.take().expect("pending reservation missing");
            // SAFETY: the registry mutex serializes all pending-list access.
            unsafe { remove_pending(&mut *pending) };
            site.next = HEAD.load(Ordering::Relaxed);
            let site = Box::into_raw(site);
            HEAD.store(site, Ordering::Release);
            drop(_guard);
            drop(pending);
            // SAFETY: registry nodes are intentionally never freed.
            Ok(unsafe { &*site })
        }
    }

    impl Drop for PendingTrapSite {
        fn drop(&mut self) {
            let Some(mut pending) = self.pending.take() else {
                return;
            };
            let guard = REGISTRY_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // SAFETY: the registry mutex serializes all pending-list access.
            unsafe { remove_pending(&mut *pending) };
            drop(guard);
        }
    }

    pub(crate) fn reserve(
        execute_address: usize,
        reservation_start: usize,
        reservation_end: usize,
        guard_mask: u8,
    ) -> Result<PendingTrapSite, TrapError> {
        let mut pending = Box::new(PendingReservation {
            execute_address,
            reservation_start,
            reservation_end,
            guard_mask,
            next: AtomicPtr::new(ptr::null_mut()),
        });
        let _guard = REGISTRY_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current = HEAD.load(Ordering::Acquire);
        while !current.is_null() {
            // SAFETY: published nodes are process-lifetime allocations.
            let site = unsafe { &*current };
            if reservation_start < site.reservation_end && site.reservation_start < reservation_end
            {
                return Err(TrapError::Overlap);
            }
            current = site.next;
        }
        if pending_overlaps(reservation_start, reservation_end) {
            return Err(TrapError::Overlap);
        }
        pending
            .next
            .store(PENDING_HEAD.load(Ordering::Relaxed), Ordering::Relaxed);
        PENDING_HEAD.store(&mut *pending, Ordering::Relaxed);
        Ok(PendingTrapSite {
            pending: Some(pending),
        })
    }

    pub(crate) fn register(
        execute_address: usize,
        reservation_start: usize,
        reservation_end: usize,
        guard_mask: u8,
    ) -> Result<&'static TrapSite, TrapError> {
        ensure_installed()?;
        let mut candidate = Box::new(TrapSite {
            execute_address,
            reservation_start,
            reservation_end,
            guard_mask,
            phase: AtomicU8::new(IDLE),
            handled: AtomicU64::new(0),
            next: ptr::null_mut(),
        });
        let _guard = lock_registry_for_registration()?;

        let mut current = HEAD.load(Ordering::Acquire);
        while !current.is_null() {
            // SAFETY: published nodes are process-lifetime allocations.
            let site = unsafe { &*current };
            if site.execute_address == execute_address
                && site.reservation_start == reservation_start
                && site.reservation_end == reservation_end
                && site.guard_mask == guard_mask
            {
                return Ok(site);
            }
            if reservation_start < site.reservation_end && site.reservation_start < reservation_end
            {
                return Err(TrapError::Overlap);
            }
            current = site.next;
        }
        if pending_overlaps(reservation_start, reservation_end) {
            return Err(TrapError::Overlap);
        }

        candidate.next = HEAD.load(Ordering::Relaxed);
        let site = Box::into_raw(candidate);
        HEAD.store(site, Ordering::Release);
        // SAFETY: registry nodes are intentionally never freed.
        Ok(unsafe { &*site })
    }

    fn lock_registry_for_registration() -> Result<MutexGuard<'static, ()>, TrapError> {
        #[cfg(test)]
        {
            // The unit-test binary exercises the process-wide registry from
            // otherwise independent tests in parallel. Wait for those tests
            // here so their incidental lock ownership cannot masquerade as a
            // registration failure in overlap and pending-list assertions.
            Ok(REGISTRY_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))
        }
        #[cfg(not(test))]
        {
            // Runtime registration remains fail-fast: a caller must never
            // wait on a lock that could be owned by interrupted code.
            REGISTRY_LOCK.try_lock().map_err(|_| TrapError::Contended)
        }
    }

    fn pending_overlaps(reservation_start: usize, reservation_end: usize) -> bool {
        let mut current = PENDING_HEAD.load(Ordering::Relaxed);
        while !current.is_null() {
            // SAFETY: the registry mutex is held, so pending nodes remain live.
            let pending = unsafe { &*current };
            if reservation_start < pending.reservation_end
                && pending.reservation_start < reservation_end
            {
                return true;
            }
            current = pending.next.load(Ordering::Relaxed);
        }
        false
    }

    unsafe fn remove_pending(target: *mut PendingReservation) {
        let mut previous: *mut PendingReservation = ptr::null_mut();
        let mut current = PENDING_HEAD.load(Ordering::Relaxed);
        while !current.is_null() {
            if current == target {
                // SAFETY: current is a live node protected by the registry mutex.
                let next = unsafe { (*current).next.load(Ordering::Relaxed) };
                if previous.is_null() {
                    PENDING_HEAD.store(next, Ordering::Relaxed);
                } else {
                    // SAFETY: previous is a live node protected by the registry mutex.
                    unsafe { (*previous).next.store(next, Ordering::Relaxed) };
                }
                return;
            }
            previous = current;
            // SAFETY: current is a live node protected by the registry mutex.
            current = unsafe { (*current).next.load(Ordering::Relaxed) };
        }
        debug_assert!(false, "pending trap reservation was not registered");
    }

    pub(crate) fn prepare() -> Result<(), TrapError> {
        ensure_installed()
    }

    pub(crate) fn prepare_with_signal_runtime(
        runtime: super::GuardSignalRuntime,
    ) -> Result<(), TrapError> {
        if INSTALL_RESULT.get().is_some() || SIGNAL_RUNTIME.set(runtime).is_err() {
            return Err(TrapError::Install(libc::EALREADY));
        }
        ensure_installed()
    }

    fn ensure_installed() -> Result<(), TrapError> {
        match *INSTALL_RESULT.get_or_init(install_handler) {
            Ok(()) => Ok(()),
            Err(errno) => Err(TrapError::Install(errno)),
        }
    }

    fn install_handler() -> Result<(), i32> {
        if let Some(runtime) = SIGNAL_RUNTIME.get().copied() {
            let mut previous = MaybeUninit::<super::GuardSignalAction>::uninit();
            // SAFETY: the host callback owns exact signal installation and
            // initializes `previous` on success.
            unsafe {
                (runtime.install)(
                    libc::SIGTRAP,
                    trap_handler,
                    libc::SA_SIGINFO | libc::SA_RESTART,
                    previous.as_mut_ptr(),
                )
            }?;
            // SAFETY: the successful callback initialized the exact action.
            let previous = unsafe { previous.assume_init() };
            let _ = PREVIOUS_ACTION.set(PreviousAction(previous));
            return Ok(());
        }

        let mut previous = MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: querying SIGTRAP disposition writes a complete sigaction.
        if unsafe { libc::sigaction(libc::SIGTRAP, ptr::null(), previous.as_mut_ptr()) } != 0 {
            return Err(last_errno());
        }
        // SAFETY: successful sigaction initialized previous.
        let previous = unsafe { previous.assume_init() };
        let previous = guard_action_from_libc(&previous);
        let _ = PREVIOUS_ACTION.set(PreviousAction(previous));

        // SAFETY: zeroed sigaction is initialized below before installation.
        let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
        action.sa_sigaction = trap_handler as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        // SAFETY: action contains a valid signal set.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        // SAFETY: installs a valid SA_SIGINFO handler.
        if unsafe { libc::sigaction(libc::SIGTRAP, &action, ptr::null_mut()) } != 0 {
            return Err(last_errno());
        }
        Ok(())
    }

    fn guard_action_from_libc(action: &libc::sigaction) -> super::GuardSignalAction {
        super::GuardSignalAction {
            handler: action.sa_sigaction,
            flags: action.sa_flags as libc::c_ulong,
            restorer: action
                .sa_restorer
                .map(|restorer| restorer as usize)
                .unwrap_or(0),
            // SAFETY: Linux x86-64 consumes the first 64 bits of libc's larger
            // sigset_t and the source is a fully initialized sigaction.
            mask: unsafe { ptr::addr_of!(action.sa_mask).cast::<u64>().read_unaligned() },
        }
    }

    fn guard_action_into_libc(action: &super::GuardSignalAction) -> libc::sigaction {
        // SAFETY: every field used by libc::sigaction is initialized below.
        let mut converted: libc::sigaction = unsafe { core::mem::zeroed() };
        converted.sa_sigaction = action.handler;
        converted.sa_flags = action.flags as libc::c_int;
        converted.sa_restorer = if action.restorer == 0 {
            None
        } else {
            // SAFETY: the value came from a previously installed kernel action.
            Some(unsafe { core::mem::transmute::<usize, extern "C" fn()>(action.restorer) })
        };
        // SAFETY: converted owns its zeroed sigset_t; Linux uses its first word.
        unsafe {
            ptr::addr_of_mut!(converted.sa_mask)
                .cast::<u64>()
                .write_unaligned(action.mask)
        };
        converted
    }

    fn last_errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    extern "C" fn trap_handler(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut c_void,
    ) {
        // SAFETY: the kernel supplies siginfo and ucontext for SA_SIGINFO.
        unsafe { handle_trap(signal, info, context) };
    }

    unsafe fn handle_trap(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
        if signal == libc::SIGTRAP && !context.is_null() {
            // SAFETY: SA_SIGINFO supplies a mutable ucontext_t.
            let context = unsafe { &mut *context.cast::<libc::ucontext_t>() };
            let rip = context.uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
            let trap_address = rip.wrapping_sub(1);

            let mut current = HEAD.load(Ordering::Acquire);
            while !current.is_null() {
                // SAFETY: published registry nodes are never freed.
                let site = unsafe { &*current };
                let relative = trap_address.wrapping_sub(site.execute_address);
                if relative < 8 && site.guard_mask & (1 << relative) != 0 {
                    site.handled.fetch_add(1, Ordering::Relaxed);
                    while site.phase.load(Ordering::Acquire) == WRITING {
                        core::hint::spin_loop();
                    }
                    context.uc_mcontext.gregs[libc::REG_RIP as usize] =
                        trap_address as libc::greg_t;
                    return;
                }
                current = site.next;
            }
        }

        // SAFETY: unknown traps are delegated to the disposition we replaced.
        unsafe { chain_previous(signal, info, context) };
    }

    unsafe fn chain_previous(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut c_void,
    ) {
        let Some(previous) = PREVIOUS_ACTION.get() else {
            // SAFETY: _exit is async-signal-safe and never returns.
            unsafe { libc::_exit(128 + signal) };
        };
        let handler = previous.0.handler;
        if handler == libc::SIG_IGN {
            return;
        }
        if handler == libc::SIG_DFL {
            if let Some(runtime) = SIGNAL_RUNTIME.get().copied() {
                // SAFETY: the host callback is required to use only trusted,
                // async-signal-safe raw operations in this signal context.
                if unsafe { (runtime.restore_default)(signal, &previous.0) }.is_err() {
                    unsafe { libc::_exit(128 + signal) };
                }
                return;
            }
            let previous = guard_action_into_libc(&previous.0);
            // SAFETY: restoring disposition and raising are async-signal-safe.
            unsafe {
                libc::sigaction(signal, &previous, ptr::null_mut());
                libc::raise(signal);
            }
            return;
        }

        if previous.0.flags & libc::SA_SIGINFO as libc::c_ulong != 0 {
            type Handler = unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut c_void);
            // SAFETY: SA_SIGINFO specifies the three-argument handler ABI.
            let handler: Handler = unsafe { core::mem::transmute(handler) };
            // SAFETY: arguments are the kernel-provided signal context.
            unsafe { handler(signal, info, context) };
        } else {
            type Handler = unsafe extern "C" fn(libc::c_int);
            // SAFETY: absence of SA_SIGINFO specifies the one-argument ABI.
            let handler: Handler = unsafe { core::mem::transmute(handler) };
            // SAFETY: signal number is kernel-provided.
            unsafe { handler(signal) };
        }
    }
    #[cfg(test)]
    mod tests {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        use super::REGISTRY_LOCK;
        use super::TrapError;
        use super::pending_overlaps;
        use super::register;
        use super::reserve;

        const PENDING_LIST_RACE_ITERATIONS: usize = 64;

        #[test]
        fn pending_reservation_blocks_only_overlapping_registrations() {
            let storage = Box::leak(Box::new([0_u8; 32]));
            let base = storage.as_ptr() as usize;
            let pending = reserve(base, base, base + 8, 0).unwrap();

            let disjoint = register(base + 16, base + 16, base + 24, 0);
            assert!(disjoint.is_ok());
            assert!(matches!(
                register(base + 4, base + 4, base + 12, 0),
                Err(TrapError::Overlap)
            ));

            drop(pending);
            assert!(
                register(base, base, base + 8, 0).is_ok(),
                "dropping a pending token must release its reservation"
            );
        }

        #[test]
        fn concurrent_pending_commit_and_drop_preserve_the_list() {
            for iteration in 0..PENDING_LIST_RACE_ITERATIONS {
                let storage = Box::leak(Box::new([0_u8; 32]));
                let base = storage.as_ptr() as usize;
                let dropped = reserve(base, base, base + 8, 0).unwrap();
                let committed = reserve(base + 16, base + 16, base + 24, 0).unwrap();
                let start = Arc::new(Barrier::new(3));

                let drop_start = Arc::clone(&start);
                let dropper = thread::spawn(move || {
                    drop_start.wait();
                    drop(dropped);
                });
                let commit_start = Arc::clone(&start);
                let committer = thread::spawn(move || {
                    commit_start.wait();
                    committed.commit()
                });
                start.wait();

                dropper.join().unwrap();
                committer.join().unwrap().unwrap();

                let _guard = REGISTRY_LOCK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                assert!(
                    !pending_overlaps(base, base + 8),
                    "iteration {iteration}: dropping a pending token leaked its reservation"
                );
                assert!(
                    !pending_overlaps(base + 16, base + 24),
                    "iteration {iteration}: committing a pending token leaked its reservation"
                );
            }
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod imp {
    #[allow(dead_code)]
    pub(crate) struct PendingTrapSite;

    #[allow(dead_code)]
    impl PendingTrapSite {
        pub(crate) fn commit(self) -> Result<&'static TrapSite, TrapError> {
            unreachable!("pending trap sites are unavailable on this target")
        }
    }
    pub(crate) struct TrapSite;

    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum TrapError {
        Contended,
        Overlap,
        Install(i32),
        Unsupported,
    }

    impl TrapSite {
        pub(crate) fn begin(&self) -> Result<(), TrapError> {
            Err(TrapError::Unsupported)
        }

        pub(crate) fn finish(&self) {}

        pub(crate) fn handled_traps(&self) -> u64 {
            0
        }
    }
    #[allow(dead_code)]
    pub(crate) fn reserve(
        _execute_address: usize,
        _reservation_start: usize,
        _reservation_end: usize,
        _guard_mask: u8,
    ) -> Result<PendingTrapSite, TrapError> {
        Err(TrapError::Unsupported)
    }
    pub(crate) fn prepare() -> Result<(), TrapError> {
        Err(TrapError::Unsupported)
    }

    pub(crate) fn prepare_with_signal_runtime(
        _runtime: super::GuardSignalRuntime,
    ) -> Result<(), TrapError> {
        Err(TrapError::Unsupported)
    }

    pub(crate) fn register(
        _execute_address: usize,
        _reservation_start: usize,
        _reservation_end: usize,
        _guard_mask: u8,
    ) -> Result<&'static TrapSite, TrapError> {
        Err(TrapError::Unsupported)
    }
}

pub(crate) use imp::TrapError;
pub(crate) use imp::TrapSite;
pub(crate) use imp::prepare;
pub(crate) use imp::prepare_with_signal_runtime;
pub(crate) use imp::register;
pub(crate) use imp::reserve;
