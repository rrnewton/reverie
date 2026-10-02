//! Constructor controls plus an actual function guest's source refusals.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Mutex;

use reverie::Guest;
use reverie::InitialCommandObservation;
use reverie::syscalls::NativeUserReadError;
use reverie::syscalls::NativeUserReadRefusal;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

// Evaluate the actual emitted classic-BPF instructions. This is a modeled
// constructor check, not evidence that a kernel installed the program.
fn action(filter: &seccomp::Filter, nr: u32, ip: u64, arch: u32) -> u32 {
    let mut accumulator = 0;
    let mut memory = [0u32; 16];
    let mut pc = 0;
    for _ in 0..filter.len() {
        let insn = &filter.instructions()[pc];
        pc += 1;
        match insn.code {
            0x20 => {
                accumulator = match insn.k {
                    0 => nr,
                    4 => arch,
                    8 => ip as u32,
                    12 => (ip >> 32) as u32,
                    offset => panic!("unexpected seccomp-data offset {offset}"),
                };
            }
            0x02 => memory[insn.k as usize] = accumulator,
            0x60 => accumulator = memory[insn.k as usize],
            0x15 | 0x25 | 0x35 => {
                let yes = match insn.code {
                    0x15 => accumulator == insn.k,
                    0x25 => accumulator > insn.k,
                    _ => accumulator >= insn.k,
                };
                pc += if yes { insn.jt } else { insn.jf } as usize;
            }
            0x06 => return insn.k,
            code => panic!("unexpected classic-BPF opcode {code:#x}"),
        }
    }
    panic!("filter failed to return within its instruction bound");
}

fn native_action(filter: &seccomp::Filter, nr: u32, ip: u64) -> u32 {
    action(filter, nr, ip, seccomp::TargetArch::CURRENT as u32)
}

const SOURCE_CALLS: &[Sysno] = &[
    Sysno::clone,
    Sysno::clone3,
    Sysno::execve,
    Sysno::execveat,
    Sysno::fork,
    Sysno::vfork,
    Sysno::madvise,
    Sysno::process_madvise,
    Sysno::io_setup,
    Sysno::io_submit,
    Sysno::io_uring_setup,
    Sysno::io_uring_enter,
    Sysno::io_uring_register,
    Sysno::userfaultfd,
    Sysno::seccomp,
    Sysno::ioctl,
    Sysno::vmsplice,
    Sysno::prctl,
];

