/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Tests of `HostProof`: one cold owner proof per managed poll, rechecked by a
// write-once MADV_WIPEONFORK marker. The fork-in-callback refusals of the
// managed polls themselves are in managed_native_tests.rs.

fn host_proof_rechecks() -> HostProofRechecks {
    HOST_PROOF_RECHECKS.with(Cell::get)
}

fn owner_cold_proofs() -> usize {
    OWNER_COLD_PROOFS.with(Cell::get)
}

/// A fresh unpublished marker slot for this thread only.
fn fresh_marker_slot_for_test() -> &'static AtomicUsize {
    let slot: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(MARKER_UNPUBLISHED)));
    MARKER_SLOT_FOR_TEST.with(|current| current.set(Some(slot)));
    slot
}

fn clear_marker_slot_for_test() {
    MARKER_SLOT_FOR_TEST.with(|current| current.set(None));
}

/// Exits a forked child: 0 only if `child` returns true without panicking.
/// A panic must not unwind into the copied libtest thread: with no harness
/// thread in the child, that thread's exit would end the child with status 0.
fn exit_host_proof_child(child: impl FnOnce() -> bool) -> ! {
    let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(child)).unwrap_or(false);
    unsafe { libc::_exit(if passed { 0 } else { 1 }) };
}

/// Forks; the child runs `child` and exits 0 only if it returns true, under a
/// 5 s alarm so that a hang fails instead of stalling the suite.
fn host_proof_fork_case(child: impl FnOnce() -> bool) -> libc::pid_t {
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        unsafe { libc::alarm(5) };
        exit_host_proof_child(child);
    }
    pid
}

fn assert_host_proof_child_passed(pid: libc::pid_t) {
    let status = waitpid_status_bounded(Pid::from_raw(pid), 0, TRACEE_WAIT_TIMEOUT).unwrap();
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "child status {status:#x}"
    );
}

#[test]
fn host_proof_keeps_local_auto_traits() {
    macro_rules! not_trait {
        ($bound:path) => {{
            trait Ambiguous<A> {
                fn witness() {}
            }
            impl<T: ?Sized> Ambiguous<()> for T {}
            struct Forbidden;
            impl<T: ?Sized + $bound> Ambiguous<Forbidden> for T {}
            let _ = <HostProof<'static> as Ambiguous<_>>::witness;
        }};
    }
    not_trait!(Send);
    not_trait!(Sync);
    not_trait!(Clone);
}

#[test]
fn host_proof_is_armed_by_a_published_marker_and_rechecks_without_a_syscall() {
    let owner = LegacyWaitOwner::capture_current().unwrap();
    let Some(_) = sample_host_proof_marker() else {
        println!("HOST_PROOF_MARKER_UNAVAILABLE");
        return;
    };
    let cold = owner_cold_proofs();
    let rechecks = host_proof_rechecks();
    let proof = owner.prove().unwrap();
    assert!(proof.marker.is_some());
    for _ in 0..3 {
        proof.recheck().unwrap();
    }
    assert_eq!(owner_cold_proofs(), cold + 1, "only the proof itself is cold");
    let after = host_proof_rechecks();
    assert_eq!(after.armed, rechecks.armed + 3);
    assert_eq!(after.wiped, rechecks.wiped);
    assert_eq!(after.unarmed, rechecks.unarmed);
}

#[test]
fn host_proof_refuses_another_thread() {
    let owner = Arc::new(LegacyWaitOwner::capture_current().unwrap());
    let other = Arc::clone(&owner);
    let refused = thread::spawn(move || other.prove().err()).join().unwrap();
    assert_eq!(refused, Some(Errno::EPERM));
    owner.prove().unwrap().recheck().unwrap();
}

