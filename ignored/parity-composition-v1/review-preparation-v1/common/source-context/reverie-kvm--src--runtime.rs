// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 212..222; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    /// Executes `request` and returns its raw Linux syscall result.
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64;
}

impl<F> SyscallExecutor for F
where
    F: FnMut(&SyscallRequest, &GuestMemory) -> i64 + Send + Sync,
{
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        self(request, memory)
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 248..272; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    pub(crate) subscriptions: Subscription,
    // TODO-HUMAN-REVIEW(PR-235): Review child release on parent handler suspension.
    pub(crate) pending_child_starts: SharedChildStarts,
}

// TODO-HUMAN-REVIEW(PR-192): Review async KVM process-action completion.
trait GuestSyscallExecutor<T: Tool>: Send + Sync {
    fn read_clock(&self) -> Result<u64>;

    fn has_cpuid_interception(&self) -> bool {
        false
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64;

    /// Reserve backend bookkeeping before executing an injected syscall.
    /// Refusal is terminal backend failure, never an emulated syscall errno.
    fn prepare_signal_effects(&mut self) -> std::result::Result<(), Errno> {
        Ok(())
    }

    fn failure_subscription(&self) -> Option<crate::failure::FailureSubscription> {
        None
    }


// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 373..379; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    fn ordinary_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        true
    }

    fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        true
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 394..554; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    fn complete_injection<'a>(
        &'a mut self,
        _context: ToolContext<'a, T>,
    ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
    where
        T: 'a,
    {
        Box::pin(async {
            Ok(InjectionCompletion::Returns {
                syscall_result: None,
            })
        })
    }
}

struct DirectSyscallExecutor<'a> {
    executor: &'a mut dyn SyscallExecutor,
    vcpu: &'a crate::clock::CountedVcpu,
}

impl<T: Tool> GuestSyscallExecutor<T> for DirectSyscallExecutor<'_> {
    fn read_clock(&self) -> Result<u64> {
        self.vcpu.read_clock()
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        self.executor.execute(request, memory)
    }
}

#[derive(Clone)]
enum ProcessExecutionContext {
    InitialExec(SyscallRequest),
    InitialExecCompleted,
    Lifecycle,
    Instruction,
    ThreadEntrySignal,
    SignalBoundary(CompletedSyscallBoundary),
    FaultBoundary(Box<PageZeroFault>),
    SyscallBoundary(CompletedSyscallBoundary),
}

impl ProcessExecutionContext {
    fn has_resumable_signal_boundary(&self) -> bool {
        // These contexts carry an exact user continuation for a signal frame.
        // Lifecycle and initial-exec callbacks have no such transport.
        matches!(
            self,
            Self::SignalBoundary(_)
                | Self::FaultBoundary(_)
                | Self::SyscallBoundary(_)
                | Self::ThreadEntrySignal
        )
    }

    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Instruction) {
            // Terminal exits do not need a syscall return frame. Every other
            // tail still requires a transport that can consume its result.
            return injection_is_explicit_exit(request);
        }
        !matches!(
            self,
            Self::SignalBoundary(_) | Self::FaultBoundary(_) | Self::ThreadEntrySignal
        )
    }

    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Instruction) && injection_is_explicit_exit(request) {
            // inject() itself is nonreturning once the real exit is staged;
            // tail_inject() uses that same path and both executor preflights.
            return true;
        }
        if matches!(self, Self::ThreadEntrySignal | Self::Instruction) {
            // There is a real user continuation, but no consumed syscall
            // transport to restore after an injected process action.
            return !injection_can_be_nonreturning(request)
                && !matches!(
                    request.number() as libc::c_long,
                    libc::SYS_fork | libc::SYS_vfork | libc::SYS_clone | libc::SYS_clone3
                );
        }
        !matches!(self, Self::SignalBoundary(_) | Self::FaultBoundary(_))
            || !injection_can_be_nonreturning(request)
    }

    fn injected_signal_allowed(&self, request: &SyscallRequest) -> bool {
        !signal_request_requires_return_frame(request)
            || matches!(
                self,
                Self::SignalBoundary(_)
                    | Self::SyscallBoundary(_)
                    | Self::FaultBoundary(_)
                    | Self::ThreadEntrySignal
            )
    }
}

fn injection_is_explicit_exit(request: &SyscallRequest) -> bool {
    matches!(
        request.number() as libc::c_long,
        libc::SYS_exit | libc::SYS_exit_group
    )
}

/// Returns whether a successful injected syscall can abandon the current Tool
/// handler instead of producing an ordinary scalar result.
fn injection_can_be_nonreturning(request: &SyscallRequest) -> bool {
    match request.number() {
        number
            if number == libc::SYS_execve as u64
                || number == libc::SYS_execveat as u64
                || number == libc::SYS_exit as u64
                || number == libc::SYS_exit_group as u64 =>
        {
            true
        }
        number if number == libc::SYS_kill as u64 || number == libc::SYS_tkill as u64 => {
            request.args()[1] == libc::SIGKILL as u64
        }
        number if number == libc::SYS_tgkill as u64 => request.args()[2] == libc::SIGKILL as u64,
        _ => false,
    }
}

/// Returns whether an injected request is the already-installed initial exec.
///
/// Tools may forward the synthetic `execve` unchanged, or use Reverie's
/// canonical `From<Execve> for Execveat` conversion. Only those two exact
/// requests are equivalent: accepting any other `execveat` would suppress a
/// real image replacement requested by the tool.
fn matches_initial_exec(expected: &SyscallRequest, request: &SyscallRequest) -> bool {
    if expected.number() != libc::SYS_execve as u64 || expected.args()[3..] != [0, 0, 0] {
        return false;
    }
    if expected == request {
        return true;
    }

    let [path, argv, envp, _, _, _] = *expected.args();
    request.number() == libc::SYS_execveat as u64
        && *request.args() == [libc::AT_FDCWD as u64, path, argv, envp, 0, 0]
}