#[test]
fn function_filter_only_traces_tool_subscriptions() {
    let subscriptions: Subscription = [Sysno::write, Sysno::execve, Sysno::rt_sigreturn]
        .into_iter()
        .collect();
    let filter = fork_function_seccomp_filter(&subscriptions);
    let private_ip = (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64;
    for nr in (0..550).chain([0x4000_0027, u32::MAX]) {
        assert_eq!(
            native_action(&filter, nr, 0x400000),
            if nr == Sysno::write as u32 || nr == Sysno::execve as u32 {
                libc::SECCOMP_RET_TRACE
            } else {
                libc::SECCOMP_RET_ALLOW
            },
            "ordinary function syscall {nr}"
        );
        assert_eq!(
            native_action(&filter, nr, private_ip),
            libc::SECCOMP_RET_ALLOW,
            "private function syscall {nr}"
        );
    }
    assert_eq!(
        action(&filter, Sysno::write as u32, private_ip, 0),
        libc::SECCOMP_RET_KILL_PROCESS
    );
}

#[test]
fn command_inherited_filter_preserves_tool_policy_for_source_families() {
    // Independent constructor cases must stay joined to the task-owned policy.
    // The policy/ENTRY/native controls test source revocation; ALLOW here is
    // not source authority or proof that an actual syscall was observed.
    assert_eq!(SOURCE_CALLS.len(), 18);
    assert_eq!(SOURCE_CALLS, crate::task::source_epoch::observed_syscalls());
    let private_ip = (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64;
    for source_subscribed in [false, true] {
        let subscriptions: Subscription = std::iter::once(Sysno::write)
            .chain(SOURCE_CALLS.iter().copied().filter(|_| source_subscribed))
            .collect();
        let command = seccomp_filter(&subscriptions);
        let function = fork_function_seccomp_filter(&subscriptions);
        for nr in SOURCE_CALLS
            .iter()
            .map(|nr| *nr as u32)
            .chain([0x4000_0027])
        {
            for ip in [0x400000, private_ip] {
                // All original ordinary/private-IP cases remain. Source-only
                // TRACE is forbidden in the inherited filter, but a genuine
                // Tool subscription must still trace the ordinary native call.
                // x32 remains unsupported by the separate source observer.
                let expected = if source_subscribed && nr < 0x4000_0000 && ip == 0x400000 {
                    libc::SECCOMP_RET_TRACE
                } else {
                    libc::SECCOMP_RET_ALLOW
                };
                for (kind, filter) in [("Command", &command), ("Function", &function)] {
                    assert_eq!(
                        native_action(filter, nr, ip),
                        expected,
                        "{kind} inherited Tool policy: nr={nr} ip={ip:#x} source_subscribed={source_subscribed}"
                    );
                    assert_eq!(
                        action(filter, nr, ip, 0),
                        libc::SECCOMP_RET_KILL_PROCESS,
                        "{kind} architecture validation: nr={nr} ip={ip:#x}"
                    );
                }
            }
        }
        for filter in [&command, &function] {
            assert_eq!(
                native_action(filter, Sysno::write as u32, 0x400000),
                libc::SECCOMP_RET_TRACE
            );
            assert_eq!(
                native_action(filter, Sysno::write as u32, private_ip),
                libc::SECCOMP_RET_ALLOW
            );
        }
    }
}

#[derive(Debug, Default)]
struct SourceRefusals(Mutex<Vec<(bool, bool)>>);

#[reverie::global_tool]
impl GlobalTool for SourceRefusals {
    type Config = ();
    type Request = (bool, bool);
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, result: Self::Request) {
        self.0.lock().unwrap().push(result);
    }
}

#[derive(Default)]
struct FunctionSourceTool;

#[reverie::tool]
impl Tool for FunctionSourceTool {
    type GlobalState = SourceRefusals;
    type ThreadState = bool;

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::write].into_iter().collect()
    }

    async fn handle_initial_stop<G: Guest<Self>>(
        &self,
        _guest: &mut G,
        _observation: &dyn InitialCommandObservation,
    ) -> Result<(), Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "function acquired Command stop"
        )))
    }

    async fn handle_initial_exec<G: Guest<Self>>(
        &self,
        _guest: &mut G,
        _observation: &dyn InitialCommandObservation,
    ) -> Result<(), Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "function acquired Command exec"
        )))
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        *guest.thread_state_mut() = true;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = call.into_parts();
        let result = guest
            .read_native_source(args.arg1, args.arg2, Box::new(()))
            .await;
        let refused = matches!(
            result,
            Err(NativeUserReadError::Refused(
                NativeUserReadRefusal::TargetState(Errno::ENOTSUPP)
            ))
        );
        eprintln!(
            "function source after_exec={}: {result:?}",
            guest.thread_state()
        );
        guest.send_rpc((*guest.thread_state(), refused)).await;
        Ok(guest.inject(call).await?)
    }
}

async fn spawn_actual_function_source_control() -> Tracer<SourceRefusals> {
    spawn_fn::<FunctionSourceTool, _>(|| unsafe {
        if libc::write(1, c"before".as_ptr().cast(), 6) != 6 {
            libc::_exit(91);
        }
        let args = [
            c"/bin/sh".as_ptr(),
            c"-c".as_ptr(),
            c"printf after".as_ptr(),
            std::ptr::null(),
        ];
        libc::execv(args[0], args.as_ptr());
        libc::_exit(92);
    })
    .await
    .expect("spawn actual function source control")
}

fn open_function_pidfd(pid: Pid) -> Result<OwnedFd, Errno> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
    if raw < 0 {
        Err(Errno::last())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
    }
}

#[derive(Debug)]
struct FunctionSourceSetupFailure {
    error: Errno,
    completion: Option<crate::ToolRunCompletion<SourceRefusals, Output>>,
}