/// The dangerous order: the parent's cold proof, a fork, then the child
/// publishes a marker reading 1 before its inherited proof rechecks. The
/// proof sampled the marker before its cold proof, when it was unpublished,
/// so it is unarmed and the child's recheck is the cold proof, which refuses
/// it. Sampling after the cold proof would arm it from the child's page.
#[test]
fn host_proof_is_never_armed_by_a_marker_published_after_its_cold_proof() {
    let owner = LegacyWaitOwner::capture_current().unwrap();
    let slot = fresh_marker_slot_for_test();
    let origin = unsafe { libc::getpid() };
    let child = Rc::new(Cell::new(0));
    let hook_child = Rc::clone(&child);
    AFTER_COLD_PROOF_FOR_TEST.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                unsafe { libc::alarm(5) };
                // The child's own initialization: its copy of the slot is
                // still unpublished, so it publishes a page reading 1.
                publish_host_proof_marker();
            } else {
                hook_child.set(pid);
            }
        }));
    });
    let proof = owner.prove().unwrap();
    if unsafe { libc::getpid() } != origin {
        exit_host_proof_child(|| {
            let published = sample_host_proof_marker().is_some();
            let rechecks = host_proof_rechecks();
            let refused = proof.recheck() == Err(Errno::EPERM);
            let unarmed = host_proof_rechecks().unarmed == rechecks.unarmed + 1;
            published && refused && unarmed
        });
    }
    assert_eq!(slot.load(Ordering::Acquire), MARKER_UNPUBLISHED);
    assert!(proof.marker.is_none());
    proof.recheck().unwrap();
    clear_marker_slot_for_test();
    assert_host_proof_child_passed(child.get());
}

/// A child of any fork-family route, and its child, read the inherited page
/// as zero, and their own initialization never rewrites it.
#[test]
fn host_proof_inherited_by_a_forked_child_or_grandchild_is_refused() {
    let owner = LegacyWaitOwner::capture_current().unwrap();
    let Some(_) = sample_host_proof_marker() else {
        println!("HOST_PROOF_MARKER_UNAVAILABLE");
        return;
    };
    let proof = owner.prove().unwrap();
    assert!(proof.marker.is_some());
    let child_refuses = |proof: &HostProof<'_>| {
        // An owner capture runs the initializer before the recheck. It must
        // not re-arm the inherited page.
        let _ = LegacyWaitOwner::capture_current();
        let rechecks = host_proof_rechecks();
        proof.recheck() == Err(Errno::EPERM) && host_proof_rechecks().wiped == rechecks.wiped + 1
    };
    type ForkRoute = (&'static str, fn() -> libc::pid_t);
    let routes: [ForkRoute; 3] = [
        ("libc fork", || unsafe { libc::fork() }),
        ("SYS_fork", || unsafe { libc::syscall(libc::SYS_fork) as libc::pid_t }),
        ("SYS_clone(SIGCHLD)", || unsafe {
            libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0usize, 0usize, 0usize, 0usize)
                as libc::pid_t
        }),
    ];
    for (name, route) in routes {
        let pid = route();
        assert!(pid >= 0, "{name}");
        if pid == 0 {
            unsafe { libc::alarm(5) };
            exit_host_proof_child(|| child_refuses(&proof));
        }
        assert_host_proof_child_passed(pid);
        proof.recheck().unwrap();
    }
    let pid = host_proof_fork_case(|| {
        let grandchild = host_proof_fork_case(|| child_refuses(&proof));
        let status =
            waitpid_status_bounded(Pid::from_raw(grandchild), 0, TRACEE_WAIT_TIMEOUT).unwrap();
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    });
    assert_host_proof_child_passed(pid);
    proof.recheck().unwrap();
}