fn signal_request_requires_return_frame(request: &SyscallRequest) -> bool {
    let signal = match request.number() {
        number if number == libc::SYS_kill as u64 || number == libc::SYS_tkill as u64 => {
            request.args()[1]
        }
        number if number == libc::SYS_tgkill as u64 => request.args()[2],
        _ => 0,
    };
    // Signal zero is an existence/permission probe and cannot create pending
    // delivery, so it is safe in lifecycle callbacks without a return frame.
    signal != 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalGuard {
    Ordinary,
    Observation,

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 572..630; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    fn read_clock(&self) -> Result<u64> {
        self.backend.vcpu.read_clock()
    }

    fn has_cpuid_interception(&self) -> bool {
        self.backend.has_cpuid_interception()
    }

    fn failure_subscription(&self) -> Option<crate::failure::FailureSubscription> {
        self.backend
            .tool_failure
            .as_ref()
            .map(|failure| failure.run.subscribe())
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        if !self.signal_injection_allowed(request) {
            // KvmGuest performs the same check before dispatch. Keep the
            // production executor fail-closed as well: a future Guest caller
            // must not execute an irreversible transition and only then learn
            // that SignalBoundary cannot resume its Tool hook.
            return -(i64::from(Errno::ENOSYS.into_raw()));
        }
        if matches!(
            self.process_context,
            ProcessExecutionContext::Lifecycle | ProcessExecutionContext::Instruction
        ) && let Some(result) = self
            .executor
            .lifecycle_signal_mask_preflight(request, memory)
        {
            return result;
        }
        if !self.process_context.injected_signal_allowed(request) {
            // A successful injected self-signal would become pending, but a
            // lifecycle callback has no transported userspace context in which
            // to run the structured hook or build a signal frame. Refuse before
            // mutating pending state instead of delaying it to another syscall.
            return -(i64::from(Errno::ENOSYS.into_raw()));
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-233): Review synthetic initial exec completion.
        if matches!(
            &self.process_context,
            ProcessExecutionContext::InitialExec(expected)
                if matches_initial_exec(expected, request)
        ) {
            self.last_result = Some(0);
            self.process_context = ProcessExecutionContext::InitialExecCompleted;
            return 0;
        }
        let result = self.executor.execute(request, memory);
        self.last_result = Some(result);
        result
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 814..822; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.signal_injection_allowed(request)
    }

    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.signal_guard == SignalGuard::Ordinary
            && !self.executor.has_prepared_signal()
            && self.process_context.tail_injection_allowed(request)
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 832..949; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    fn complete_injection<'a>(
        &'a mut self,
        context: ToolContext<'a, T>,
    ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
    where
        T: 'a,
    {
        Box::pin(async move {
            if matches!(
                self.process_context,
                ProcessExecutionContext::InitialExecCompleted
            ) {
                *self.process_completed = true;
                return Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: true,
                    process_exited: false,
                });
            }
            let Some(action) = self.executor.take_process_action() else {
                return Ok(if self.executor.has_pending_exit() {
                    InjectionCompletion::DoesNotReturn {
                        image_replaced: false,
                        process_exited: true,
                    }
                } else {
                    InjectionCompletion::Returns {
                        syscall_result: None,
                    }
                });
            };
            if !action.returns_to_original_image() && self.executor.has_eligible_pending_signal() {
                return Err(Error::UnexpectedVcpuExit(
                    "KVM Tool exec with an eligible deferred signal is unsupported; \
                     delivery requires a syscall return frame"
                        .to_owned(),
                ));
            }
            hide_tool_scratch(&self.memory, self.backend.tool_stack_top())?;
            let action_result: Result<ProcessActionOutcome> = async {
                match self.process_context.clone() {
                    ProcessExecutionContext::FaultBoundary(fault) => {
                        self.backend
                            .run_process_action_with_tool_from_fault(
                                self.executor,
                                action,
                                context,
                                &fault,
                            )
                            .await
                    }
                    ProcessExecutionContext::SignalBoundary(boundary)
                    | ProcessExecutionContext::SyscallBoundary(boundary) => {
                        let result = self
                            .last_result
                            .expect("process action must have an injected syscall result");
                        boundary.stage_action_result(self.backend, result)?;
                        let continuation =
                            ProcessActionContinuation::from_captured(&action, boundary);
                        self.backend
                            .run_process_action_with_tool_at_boundary(
                                self.executor,
                                action,
                                context,
                                continuation,
                            )
                            .await
                    }
                    ProcessExecutionContext::InitialExec(_)
                    | ProcessExecutionContext::Lifecycle => match action {
                        ProcessAction::Exec {
                            executable_path,
                            executable_file,
                            image,
                            argv,
                            envp,
                        } => {
                            self.backend.exec_process(
                                self.executor,
                                (&executable_path, executable_file),
                                &image,
                                &argv,
                                &envp,
                            )?;
                            Ok(ProcessActionOutcome::replaced())
                        }
                        _ => Err(Error::UnexpectedVcpuExit(
                            "fork/clone injection requires a guest syscall boundary".to_owned(),
                        )),
                    },
                    ProcessExecutionContext::InitialExecCompleted => unreachable!(
                        "synthetic initial exec completes before process actions are inspected"
                    ),
                    ProcessExecutionContext::Instruction => Err(Error::UnexpectedVcpuExit(
                        "process injection from an instruction callback is unsupported".to_owned(),
                    )),
                    ProcessExecutionContext::ThreadEntrySignal => Err(Error::UnexpectedVcpuExit(
                        "process injection from a thread-entry signal hook is unsupported"
                            .to_owned(),
                    )),
                }
            }
            .await;
            let expose_result = expose_tool_scratch(&self.memory, self.backend.tool_stack_top());
            let outcome = action_result?;
            expose_result?;
            *self.process_completed = true;
            if outcome.image_replaced || self.executor.has_pending_exit() {
                Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: outcome.image_replaced,
                    process_exited: self.executor.has_pending_exit(),
                })
            } else {
                Ok(InjectionCompletion::Returns {
                    syscall_result: Some(outcome.syscall_result),
                })
            }
        })
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 1091..1115; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T: Tool> Guest<T> for KvmGuest<'_, T> {
    fn has_cpuid_interception(&self) -> bool {
        self.executor.has_cpuid_interception()
    }

    type Memory = GuestMemory;
    type Stack = KvmStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 1348..1352; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    fn read_clock(&mut self) -> std::result::Result<u64, reverie::Error> {
        self.executor
            .read_clock()
            .map_err(|error| reverie::Error::Io(std::io::Error::other(error)))
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 2344..2551; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    /// Runs the installed guest program through a shared Reverie `Tool`.
    ///
    /// The executor supplies Linux syscall semantics that a future guest kernel
    /// will provide. Tool lifecycle, typed syscall dispatch, thread state,
    /// global RPC, memory, stack, injection, and tail injection use the same
    /// Reverie contracts as the ptrace backend.
    pub async fn run_with_tool<T, E>(
        &mut self,
        config: <T::GlobalState as GlobalTool>::Config,
        mut executor: E,
    ) -> Result<T::GlobalState>
    where
        T: Tool,
        E: SyscallExecutor,
    {
        // This public non-ELF loop has no instruction consumer. Establish its
        // ownership locally even if the vCPU previously ran a subscribed Tool.
        self.set_rdtsc_interception(false)?;
        self.set_cpuid_interception(false)?;
        self.vcpu.track_clock()?;
        let tool_stack_top = self.tool_stack_top();
        let pid = Pid::from_raw(self.root_pid);
        let global_state = T::GlobalState::init_global_state(&config).await;
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let mut thread_state = tool.init_thread_state(pid, None);
        let memory = self.memory.clone();
        let auxv = Vec::new();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let outcome: Result<ExitStatus> = async {
            let registers = kvm_registers(self.vcpu.get_regs()?, 0);
            let handler_signal = Arc::new(Mutex::new(None));
            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
            expose_tool_scratch(&memory, tool_stack_top)?;
            let start_outcome = {
                let mut guest_executor = DirectSyscallExecutor {
                    executor: &mut executor,
                    vcpu: &self.vcpu,
                };
                let mut guest = KvmGuest::<T>::new(
                    pid,
                    // run_with_tool drives a single root thread (tid == pid).
                    pid,
                    tool.clone(),
                    memory.clone(),
                    &auxv,
                    registers,
                    &mut thread_state,
                    &mut guest_executor,
                    &global_state,
                    None,
                    &config,
                    &subscriptions,
                    handler_signal.clone(),
                    pending_child_starts.clone(),
                    tool_stack_top,
                    stack_checked_out.clone(),
                );
                drive_handler(
                    tool.handle_thread_start(&mut guest),
                    handler_signal,
                    pending_child_starts,
                    wait_for_failure(&global_state, None),
                )
                .await
            };
            hide_tool_scratch(&memory, tool_stack_top)?;
            match start_outcome {
                HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
                HandlerOutcome::ThreadCancelled => {
                    return Ok(ExitStatus::SUCCESS);
                }
                HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                    return Err(Error::UnexpectedVcpuExit(
                        "parked outcome outside its original syscall callback".to_owned(),
                    ));
                }
                HandlerOutcome::RuntimeError(error) => return Err(error),
                HandlerOutcome::TailInjected { .. } => {}
            }

            loop {
                let vcpu_exit = self.vcpu.run()?;
                Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                match vcpu_exit {
                    VcpuExit::Hypercall(exit) => {
                        if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                            return Err(Error::UnexpectedHypercall(exit.nr));
                        }
                        let frame_address = exit.args[0];
                        let return_slot = std::ptr::from_mut(exit.ret) as usize;
                        let registers = self.vcpu.get_regs()?;
                        let request = SyscallRequest::read_from(&memory, frame_address)?;
                        let syscall = request.into_syscall()?;
                        let subscribed = subscriptions
                            .iter_syscalls()
                            .any(|number| number == syscall.number());
                        let result = if subscribed {
                            // Same `ERESTARTSYS` restart protocol as the
                            // process-syscall path below; see
                            // `classify_handler_result`.
                            loop {
                                let handler_signal = Arc::new(Mutex::new(None));
                                let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                                expose_tool_scratch(&memory, tool_stack_top)?;
                                let outcome = {
                                    let mut guest_executor = DirectSyscallExecutor {
                                        executor: &mut executor,
                                        vcpu: &self.vcpu,
                                    };
                                    let mut guest = KvmGuest::<T>::new(
                                        pid,
                                        // run_with_tool drives a single root thread.
                                        pid,
                                        tool.clone(),
                                        memory.clone(),
                                        &auxv,
                                        kvm_registers(registers, request.number()),
                                        &mut thread_state,
                                        &mut guest_executor,
                                        &global_state,
                                        None,
                                        &config,
                                        &subscriptions,
                                        handler_signal.clone(),
                                        pending_child_starts.clone(),
                                        tool_stack_top,
                                        stack_checked_out.clone(),
                                    );
                                    drive_handler(
                                        tool.handle_syscall_event(&mut guest, syscall),
                                        handler_signal,
                                        pending_child_starts,
                                        wait_for_failure(&global_state, None),
                                    )
                                    .await
                                };
                                hide_tool_scratch(&memory, tool_stack_top)?;
                                break match outcome {
                                    HandlerOutcome::Returned(result) => {
                                        match classify_handler_result(result)? {
                                            Some(raw) => raw,
                                            None => continue,
                                        }
                                    }
                                    HandlerOutcome::TailInjected { result, .. } => {
                                        result_to_raw(result)
                                    }
                                    HandlerOutcome::ThreadCancelled => {
                                        return Ok(ExitStatus::SUCCESS);
                                    }
                                    HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                                    HandlerOutcome::ParkedFatal(_)
                                    | HandlerOutcome::ParkedCancelled(_) => {
                                        return Err(Error::UnexpectedVcpuExit(
                                            "parked outcome outside its original syscall callback"
                                                .to_owned(),
                                        ));
                                    }
                                    HandlerOutcome::RuntimeError(error) => return Err(error),
                                };
                            }
                        } else {
                            executor.execute(&request, &memory)
                        };
                        // SAFETY: return_slot points into this vCPU's stable KVM_RUN
                        // mapping. The vCPU remains stopped and is not run again while
                        // the tool callback is active.
                        unsafe {
                            (return_slot as *mut u64).write(result as u64);
                        }
                    }
                    VcpuExit::Hlt => {
                        return Ok(ExitStatus::SUCCESS);
                    }
                    exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
                }
            }
        }
        .await;
        if outcome.is_err() {
            global_state.report_backend_failure(reverie::BackendFailure {
                pid,
                tid: pid,
                phase: "direct Tool execution",
            });
        }
        let status = outcome.as_ref().copied().unwrap_or(ExitStatus::Exited(255));
        let cleanup = self
            .notify_tool_exit(
                tool,
                (pid, pid),
                &global_state,
                &config,
                thread_state,
                status,
            )
            .await;
        Error::combine(
            [outcome.map(|_| ()), cleanup]
                .into_iter()
                .filter_map(Result::err)
                .collect(),
        )?;
        Ok(global_state)
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 2590..2671; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    /// Runs the installed ELF and retains GlobalState even after a runtime
    /// failure, so its owner can finish scheduler/global cleanup. An outer
    /// error means setup failed before the global state existed, or ownership
    /// could not be recovered after all owned children were joined.
    pub async fn run_static_elf_with_tool_completion<T>(
        &mut self,
        config: <T::GlobalState as GlobalTool>::Config,
        capture_output: bool,
    ) -> Result<ToolRunCompletion<T::GlobalState>>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        // Resolve thread ownership before any CLONE_THREAD worker is created: an
        // explicit caller override wins, otherwise follow the tool's
        // `Tool::thread_ownership` (default: Tool-owned "follow children"). This
        // is why the KVM backend no longer needs the caller to opt threads in.
        self.resolve_thread_ownership(T::thread_ownership(&config));
        if T::observe_signal_dequeues(&config) && self.thread_ownership.executes_on_host() {
            // This combination is known before GlobalState initialization or
            // consuming the installed image. An uninstrumented worker has no
            // removing Guest on which to acknowledge its pending-state effects.
            return Err(Error::SignalObservationRequiresToolThreads);
        }
        // Capture setup can fail before image consumption or any Tool state.
        // Keep this root owner until the later executor and its workers retire.
        let capture_owner = self.prepare_captured_output(capture_output)?;
        let mut loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        // Output capture replaces stdout and stderr with the executor's pipes,
        // but an explicitly configured stdin remains the guest's input. Use
        // /dev/null only when the caller supplied no stdin, matching
        // `run_static_elf_captured` while keeping the no-input default.
        if capture_output && loaded.stdin.is_none() {
            loaded.stdin = Some(std::fs::File::open("/dev/null")?);
        }
        let pid = Pid::from_raw(self.root_pid);
        let global_state = Arc::new(T::GlobalState::init_global_state(&config).await);
        let failure = RunFailure::new(&global_state);
        self.tool_failure = Some(FailureContext::new(failure.clone(), pid, pid));
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let thread_state = tool.init_thread_state(pid, None);
        let mut executor = ElfExecutor::with_output(loaded, capture_owner.clone());
        let result = self
            .run_static_elf_process_with_tool(
                &mut executor,
                pid,
                // The root process leader has tid == pid.
                pid,
                tool,
                thread_state,
                global_state.clone(),
                &config,
                &subscriptions,
                true,
            )
            .await;
        // All owned children have returned. Do not leave the backend holding
        // a reporter whose Weak owner is about to be consumed or released.
        self.tool_failure = None;
        let global_state = match Arc::try_unwrap(global_state) {
            Ok(global) => global,
            Err(_) => {
                let ownership = Error::UnexpectedVcpuExit(
                    "KVM child retained global Tool state after exit".to_owned(),
                );
                return Err(match result {
                    Err(primary) => primary.with_cleanup(vec![ownership]),
                    Ok(_) => ownership,
                });
            }
        };
        let result = failure
            .complete(result)
            .map(|(status, stdout, stderr)| (conventional_exit_code(status), stdout, stderr));
        Ok(ToolRunCompletion {
            global_state,
            result,
        })
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 2924..3859; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
    // TODO-HUMAN-REVIEW(PR-192): Review recursive KVM process Tool runtime.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_static_elf_process_with_tool<T>(
        &mut self,
        executor: &mut ElfExecutor,
        pid: Pid,
        tid: Pid,
        tool: Arc<T>,
        mut thread_state: T::ThreadState,
        global_state: Arc<T::GlobalState>,
        config: &<T::GlobalState as GlobalTool>::Config,
        subscriptions: &Subscription,
        initial_post_exec: bool,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        if let Some(failure) = &self.tool_failure {
            self.tool_failure = Some(failure.for_thread(tid));
        }
        executor.observe_ignored_signals_with_tool();
        if T::observe_signal_dequeues(config) {
            executor.enable_signal_dequeues();
        }
        let tool_stack_top = self.tool_stack_top();
        let failure_subscription = self.failure_subscription(executor.is_traced_tree_root());
        let mut _registration = None;
        let mut auxv = executor.auxv().to_vec();
        // Clones share the MAP_SHARED guest mapping; a mutable handle lets the
        // loop write syscall results back into the guest's frame.
        let mut memory = self.memory.clone();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let mut _process_completed = false;
        let outcome: Result<ToolProcessExit> = async {
            // Each actual Tool consumer admits its own subscription before
            // any user instruction. New fork/thread vCPUs start unarmed; the
            // tool-less Host worker loop never inherits trapping without a hook.
            self.set_rdtsc_interception(subscriptions.has_rdtsc())?;
            self.set_cpuid_interception(subscriptions.has_cpuid())?;
            self.vcpu.track_clock()?;
            _registration = Some(self.register_guest_thread()?);
            let registers = kvm_registers(self.vcpu.get_regs()?, 0);
            expose_tool_scratch(&memory, tool_stack_top)?;
            let start_outcome = {
                let mut guest_executor = StaticElfSyscallExecutor {
                    backend: self,
                    executor,
                    memory: memory.clone(),
                    process_context: ProcessExecutionContext::Lifecycle,
                    callback_site: None,
                    original_syscall: None,
                    signal_guard: SignalGuard::Ordinary,
                    last_result: None,
                    process_completed: &mut _process_completed,
                };
                let mut guest = KvmGuest::<T>::new(
                    pid,
                    tid,
                    tool.clone(),
                    memory.clone(),
                    &auxv,
                    registers,
                    &mut thread_state,
                    &mut guest_executor,
                    global_state.as_ref(),
                    Some(global_state.clone()),
                    config,
                    subscriptions,
                    handler_signal.clone(),
                    pending_child_starts.clone(),
                    tool_stack_top,
                    stack_checked_out.clone(),
                );
                drive_handler(
                    tool.handle_thread_start(&mut guest),
                    handler_signal,
                    pending_child_starts.clone(),
                    wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
                )
                .await
            };
            hide_tool_scratch(&memory, tool_stack_top)?;
            match start_outcome {
                HandlerOutcome::ThreadCancelled => {
                    self.start_pending_tool_children(executor, &pending_child_starts)?;
                    return Ok(self.cancelled_tool_thread_status(executor));
                }
                HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
                HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                    return Err(Error::UnexpectedVcpuExit(
                        "parked outcome outside its original syscall callback".to_owned(),
                    ));
                }
                HandlerOutcome::RuntimeError(error) => return Err(error),
                HandlerOutcome::TailInjected { .. } => {}
            }
            if self.guest_thread_is_cancelled() {
                // The parent releases the start gate after it has begun scheduler
                // registration. handle_thread_start completes the child-side
                // ordering. Preserve handle_thread_start -> on_exit ordering,
                // but do not execute post-exec hooks or a guest instruction after
                // cancellation. Clear CHILD_CLEARTID immediately before the Tool
                // exit callback; the worker wrapper then observes that the address
                // has already been consumed.
                return Ok(self.cancelled_tool_thread_status(executor));
            }
            auxv = executor.auxv().to_vec();
            if let Some(exit) = executor.take_exit() {
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                return Ok(exit.into());
            }

            if initial_post_exec {
                // The root ELF image is already installed when this backend begins.
                // Present the same initial exec syscall and successful-exec lifecycle
                // boundaries as ptrace without loading the installed image twice.
                if subscriptions
                    .iter_syscalls()
                    .any(|number| number == reverie::syscalls::Sysno::execve)
                {
                    let initial_outcome = run_initial_exec_handler(
                        self,
                        &tool,
                        pid,
                        &memory,
                        &auxv,
                        &mut thread_state,
                        executor,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                    )
                    .await?;
                    if matches!(initial_outcome, CallbackOutcome::ThreadCancelled) {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                    auxv = executor.auxv().to_vec();
                    if let Some(exit) = executor.take_exit() {
                        if exit.group {
                            self.request_guest_thread_group_exit(exit.status);
                        }
                        return Ok(exit.into());
                    }
                }
                let post_exec_outcome = run_post_exec_handler(
                    self,
                    &tool,
                    pid,
                    &memory,
                    &mut auxv,
                    &mut thread_state,
                    executor,
                    global_state.clone(),
                    config,
                    subscriptions,
                    &stack_checked_out,
                )
                .await;
                let post_exec_error = match post_exec_outcome {
                    Ok(CallbackOutcome::ThreadCancelled) => {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                    Ok(CallbackOutcome::Completed(())) => None,
                    Err(error) => Some(error),
                };
                if let Some(error) = post_exec_error {
                    return Err(error);
                }
            }

            // CLONE_THREAD has a complete user continuation before its first
            // KVM_RUN. Admission comes first; then deliver an already queued event
            // without executing a synthetic guest syscall or the child's first
            // instruction. Root/exec lifecycle contexts keep their existing limit.
            if pid != tid && executor.has_eligible_pending_signal() {
                let entry_registers = self.vcpu.get_regs()?;
                executor.set_current_user_stack_pointer(entry_registers.rsp);
                let pending = self
                    .filter_one_pending_signal_with_tool(
                        executor,
                        pid,
                        tid,
                        &tool,
                        &memory,
                        &auxv,
                        entry_registers,
                        u64::MAX,
                        self.syscall_frame_address,
                        &mut thread_state,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                        true,
                    )
                    .await?;
                let pending = match pending {
                    CallbackOutcome::Completed(pending) => pending,
                    CallbackOutcome::ThreadCancelled => {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                };
                if !executor.has_pending_exit()
                    && let Some(pending) = pending
                {
                    self.deliver_selected_signal_before_thread_entry(
                        executor,
                        entry_registers,
                        pending,
                    )?;
                }
            }

            if let Some((segment, address)) = executor.take_segment() {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }
            if let Some(exit) = executor.take_exit() {
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                return Ok(exit.into());
            }

            // Read once so the per-syscall classifier can borrow it while `self` is
            // borrowed elsewhere in the loop body.
            let thread_ownership = self.thread_ownership;
            loop {
                if wait_for_failure(global_state.as_ref(), failure_subscription.clone())
                    .now_or_never()
                    .is_some()
                {
                    return Err(Error::RunAborted);
                }
                if let Some(status) = self.guest_thread_group_exit_status() {
                    return Ok(ProcessExit {
                        status,
                        group: true,
                    }
                    .into());
                }
                if self.guest_thread_is_cancelled() {
                    return Ok(self.cancelled_tool_thread_status(executor));
                }
                let vcpu_exit = match self.vcpu.run() {
                    Ok(exit) => exit,
                    Err(Error::Kvm(error)) if error.errno() == libc::EINTR => continue,
                    Err(error) => return Err(error),
                };
                Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                let (frame_address, return_slot) = match vcpu_exit {
                    VcpuExit::Hypercall(exit) => {
                        if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                            return Err(Error::UnexpectedHypercall(exit.nr));
                        }
                        (exit.args[0], std::ptr::from_mut(exit.ret) as usize)
                    }
                    VcpuExit::Hlt => {
                        if let Some(boundary) = self.tool_instruction_exception()? {
                            executor.set_current_user_stack_pointer(boundary.user_registers().rsp);
                            let handler_signal = Arc::new(Mutex::new(None));
                            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                            let mut process_completed = false;
                            expose_tool_scratch(&memory, tool_stack_top)?;
                            let result = {
                                let mut guest_executor = StaticElfSyscallExecutor {
                                    backend: self,
                                    executor,
                                    memory: memory.clone(),
                                    process_context: ProcessExecutionContext::Instruction,
                                    callback_site: None,
                                    original_syscall: None,
                                    signal_guard: SignalGuard::Ordinary,
                                    last_result: None,
                                    process_completed: &mut process_completed,
                                };
                                let mut guest = KvmGuest::<T>::new(
                                    pid,
                                    tid,
                                    tool.clone(),
                                    memory.clone(),
                                    &auxv,
                                    boundary.user_registers(),
                                    &mut thread_state,
                                    &mut guest_executor,
                                    global_state.as_ref(),
                                    Some(global_state.clone()),
                                    config,
                                    subscriptions,
                                    handler_signal.clone(),
                                    pending_child_starts.clone(),
                                    tool_stack_top,
                                    stack_checked_out.clone(),
                                );
                                drive_handler(
                                    async {
                                        match &boundary {
                                            crate::vm::ToolInstructionBoundary::Timestamp(
                                                boundary,
                                            ) => tool
                                                .handle_rdtsc_event(
                                                    &mut guest,
                                                    boundary.instruction.request,
                                                )
                                                .await
                                                .map(crate::vm::ToolInstructionResult::Timestamp),
                                            crate::vm::ToolInstructionBoundary::Cpuid(boundary) => {
                                                tool.handle_cpuid_event(
                                                    &mut guest,
                                                    boundary.registers.rax as u32,
                                                    boundary.registers.rcx as u32,
                                                )
                                                .await
                                                .map(crate::vm::ToolInstructionResult::Cpuid)
                                            }
                                        }
                                    },
                                    handler_signal,
                                    pending_child_starts,
                                    wait_for_failure(
                                        global_state.as_ref(),
                                        failure_subscription.clone(),
                                    ),
                                )
                                .await
                            };
                            // Hide scratch even on callback failure. The outer
                            // process supervisor retains any committed effects.
                            let hidden = hide_tool_scratch(&memory, tool_stack_top);
                            let value = match result {
                                HandlerOutcome::Returned(Ok(value)) => value,
                                HandlerOutcome::Returned(Err(error)) => {
                                    return Err(Error::Reverie(error.into())
                                        .with_cleanup(hidden.err().into_iter().collect()));
                                }
                                HandlerOutcome::ThreadCancelled => {
                                    hidden?;
                                    return Ok(self.cancelled_tool_thread_status(executor));
                                }
                                HandlerOutcome::RunFailed => {
                                    return Err(Error::RunAborted
                                        .with_cleanup(hidden.err().into_iter().collect()));
                                }
                                HandlerOutcome::RuntimeError(error) => {
                                    return Err(
                                        error.with_cleanup(hidden.err().into_iter().collect())
                                    );
                                }
                                HandlerOutcome::TailInjected {
                                    result: Ok(_),
                                    image_replaced: false,
                                    process_exited: true,
                                } => {
                                    hidden?;
                                    let exit = executor.take_exit().ok_or_else(|| {
                                        Error::UnexpectedVcpuExit(
                                            "terminal instruction injection lost its exit"
                                                .to_owned(),
                                        )
                                    })?;
                                    if exit.group {
                                        self.request_guest_thread_group_exit(exit.status);
                                    }
                                    // The original instruction never resumes. Normal
                                    // retirement and consuming hooks own cleanup.
                                    return Ok(exit.into());
                                }
                                HandlerOutcome::TailInjected { .. }
                                | HandlerOutcome::ParkedFatal(_)
                                | HandlerOutcome::ParkedCancelled(_) => {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "nonreturning instruction callback outcome".to_owned(),
                                    )
                                    .with_cleanup(hidden.err().into_iter().collect()));
                                }
                            };
                            hidden?;
                            if process_completed {
                                return Err(Error::UnexpectedVcpuExit(
                                    "instruction callback changed the process continuation"
                                        .to_owned(),
                                ));
                            }
                            if let Some((segment, address)) = executor.take_segment() {
                                set_user_segment_base(&self.vcpu, segment, address)?;
                            }
                            self.resume_tool_instruction(boundary, value)?;
                            continue;
                        }
                        if self.try_resume_vmware_backdoor_probe()? {
                            continue;
                        }
                        let Some(fault) = self.capture_page_zero_fault(executor)? else {
                            return Err(self.static_elf_halt_error()?);
                        };
                        executor.prepare_captured_page_zero_fault();
                        executor.set_current_user_stack_pointer(fault.registers.rsp);
                        let pending = self
                            .filter_next_pending_signal_with_tool(
                                executor,
                                pid,
                                tid,
                                &tool,
                                &memory,
                                &auxv,
                                fault.registers,
                                u64::MAX,
                                self.syscall_frame_address,
                                &mut thread_state,
                                &global_state,
                                config,
                                subscriptions,
                                &stack_checked_out,
                                Some(&fault),
                                false,
                            )
                            .await?;
                        let pending = match pending {
                            CallbackOutcome::Completed(pending) => pending,
                            CallbackOutcome::ThreadCancelled => {
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                        };
                        if let Some((segment, address)) = executor.take_segment() {
                            set_user_segment_base(&self.vcpu, segment, address)?;
                        }
                        if !executor.has_pending_exit() {
                            if let Some(pending) = pending {
                                self.deliver_page_zero_fault(executor, &fault, pending)?;
                            } else {
                                fault.resume_user(self, fault.registers)?;
                            }
                        }
                        if let Some(exit) = executor.take_exit() {
                            self.discard_process_clear_tid_at_signal_return(executor);
                            if exit.group {
                                self.request_guest_thread_group_exit(exit.status);
                            }
                            return Ok(exit.into());
                        }
                        continue;
                    }
                    exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
                };
                // A CLONE_THREAD worker runs on its own vCPU with a per-thread
                // syscall area, so the transported frame is `self.syscall_frame_address`
                // (equal to the root constant for the process leader, distinct for
                // each worker), not the fixed root `SYSCALL_FRAME_ADDRESS`.
                if frame_address != self.syscall_frame_address {
                    return Err(Error::UnexpectedVcpuExit(format!(
                        "syscall frame is at unexpected address {frame_address:#x}"
                    )));
                }
                let registers = self.vcpu.get_regs()?;
                let request = SyscallRequest::read_from(&memory, frame_address)?;
                // The KVM_RUN hypercall return slot is one-shot storage. Publish
                // its unused value exactly once while this decoded exit is live;
                // process actions may re-enter the vCPU before the Tool callback
                // returns, after which this pointer must never be reused.
                unsafe {
                    (return_slot as *mut u64).write(0);
                }
                let userspace =
                    process_syscall_return_registers(&memory, registers, frame_address, 0, None)?;
                executor.set_current_user_stack_pointer(userspace.rsp);
                if request.number() == libc::SYS_rt_sigreturn as u64 {
                    // `rt_sigreturn` is backend-owned: its apparent syscall result
                    // is the register file restored from the guest frame, not an
                    // ordinary scalar return value that a Tool can replace.
                    let mut signal_exit = None;
                    if let Some(restored) = self.restore_rt_sigreturn(executor, frame_address)? {
                        executor.set_current_user_stack_pointer(restored.rsp);
                        let pending = self
                            .filter_one_pending_signal_with_tool(
                                executor,
                                pid,
                                tid,
                                &tool,
                                &memory,
                                &auxv,
                                restored,
                                request.number(),
                                frame_address,
                                &mut thread_state,
                                &global_state,
                                config,
                                subscriptions,
                                &stack_checked_out,
                                false,
                            )
                            .await?;
                        let pending = match pending {
                            CallbackOutcome::Completed(pending) => pending,
                            CallbackOutcome::ThreadCancelled => {
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                        };
                        if let Some((segment, address)) = executor.take_segment() {
                            set_user_segment_base(&self.vcpu, segment, address)?;
                        }
                        signal_exit = executor.take_exit();
                        let delivered = if signal_exit.is_none()
                            && let Some(pending) = pending
                        {
                            self.deliver_selected_signal_from_registers(
                                executor,
                                frame_address,
                                restored,
                                pending,
                            )?
                        } else {
                            false
                        };
                        signal_exit = signal_exit.or_else(|| executor.take_exit());
                        if !delivered && signal_exit.is_none() {
                            stage_process_syscall_return(
                                &mut memory,
                                &self.vcpu,
                                frame_address,
                                restored,
                            )?;
                        }
                    }
                    signal_exit = signal_exit.or_else(|| executor.take_exit());
                    if let Some(exit) = signal_exit {
                        self.discard_process_clear_tid_at_signal_return(executor);
                        if exit.group {
                            self.request_guest_thread_group_exit(exit.status);
                        }
                        return Ok(exit.into());
                    }
                    continue;
                }
                let syscall = request.into_syscall()?;
                // TODO-HUMAN-REVIEW(PR-156): Review root process-syscall Tool dispatch.
                // CLONE_THREAD is deliberately NOT backend-owned: the parent's
                // clone is delivered to the Tool (Detcore) so the worker inherits
                // process-shared Tool state (fd table, memory identity) and joins
                // Detcore's scheduler. `run_process_action_with_tool` then spawns
                // the worker on the Tool loop, which issues the matching
                // `handle_thread_start` the parent's clone handler waits for.
                let backend_owned = is_backend_owned_syscall(request.number(), thread_ownership)
                    && !executor.is_random_device_read(&request)
                    && !executor.is_tool_visible_read(&request);
                let subscribed = !backend_owned
                    && subscriptions
                        .iter_syscalls()
                        .any(|number| number == syscall.number());
                let (
                    mut result,
                    handler_replaced_image,
                    _handler_process_completed,
                    restart_requested,
                ) = if subscribed {
                    let mut handler_process_completed = false;
                    // With no eligible virtual signal, ERESTARTSYS immediately
                    // re-enters the callback as before. With one, leave the loop so
                    // the structured signal hook and disposition decide whether
                    // the saved context reports EINTR or rewinds the syscall under
                    // SA_RESTART. The private errno itself never reaches userspace.
                    loop {
                        let handler_signal = Arc::new(Mutex::new(None));
                        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                        expose_tool_scratch(&memory, tool_stack_top)?;
                        let boundary =
                            CompletedSyscallBoundary::capture(self, frame_address, None)?;
                        let outcome = {
                            let mut guest_executor = StaticElfSyscallExecutor {
                                backend: self,
                                executor,
                                memory: memory.clone(),
                                process_context: ProcessExecutionContext::SyscallBoundary(boundary),
                                callback_site: None,
                                original_syscall: Some(request),
                                signal_guard: SignalGuard::Ordinary,
                                last_result: None,
                                process_completed: &mut handler_process_completed,
                            };
                            let mut guest = KvmGuest::<T>::new(
                                pid,
                                tid,
                                tool.clone(),
                                memory.clone(),
                                &auxv,
                                kvm_registers(registers, request.number()),
                                &mut thread_state,
                                &mut guest_executor,
                                global_state.as_ref(),
                                Some(global_state.clone()),
                                config,
                                subscriptions,
                                handler_signal.clone(),
                                pending_child_starts.clone(),
                                tool_stack_top,
                                stack_checked_out.clone(),
                            );
                            drive_handler(
                                tool.handle_syscall_event(&mut guest, syscall),
                                handler_signal,
                                pending_child_starts.clone(),
                                wait_for_failure(
                                    global_state.as_ref(),
                                    failure_subscription.clone(),
                                ),
                            )
                            .await
                        };
                        hide_tool_scratch(&memory, tool_stack_top)?;
                        let classified = match outcome {
                            HandlerOutcome::Returned(result) => {
                                let classified = match classify_handler_result(result) {
                                    Ok(classified) => classified,
                                    Err(error) => {
                                        return Err(self
                                            .cleanup_unstarted_tool_children_after_error(
                                                executor,
                                                &pending_child_starts,
                                                error,
                                            ));
                                    }
                                };
                                match classified {
                                    Some(raw) => (raw, false, handler_process_completed, false),
                                    None if !handler_process_completed
                                        && (executor.has_prepared_signal()
                                            || executor.has_eligible_pending_signal()) =>
                                    {
                                        // A prepared signal was already removed from
                                        // pending. Deliver its frame before another
                                        // callback invalidates the selection's nonce;
                                        // the frame's SA_RESTART policy decides whether
                                        // the syscall runs again after the handler.
                                        (-(libc::EINTR as i64), false, false, true)
                                    }
                                    // A restart is only meaningful while the process
                                    // is still live to re-run the syscall.
                                    None if !handler_process_completed => {
                                        self.start_pending_tool_children(
                                            executor,
                                            &pending_child_starts,
                                        )?;
                                        continue;
                                    }
                                    None => (
                                        -(i64::from(Errno::ERESTARTSYS.into_raw())),
                                        false,
                                        handler_process_completed,
                                        false,
                                    ),
                                }
                            }
                            HandlerOutcome::TailInjected {
                                result,
                                image_replaced,
                                ..
                            } => (
                                result_to_raw(result),
                                image_replaced,
                                handler_process_completed,
                                false,
                            ),
                            HandlerOutcome::ThreadCancelled => {
                                self.start_pending_tool_children(executor, &pending_child_starts)?;
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                            HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                            HandlerOutcome::ParkedFatal(selection) => {
                                if !executor.prepared_signal_is_fatal(selection) {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "fatal selection identity changed".to_owned(),
                                    ));
                                }
                                let pending = executor
                                    .take_prepared_signal()
                                    .map_err(|errno| Error::Reverie(errno.into()))?
                                    .expect("validated fatal selection");
                                self.deliver_selected_signal_from_registers(
                                    executor,
                                    frame_address,
                                    registers,
                                    pending,
                                )?;
                                let exit = executor.take_exit().ok_or_else(|| {
                                    Error::UnexpectedVcpuExit(
                                        "fatal parked selection did not terminate".to_owned(),
                                    )
                                })?;
                                executor.finish_parked_delivery();
                                if exit.group {
                                    self.request_guest_thread_group_exit(exit.status);
                                }
                                return Ok(exit.into());
                            }
                            HandlerOutcome::ParkedCancelled(context) => {
                                if !executor.signal_failure_context_is_current(context) {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "parked cancellation ledger identity changed".to_owned(),
                                    ));
                                }
                                return Err(executor.with_signal_effects(Error::RunAborted, None));
                            }
                            HandlerOutcome::RuntimeError(error) => {
                                let error = self.cleanup_unstarted_tool_children_after_error(
                                    executor,
                                    &pending_child_starts,
                                    error,
                                );
                                return Err(error);
                            }
                        };
                        self.start_pending_tool_children(executor, &pending_child_starts)?;
                        break classified;
                    }
                } else {
                    executor.reserve_signal_effects(64).map_err(|errno| {
                        executor.with_signal_effects(Error::Reverie(errno.into()), None)
                    })?;
                    let raw = executor.execute(&request, &memory);
                    flush_pending_signal_effects_with_tool(
                        self,
                        executor,
                        pid,
                        tid,
                        &tool,
                        &memory,
                        &auxv,
                        kvm_registers(registers, request.number()),
                        &mut thread_state,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                        Some(raw),
                    )
                    .await?;
                    (raw, false, false, false)
                };
                let mut returned_registers = process_syscall_return_registers(
                    &memory,
                    registers,
                    frame_address,
                    result,
                    None,
                )?;
                let mut restarted_registers = restart_requested
                    .then(|| restart_syscall_registers(returned_registers, request.number()))
                    .transpose()?;
                // The ring0 trampoline reads the result from the frame and then
                // SYSRETs, so the hypercall return slot is unused here.
                SyscallRequest::write_result(&mut memory, frame_address, result)?;
                let pending_segment = executor.take_segment();
                let mut pending_exit = executor.take_exit();
                let pending_process = executor.take_process_action();

                if let Some((segment, address)) = pending_segment {
                    set_user_segment_base(&self.vcpu, segment, address)?;
                }
                let mut replaced_image = handler_replaced_image;
                if let Some(action) = pending_process {
                    let continuation = CompletedSyscallBoundary::capture_for_action(
                        self,
                        frame_address,
                        None,
                        &action,
                    )?;
                    let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                    let context: ToolContext<'_, T> = ToolContext {
                        pid,
                        tid,
                        process_state: tool.clone(),
                        thread_state: &thread_state,
                        global_state: Some(global_state.clone()),
                        config: config.clone(),
                        subscriptions: subscriptions.clone(),
                        pending_child_starts: pending_child_starts.clone(),
                    };
                    let outcome = self
                        .run_process_action_with_tool_at_boundary(
                            executor,
                            action,
                            context,
                            continuation,
                        )
                        .await;
                    let outcome = outcome?;
                    if !outcome.image_replaced {
                        result = outcome.syscall_result;
                        returned_registers = process_syscall_return_registers(
                            &memory,
                            registers,
                            frame_address,
                            result,
                            None,
                        )?;
                        restarted_registers = restart_requested
                            .then(|| {
                                restart_syscall_registers(returned_registers, request.number())
                            })
                            .transpose()?;
                    }
                    replaced_image |= outcome.image_replaced;
                    self.start_pending_tool_children(executor, &pending_child_starts)?;
                }
                if replaced_image {
                    auxv = executor.auxv().to_vec();
                    let post_exec_outcome = run_post_exec_handler(
                        self,
                        &tool,
                        pid,
                        &memory,
                        &mut auxv,
                        &mut thread_state,
                        executor,
                        global_state.clone(),
                        config,
                        subscriptions,
                        &stack_checked_out,
                    )
                    .await;
                    let post_exec_error = match post_exec_outcome {
                        Ok(CallbackOutcome::ThreadCancelled) => {
                            return Ok(self.cancelled_tool_thread_status(executor));
                        }
                        Ok(CallbackOutcome::Completed(())) => None,
                        Err(error) => Some(error),
                    };
                    if let Some(error) = post_exec_error {
                        return Err(error);
                    }
                }
                if let Some((segment, address)) = executor.take_segment() {
                    set_user_segment_base(&self.vcpu, segment, address)?;
                }
                if !replaced_image && pending_exit.is_none() {
                    let pending = if let Some(prepared) = executor
                        .take_prepared_signal()
                        .map_err(|errno| Error::Reverie(errno.into()))?
                    {
                        CallbackOutcome::Completed(Some(prepared))
                    } else {
                        self.filter_one_pending_signal_with_tool(
                            executor,
                            pid,
                            tid,
                            &tool,
                            &memory,
                            &auxv,
                            returned_registers,
                            request.number(),
                            frame_address,
                            &mut thread_state,
                            &global_state,
                            config,
                            subscriptions,
                            &stack_checked_out,
                            false,
                        )
                        .await?
                    };
                    let pending = match pending {
                        CallbackOutcome::Completed(pending) => pending,
                        CallbackOutcome::ThreadCancelled => {
                            return Ok(self.cancelled_tool_thread_status(executor));
                        }
                    };
                    if let Some((segment, address)) = executor.take_segment() {
                        set_user_segment_base(&self.vcpu, segment, address)?;
                    }
                    pending_exit = pending_exit.or_else(|| executor.take_exit());
                    let mut delivered = false;
                    if pending_exit.is_none()
                        && let Some(pending) = pending
                    {
                        let signal_registers = if restart_requested
                            && executor.caught_signal_restarts_syscall(pending)
                        {
                            restarted_registers.expect("restart registers were constructed")
                        } else {
                            returned_registers
                        };
                        delivered = self.deliver_selected_signal_from_registers(
                            executor,
                            frame_address,
                            signal_registers,
                            pending,
                        )?;
                    }
                    pending_exit = pending_exit.or_else(|| executor.take_exit());
                    if restart_requested && !delivered && pending_exit.is_none() {
                        // Suppression, an ignored replacement, or a replacement
                        // newly blocked by its signal number means no handler ran;
                        // resume by re-executing the original syscall rather than
                        // leaking EINTR or the kernel-private ERESTARTSYS value.
                        stage_process_syscall_return(
                            &mut memory,
                            &self.vcpu,
                            frame_address,
                            restarted_registers.expect("restart registers were constructed"),
                        )?;
                    }
                }
                executor.finish_parked_delivery();
                pending_exit = pending_exit.or_else(|| executor.take_exit());
                if let Some(exit) = pending_exit {
                    if exit.group {
                        self.request_guest_thread_group_exit(exit.status);
                    }
                    return Ok(exit.into());
                }
            }
        }
        .await;
        let outcome = outcome.map_err(|error| {
            self.cleanup_unstarted_tool_children_after_error(executor, &pending_child_starts, error)
        });
        self.finish_tool_process(
            executor,
            tool,
            (pid, tid),
            global_state.as_ref(),
            config,
            thread_state,
            outcome,
            true,
        )
        .await
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 4202..4252; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
        fn read_clock(&self) -> Result<u64> {
            Err(Error::GuestClock(
                "side-effect test has no guest counter".into(),
            ))
        }

        fn execute(&mut self, request: &SyscallRequest, _memory: &GuestMemory) -> i64 {
            match request.number() as libc::c_long {
                libc::SYS_write => self.state.output_bytes += request.args()[2] as usize,
                libc::SYS_pipe2 => self.state.descriptors += 2,
                libc::SYS_fork | libc::SYS_clone => {
                    self.state.tasks += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_execve | libc::SYS_execveat => {
                    self.state.address_space_generation += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_mmap => self.state.address_space_generation += 1,
                libc::SYS_kill | libc::SYS_tkill | libc::SYS_tgkill => {
                    self.state.pending_signal = true
                }
                libc::SYS_exit | libc::SYS_exit_group => self.state.exited = true,
                number => panic!("unexpected side-effect probe syscall {number}"),
            }
            0
        }

        fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
            !injection_can_be_nonreturning(request)
        }

        fn complete_injection<'a>(
            &'a mut self,
            _context: ToolContext<'a, crate::StraceTool>,
        ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
        where
            crate::StraceTool: 'a,
        {
            self.state.completion_callbacks += 1;
            Box::pin(async {
                Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: true,
                    process_exited: true,
                })
            })
        }

        fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
            false
        }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/runtime.rs lines 4261..4295; full file SHA256 3d061e02a0fa5bd023f194f1aa6e1c9ee20821ed11e25ebbdc44c56e678b62c2
        fn read_clock(&self) -> Result<u64> {
            Err(Error::GuestClock(
                "side-effect test has no guest counter".into(),
            ))
        }

        fn execute(&mut self, request: &SyscallRequest, _memory: &GuestMemory) -> i64 {
            match request.number() as libc::c_long {
                libc::SYS_execve | libc::SYS_execveat => {
                    self.state.address_space_generation += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_kill | libc::SYS_tkill | libc::SYS_tgkill => {
                    self.state.pending_signal = true
                }
                libc::SYS_exit | libc::SYS_exit_group => self.state.exited = true,
                number => panic!("unexpected nonreturning probe syscall {number}"),
            }
            0
        }

        fn complete_injection<'a>(
            &'a mut self,
            _context: ToolContext<'a, crate::StraceTool>,
        ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
        where
            crate::StraceTool: 'a,
        {
            self.state.completion_callbacks += 1;
            Box::pin(async {
                Ok(InjectionCompletion::Returns {
                    syscall_result: None,
                })
            })
        }