// The caller keeps the actual Tracer until this control consumes its wait.
// Only the extra post-spawn acquisition is injectable; spawn and completion
// still use their real owners. Returning the setup error lets the BEFORE
// witness rescue that owner without unwinding through a live Tracer. A setup
// failure carries a completion receipt only if the control actually got one.
async fn run_actual_function_source_control(
    tracer: &mut Option<Tracer<SourceRefusals>>,
    extra_pidfd_open: impl FnOnce(Pid) -> Result<OwnedFd, Errno>,
) -> Result<(), Box<FunctionSourceSetupFailure>> {
    let pid = tracer.as_ref().unwrap().guest_pid();
    let termination = tracer.as_ref().unwrap().termination_handle().unwrap();
    let mut completion = Box::pin(tracer.take().unwrap().wait_with_output_completion());
    let pidfd = match extra_pidfd_open(pid) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            // The extra diagnostic descriptor is not cleanup authority. Keep
            // driving the original owner even when no new fd can be opened.
            termination.terminate(Error::Errno(error));
            let receipt = match tokio::time::timeout(Duration::from_secs(2), &mut completion).await
            {
                Ok(ToolRunOutcome::Complete(done)) => Some(done),
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    std::mem::forget(pending);
                    eprintln!(
                        "function pidfd setup failure {error}: original pending owner retained"
                    );
                    None
                }
                Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                    std::mem::forget(tracer);
                    eprintln!(
                        "function pidfd setup failure {error}: original unsupported owner retained"
                    );
                    None
                }
                Err(_) => {
                    std::mem::forget(completion);
                    eprintln!(
                        "function pidfd setup failure {error}: original completion retained at 2s rescue bound"
                    );
                    None
                }
            };
            return Err(Box::new(FunctionSourceSetupFailure {
                error,
                completion: receipt,
            }));
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut completion).await;
    let original_complete = matches!(&outcome, Ok(ToolRunOutcome::Complete(_)));
    let done = match outcome {
        Ok(ToolRunOutcome::Complete(done)) => done,
        other => {
            termination.terminate(Error::Tool(anyhow::anyhow!("function source deadline")));
            let signal = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    0,
                    0,
                )
            };
            eprintln!(
                "function source rescue signal={signal}, errno={}",
                Errno::last()
            );
            let rescued = match other {
                Err(_) => tokio::time::timeout(Duration::from_secs(2), &mut completion).await,
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    let mut resume = Box::pin(pending.resume_cleanup());
                    let result = tokio::time::timeout(Duration::from_secs(2), &mut resume).await;
                    if result.is_err() {
                        std::mem::forget(resume);
                    }
                    result
                }
                Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                    std::mem::forget(tracer);
                    panic!("actual function source control unsupported");
                }
                Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
            };
            match rescued {
                Ok(ToolRunOutcome::Complete(done)) => done,
                other => {
                    std::mem::forget(other);
                    std::mem::forget(completion);
                    panic!("actual function source cleanup incomplete; original owner retained");
                }
            }
        }
    };
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(
        poll.revents & libc::POLLHUP,
        0,
        "original function must be reaped"
    );
    assert!(
        original_complete,
        "rescue is cleanup evidence, never a successful run"
    );
    let output = done.result.expect("actual function source run failed");
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(output.stdout, b"beforeafter");
    let observations = done.global_state.0.lock().unwrap();
    assert!(
        observations.iter().any(|(after, _)| !after),
        "missing pre-exec source attempt"
    );
    assert!(
        observations.iter().any(|(after, _)| *after),
        "missing post-exec source attempt"
    );
    assert!(
        observations.iter().all(|(_, refused)| *refused),
        "wrong source verdict: {observations:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn actual_function_source_refuses_before_and_after_exec() {
    let mut tracer = Some(spawn_actual_function_source_control().await);
    run_actual_function_source_control(&mut tracer, open_function_pidfd)
        .await
        .expect("retain original function pidfd");
}