/// A failed madvise unmaps its candidate and publishes the permanent failure:
/// no later capture maps again, and every recheck is cold.
#[test]
fn host_proof_marker_failure_is_sticky_and_keeps_proofs_cold() {
    let slot = fresh_marker_slot_for_test();
    MARKER_MADVISE_FAILS_FOR_TEST.with(|fails| fails.set(true));
    let maps = MARKER_MAPS_FOR_TEST.with(Cell::get);
    let owner = LegacyWaitOwner::capture_current().unwrap();
    assert_eq!(MARKER_MAPS_FOR_TEST.with(Cell::get), maps + 1);
    assert_eq!(slot.load(Ordering::Acquire), MARKER_UNAVAILABLE);
    // The failed candidate is unmapped: msync reports ENOMEM for it.
    let candidate = MARKER_LAST_MAP_FOR_TEST.with(Cell::get);
    assert_eq!(
        unsafe { libc::msync(candidate as *mut libc::c_void, MARKER_PAGE_SIZE, libc::MS_ASYNC) },
        -1
    );
    assert_eq!(Errno::last(), Errno::ENOMEM);
    MARKER_MADVISE_FAILS_FOR_TEST.with(|fails| fails.set(false));
    publish_host_proof_marker();
    let _second = LegacyWaitOwner::capture_current().unwrap();
    assert_eq!(
        MARKER_MAPS_FOR_TEST.with(Cell::get),
        maps + 1,
        "a recorded failure is never retried"
    );
    let cold = owner_cold_proofs();
    let rechecks = host_proof_rechecks();
    let proof = owner.prove().unwrap();
    assert!(proof.marker.is_none());
    proof.recheck().unwrap();
    proof.recheck().unwrap();
    assert_eq!(owner_cold_proofs(), cold + 3);
    assert_eq!(host_proof_rechecks().unarmed, rechecks.unarmed + 2);
    clear_marker_slot_for_test();
}

/// Publication never waits: a child forked while another thread sits between
/// writing its page and publishing it publishes its own page, and the paused
/// thread's publication then completes in the parent.
#[test]
fn host_proof_marker_publication_never_waits_across_a_fork() {
    let slot = fresh_marker_slot_for_test();
    let gates: &'static [AtomicBool; 2] =
        Box::leak(Box::new([AtomicBool::new(false), AtomicBool::new(false)]));
    let publisher = thread::spawn(move || {
        MARKER_SLOT_FOR_TEST.with(|current| current.set(Some(slot)));
        PAUSE_BEFORE_MARKER_PUBLICATION_FOR_TEST.with(|pause| pause.set(Some(gates)));
        publish_host_proof_marker();
    });
    while !gates[0].load(Ordering::Acquire) {
        thread::yield_now();
    }
    assert_eq!(slot.load(Ordering::Acquire), MARKER_UNPUBLISHED);
    let pid = host_proof_fork_case(|| {
        publish_host_proof_marker();
        sample_host_proof_marker().is_some()
    });
    assert_host_proof_child_passed(pid);
    gates[1].store(true, Ordering::Release);
    publisher.join().unwrap();
    assert!(sample_host_proof_marker().is_some());
    clear_marker_slot_for_test();
}

/// Debug builds catch an armed proof whose continuation runs on another task.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "a HostProof's continuation moved to another task")]
fn host_proof_debug_build_asserts_the_tid_on_the_armed_branch() {
    let other = thread::spawn(|| LegacyWaitOwner::capture_current().unwrap())
        .join()
        .unwrap();
    let marker = sample_host_proof_marker().expect("published by the capture");
    // A forged armed proof for another task's owner: reaches the armed,
    // still-1 branch without a cold proof.
    let forged = HostProof {
        owner: &other,
        marker: Some(marker),
        _local: PhantomData,
    };
    let _ = forged.recheck();
}

/// A separate-stack CLONE_VM child cannot reach a poll's proof; through a
/// guard it gets the unchanged cold check, which refuses it. A regression test
/// of `check_current`, not evidence for `HostProof`.
#[test]
fn check_current_refuses_a_separate_stack_clone_vm_child() {
    let guard = PtracerThreadGuard::from_owner(Arc::new(LegacyWaitOwner::capture_current().unwrap()));
    struct Shared {
        guard: PtracerThreadGuard,
        result: Option<Result<(), Errno>>,
    }
    extern "C" fn child(arg: *mut libc::c_void) -> libc::c_int {
        let shared = unsafe { &mut *arg.cast::<Shared>() };
        shared.result = Some(shared.guard.check_current());
        0
    }
    let mut shared = Shared {
        guard: guard.clone(),
        result: None,
    };
    let mut stack = vec![0u8; 256 * 1024];
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) }.cast::<libc::c_void>();
    let pid = unsafe {
        libc::clone(
            child,
            top,
            libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD,
            (&mut shared as *mut Shared).cast(),
        )
    };
    assert!(pid > 0);
    let status = waitpid_status_bounded(Pid::from_raw(pid), 0, TRACEE_WAIT_TIMEOUT).unwrap();
    assert!(libc::WIFEXITED(status));
    assert_eq!(shared.result, Some(Err(Errno::EPERM)));
    guard.check_current().unwrap();
}