#[tokio::test(flavor = "current_thread")]
async fn actual_function_source_pidfd_failure_keeps_original_owner() {
    FATAL_REAP_OBSERVATIONS.with(|slot| {
        assert!(slot.borrow().is_none(), "no other owner observation active");
        *slot.borrow_mut() = Some(Vec::new());
    });
    let mut tracer = Some(spawn_actual_function_source_control().await);
    let original = tracer.as_ref().unwrap();
    let pid = original.guest_pid();
    let session = original.ordinary_session.clone();
    let termination = original.termination_handle();
    let Some(termination) = termination else {
        std::mem::forget(tracer);
        panic!("function control has no termination handle; original owner retained");
    };
    let same_session = Arc::ptr_eq(&session, &termination.session);
    let groups_at_spawn = session.retained_group_counts_for_test();
    let failed_at_spawn = session.is_failed();
    let mut injected_pids = Vec::new();
    let result = run_actual_function_source_control(&mut tracer, |actual_pid| {
        injected_pids.push(actual_pid);
        Err(Errno::EMFILE)
    })
    .await;

    // Seal the control's result before the separate outer rescue. These
    // reads cannot signal, reap, or manufacture a replacement stopped owner.
    let consumed_before_rescue = tracer.is_none();
    let returned_error = result.as_ref().err().map(|failure| failure.error);
    let completed_with_error_before_rescue = result.as_ref().err().is_some_and(|failure| {
        failure.completion.as_ref().is_some_and(|done| {
            matches!(
                &done.result,
                Err(failure) if matches!(failure.primary(), Error::Errno(Errno::EMFILE))
            )
        })
    });
    let groups_before_rescue = session.retained_group_counts_for_test();
    let tasks_before_rescue = session.unconfirmed_task_count();
    let root_retired_before_rescue = FATAL_REAP_OBSERVATIONS.with(|slot| {
        slot.borrow().as_ref().is_some_and(|roots| {
            roots.len() == 1
                && roots[0].tid == pid
                && matches!(
                    roots[0].terminal.observed_terminal(),
                    Some(Ok(ExitStatus::Signaled(Signal::SIGKILL, false)))
                )
                && roots[0].terminal.wait(Duration::ZERO)
                && roots[0].terminal.is_reaped() == Ok(true)
                && roots[0].held.lock().unwrap().is_none()
        })
    });

    // BEFORE returns EMFILE without reaching the consuming wait. Retain that
    // exact Tracer and drive it here so the failing test does not leak a guest.
    // This rescue can never satisfy the sealed predicate above.
    let mut outer_rescue_complete = false;
    let mut outer_rescue_kept_error = false;
    if let Some(original) = tracer.take() {
        termination.terminate(Error::Errno(Errno::EMFILE));
        let mut completion = Box::pin(original.wait_with_output_completion());
        match tokio::time::timeout(Duration::from_secs(2), &mut completion).await {
            Ok(ToolRunOutcome::Complete(done)) => {
                outer_rescue_complete = true;
                outer_rescue_kept_error = matches!(
                    &done.result,
                    Err(failure) if matches!(failure.primary(), Error::Errno(Errno::EMFILE))
                );
            }
            Ok(other) => {
                std::mem::forget(other);
                eprintln!("function pidfd witness: original cleanup owner retained unresolved");
            }
            Err(_) => {
                std::mem::forget(completion);
                eprintln!(
                    "function pidfd witness: original completion retained at 2s rescue bound"
                );
            }
        }
    }
    let roots = FATAL_REAP_OBSERVATIONS.with(|slot| slot.borrow_mut().take().unwrap());
    let root_receipts: Vec<_> = roots
        .iter()
        .map(|root| {
            (
                root.tid,
                root.terminal.observed_terminal(),
                root.terminal.wait(Duration::ZERO),
                root.terminal.is_reaped(),
                root.held.lock().unwrap().is_none(),
            )
        })
        .collect();
    eprintln!(
        "function pidfd custody: pid={pid}, error={result:?}, injected_pids={injected_pids:?}, \
         same_session={same_session}, failed_at_spawn={failed_at_spawn}, \
         groups_at_spawn={groups_at_spawn:?}, consumed_before_rescue={consumed_before_rescue}, \
         completed_with_error_before_rescue={completed_with_error_before_rescue}, \
         groups_before_rescue={groups_before_rescue:?}, tasks_before_rescue={tasks_before_rescue}, \
         root_retired_before_rescue={root_retired_before_rescue}, \
         outer_rescue_complete={outer_rescue_complete}, outer_rescue_kept_error={outer_rescue_kept_error}, \
         root_receipts_after={root_receipts:?}"
    );
    assert!(same_session && !failed_at_spawn);
    assert_eq!(groups_at_spawn, (1, 0, 0));
    assert_eq!(
        injected_pids,
        vec![pid],
        "inject only the extra original-root acquisition"
    );
    assert_eq!(
        returned_error,
        Some(Errno::EMFILE),
        "setup failure must remain a failure"
    );
    assert!(
        consumed_before_rescue
            && completed_with_error_before_rescue
            && root_retired_before_rescue
            && groups_before_rescue == (0, 0, 0)
            && tasks_before_rescue == 0,
        "post-spawn pidfd failure escaped without original completion; outer rescue is not a pass"
    );
}