/// Each managed poll call makes exactly one cold owner proof, whatever its
/// outcome; every later check in the call is an armed recheck.
#[test]
#[cfg(not(sanitized))]
fn managed_poll_makes_one_cold_owner_proof_per_call() {
    const NAME: &str = "managed_poll_makes_one_cold_owner_proof_per_call";
    const MARKER: &str = "ACTUAL_MANAGED_ONE_PROOF_PER_POLL_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_ONE_PROOF_CONTROL";
    if run_managed_native_outer(NAME, MARKER, UNAVAILABLE) {
        return;
    }
    let Some(mut guest) = managed_native_guest(true, UNAVAILABLE) else {
        return;
    };
    assert!(sample_host_proof_marker().is_some(), "published at owner capture");
    let noop = futures::task::noop_waker();
    let mut cx = Context::from_waker(&noop);
    // A queued SIGUSR1: Ready in one call, rechecked after registration and
    // after decode.
    let mut owned = guest.running.take().unwrap().wait_owned();
    let cold = owner_cold_proofs();
    let rechecks = host_proof_rechecks();
    let stopped = match owned.poll_with_ptracer_guard(&mut cx, &guest.guard) {
        Poll::Ready(Ok(Wait::Stopped(stopped, crate::Event::Signal(Signal::SIGUSR1)))) => stopped,
        other => panic!("managed wait did not return the queued SIGUSR1: {other:?}"),
    };
    assert_eq!(owner_cold_proofs(), cold + 1);
    let after = host_proof_rechecks();
    assert_eq!(after.armed, rechecks.armed + 2);
    assert_eq!((after.wiped, after.unarmed), (rechecks.wiped, rechecks.unarmed));
    // A completed future's call still proves once at entry.
    assert!(matches!(
        owned.poll_with_ptracer_guard(&mut cx, &guest.guard),
        Poll::Ready(Err(OwnedWaitError::Completed))
    ));
    assert_eq!(owner_cold_proofs(), cold + 2);
    // The exit future: one cold proof per call, Pending or Ready; a Ready call
    // also rechecks after wake_all.
    drop(guest.running.take());
    let running = stopped.resume(None).unwrap();
    let (mut calls, mut ready) = (0, None);
    let cold = owner_cold_proofs();
    let rechecks = host_proof_rechecks();
    while ready.is_none() {
        calls += 1;
        match guest.exit.poll_with_ptracer_guard(&mut cx, &guest.guard) {
            Poll::Ready(result) => ready = Some(result.unwrap()),
            Poll::Pending => {
                assert!(Instant::now() < guest.deadline, "managed EXIT deadline");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    let stopped = ready.unwrap();
    assert_eq!(owner_cold_proofs(), cold + calls);
    assert_eq!(host_proof_rechecks().armed, rechecks.armed + calls + 1);
    guest.cleanup.mark_claimed_exit();
    assert_eq!(stopped.getevent().unwrap(), (23 << 8) as libc::c_long);
    drop(running);
    // The final wait: Pending calls recheck once, the Ready call twice.
    let mut final_wait = stopped.resume(None).unwrap().wait_owned();
    let (mut calls, mut ready) = (0, None);
    let cold = owner_cold_proofs();
    let rechecks = host_proof_rechecks();
    while ready.is_none() {
        calls += 1;
        match final_wait.poll_with_ptracer_guard(&mut cx, &guest.guard) {
            Poll::Ready(result) => ready = Some(result.unwrap()),
            Poll::Pending => {
                assert!(Instant::now() < guest.deadline, "managed final wait deadline");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    assert_eq!(
        ready.unwrap().assume_exited(),
        (guest.root.into(), crate::ExitStatus::Exited(23))
    );
    assert_eq!(owner_cold_proofs(), cold + calls);
    assert_eq!(host_proof_rechecks().armed, rechecks.armed + calls + 1);
    assert!(
        guest
            .terminal
            .wait(guest.deadline.saturating_duration_since(Instant::now()))
    );
    guest.cleanup.disarm();
    emit_completion_marker(MARKER);
}

/// A first managed poll refused by the cold proof (another thread, holding the
/// owner's guard) retains nothing; the owner's next poll is admitted.
#[test]
#[cfg(not(sanitized))]
fn refused_first_managed_poll_retains_no_guard() {
    const NAME: &str = "refused_first_managed_poll_retains_no_guard";
    const MARKER: &str = "ACTUAL_REFUSED_FIRST_POLL_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_REFUSED_FIRST_POLL_CONTROL";
    if run_managed_native_outer(NAME, MARKER, UNAVAILABLE) {
        return;
    }
    let Some(mut guest) = managed_native_guest(true, UNAVAILABLE) else {
        return;
    };
    let original_fifo = guest.terminal.queued_raw_statuses();
    let owned = guest.running.take().unwrap().wait_owned();
    let guard = guest.guard.clone();
    let mut owned = thread::spawn(move || {
        let mut owned = owned;
        let noop = futures::task::noop_waker();
        assert!(matches!(
            owned.poll_with_ptracer_guard(&mut Context::from_waker(&noop), &guard),
            Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
        ));
        assert!(owned.managed_guard.is_none(), "a refused poll retained its guard");
        owned
    })
    .join()
    .unwrap();
    assert_eq!(guest.terminal.queued_raw_statuses(), original_fifo);
    let noop = futures::task::noop_waker();
    let stopped = match owned.poll_with_ptracer_guard(&mut Context::from_waker(&noop), &guest.guard)
    {
        Poll::Ready(Ok(Wait::Stopped(stopped, crate::Event::Signal(Signal::SIGUSR1)))) => stopped,
        other => panic!("owner's managed wait was not admitted: {other:?}"),
    };
    assert!(Arc::ptr_eq(
        &owned.managed_guard.as_ref().unwrap().owner,
        &guest.guard.owner
    ));
    finish_managed_native_guest(guest, Some(stopped));
    emit_completion_marker(MARKER);
}

/// A fork from a callback that runs inside decode, before the decode recheck.
///
/// The tracee clones a child. Before the parent's clone stop is decoded, the
/// test captures the child under a forced legacy (non-pidfd) handle and
/// registers a forking waker on that Event. Decoding the clone stop captures
/// the child natively, which promotes the cached legacy Event to a Native view
/// and wakes its status waker (`native_view`), all inside decode. The child of
/// that fork returns into decode and must be refused by the decode recheck,
/// with the original FIFO front still queued.
#[test]
#[cfg(not(sanitized))]
fn managed_wait_rechecks_after_a_fork_inside_decode() {
    const NAME: &str = "managed_wait_rechecks_after_a_fork_inside_decode";
    const MARKER: &str = "ACTUAL_MANAGED_DECODE_FORK_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_DECODE_FORK_CONTROL";
    if run_managed_native_outer(NAME, MARKER, UNAVAILABLE) {
        return;
    }
    let root = match unsafe { fork() }.unwrap() {
        ForkResult::Parent { child } => child,
        ForkResult::Child => {
            crate::traceme_and_stop().unwrap();
            let cloned =
                unsafe { libc::syscall(libc::SYS_clone, 0usize, 0usize, 0usize, 0usize, 0usize) };
            if cloned == 0 {
                unsafe { libc::_exit(0) };
            }
            unsafe { libc::_exit(if cloned > 0 { 23 } else { 1 }) };
        }
    };
    let mut cleanup = TraceeCleanupGuard::new(root).unwrap();
    let status = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
    assert!(libc::WIFSTOPPED(status));
    let running = match Running::try_new(root.into()) {
        Ok(running) => running,
        Err(Errno::EINVAL) => {
            println!("{UNAVAILABLE}");
            return;
        }
        Err(error) => panic!("managed original Native capture: {error}"),
    };
    let generation = running.generation();
    let terminal = generation.terminal_cleanup();
    let guard = generation.ptracer_thread_guard_for_wait().unwrap();
    let stopped = generation.assume_stopped();
    drop(running);
    stopped
        .setoptions(
            Options::PTRACE_O_TRACEEXIT
                | Options::PTRACE_O_EXITKILL
                | Options::PTRACE_O_TRACECLONE,
        )
        .unwrap();
    let _exit = cleanup.exit_event(&stopped).unwrap();
    let running = stopped.resume(None).unwrap();
    let deadline = Instant::now() + TRACEE_WAIT_TIMEOUT;
    // The parent's clone stop is queued, and the new child is attached and
    // stopped in its initial SIGSTOP.
    while terminal.queued_raw_statuses().is_empty() {
        assert!(Instant::now() < deadline, "no clone stop queued");
        thread::sleep(Duration::from_millis(1));
    }
    // The parent is in its clone stop; its event message names the child.
    let child = Pid::from_raw(nix::sys::ptrace::getevent(root).unwrap() as libc::pid_t);
    loop {
        let status = fs::read_to_string(format!("/proc/{child}/status")).unwrap_or_default();
        if status.lines().any(|line| line.starts_with("State:\tt")) {
            break;
        }
        assert!(Instant::now() < deadline, "clone child {child} never stopped");
        thread::sleep(Duration::from_millis(1));
    }
    let child_pidfd = pidfd_open(child.as_raw()).unwrap();
    let force = LegacyThreadGroup::new(child);
    let legacy_child = Running::new_on_ptracer_thread(child.into()).unwrap();
    drop(force);
    // Read the handle kind directly: `terminal_cleanup()` would register the
    // Event, and registration already promotes it to a Native view.
    assert!(matches!(
        legacy_child.1.event().identity().unwrap().pidfd,
        ThreadHandle::LegacyLeader { .. }
    ));
    assert!(legacy_child.1.event().event().native_wait_identity.get().is_none());
    let state = ManagedWakerFork::new(ManagedForkCallback::Wake, ManagedForkRoute::Libc);
    legacy_child
        .1
        .event()
        .event()
        .status_waker
        .register(&state.waker());
    let original_fifo = terminal.queued_raw_statuses();
    assert_eq!(original_fifo.len(), 1);
    state.armed.store(true, Ordering::Release);
    let mut owned = running.wait_owned();
    let noop = futures::task::noop_waker();
    let result = owned.poll_with_ptracer_guard(&mut Context::from_waker(&noop), &guard);
    if state.is_foreign_copy() {
        assert!(matches!(
            result,
            Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
        ));
        assert_eq!(terminal.queued_raw_statuses(), original_fifo);
        assert!(owned.generation().is_some());
        state.assert_child_took_the_wiped_fallback();
        state.report_and_exit();
    }
    assert!(state.fired.load(Ordering::Acquire), "no wake inside decode: {result:?}");
    state.assert_original_recheck_was_armed();
    let (stopped, new_child) = match result {
        Poll::Ready(Ok(Wait::Stopped(
            stopped,
            crate::Event::NewChild(crate::ChildOp::Clone, new_child),
        ))) => (stopped, new_child),
        other => panic!("original managed wait did not decode the clone stop: {other:?}"),
    };
    assert_eq!(new_child.pid(), child.into());
    assert!(terminal.pending_is_empty());
    // Teardown: both tracees die by SIGKILL; the root's guard reaps it.
    pidfd_send_signal(&child_pidfd, libc::SIGKILL).unwrap();
    drop((new_child, legacy_child));
    pidfd_send_signal(&cleanup.pidfd, libc::SIGKILL).unwrap();
    drop(stopped);
    drop(cleanup);
    emit_completion_marker(MARKER);
}
