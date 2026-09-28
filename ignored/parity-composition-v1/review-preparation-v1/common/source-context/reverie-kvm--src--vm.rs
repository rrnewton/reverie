// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 61..85; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
use crate::bootstrap::exception_pushes_error_code;
use crate::bootstrap::process_syscall_return_registers;
use crate::bootstrap::set_syscall_return_park;
use crate::bootstrap::set_user_segment_base;
use crate::bootstrap::stage_process_syscall_return;
use crate::bootstrap::syscall_hypercall_address;
use crate::bootstrap::thread_tool_stack_top;
use crate::elf::LoadedStaticElf;
use crate::elf::TaskLifecycleTable;
use crate::elf::initial_thread_name;
use crate::elf::load_static_elf;
use crate::elf::load_static_elf_file;
use crate::executor::CapturedOutput;
use crate::executor::ChildCompletion;
use crate::executor::ChildStartCommand;
use crate::executor::ChildStartGate;
use crate::executor::ElfExecutor;
use crate::executor::ProcessAction;
use crate::executor::ProcessExit;
use crate::executor::SignalDisposition;
use crate::executor::conventional_exit_code;
use crate::runtime::PendingChildCancellation;
use crate::runtime::PendingChildKind;
use crate::runtime::PendingChildStart;
use crate::runtime::SharedChildStarts;

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 111..165; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
const HLT: u8 = 0xf4;
const VMWARE_BACKDOOR_MAGIC: u64 = 0x564d_5868;
const VMWARE_BACKDOOR_PORT: u64 = 0x5658;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StaticElfException {
    vector: u8,
    instruction_pointer: u64,
    stack_pointer: u64,
    rflags: u64,
}

pub(crate) struct InstructionBoundary<I> {
    pub(crate) registers: kvm_regs,
    pub(crate) instruction: I,
    special_registers: kvm_bindings::kvm_sregs,
    code_segment: u16,
    stack_segment: u16,
}

pub(crate) type TimestampBoundary = InstructionBoundary<crate::timestamp::TimestampInstruction>;
pub(crate) type CpuidBoundary = InstructionBoundary<crate::cpuid_instruction::Instruction>;

pub(crate) enum ToolInstructionBoundary {
    Timestamp(TimestampBoundary),
    Cpuid(CpuidBoundary),
}

pub(crate) enum ToolInstructionResult {
    Timestamp(reverie::RdtscResult),
    Cpuid(reverie::CpuIdResult),
}

impl ToolInstructionBoundary {
    pub(crate) fn user_registers(&self) -> libc::user_regs_struct {
        match self {
            Self::Timestamp(boundary) => boundary.user_registers(),
            Self::Cpuid(boundary) => boundary.user_registers(),
        }
    }
}

impl<I> InstructionBoundary<I> {
    pub(crate) fn user_registers(&self) -> libc::user_regs_struct {
        let mut registers = crate::runtime::kvm_registers(self.registers, u64::MAX);
        registers.cs = self.code_segment.into();
        registers.ss = self.stack_segment.into();
        registers.ds = self.special_registers.ds.selector.into();
        registers.es = self.special_registers.es.selector.into();
        registers.fs = self.special_registers.fs.selector.into();
        registers.gs = self.special_registers.gs.selector.into();
        registers.fs_base = self.special_registers.fs.base;
        registers.gs_base = self.special_registers.gs.base;
        registers
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 982..1006; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    // ownership; when running a tool it is resolved from `thread_ownership_override`
    // (if set) else the tool's `Tool::thread_ownership` at run entry.
    pub(crate) thread_ownership: ThreadOwnership,
    // Explicit caller override for `thread_ownership`. `None` means "follow the
    // tool" — resolve from `Tool::thread_ownership` at run entry (the safe,
    // Tool-owned "follow children" default). `Some(_)` forces that ownership
    // regardless of the tool (set via `set_thread_ownership` /
    // `unmonitored_threads`), and survives run-entry resolution.
    thread_ownership_override: Option<ThreadOwnership>,
    // Set by the active execution consumer, never copied to a new vCPU. A
    // Host-owned worker has no Tool dispatcher and must retain native TSC.
    intercept_rdtsc: bool,
    cpuid_interception: crate::cpuid_instruction::Interception,
    pub(crate) static_elf: Option<LoadedStaticElf>,
    stdin: Option<File>,
    pub(crate) root_pid: i32,
    // One optional collector is shared by every fork and thread backend in the
    // guest tree. `None` is the allocation-free, update-free default.
    pub(crate) exit_collector: Option<Arc<KvmExitCollector>>,
}

struct KvmProcessSnapshot {
    memory: GuestMemory,
    registers: kvm_regs,
    xsave: kvm_xsave,

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 1130..1181; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    fn new_with_memory_and_cpuid_policy(
        memory: GuestMemory,
        cpuid_policy: CpuidPolicy,
        stdin: Option<File>,
    ) -> Result<Self> {
        install_worker_interrupt_handler()?;
        const INITIALIZATION_ATTEMPTS: usize = 3;
        let mut attempts = 0;
        let InitializedKvmResources {
            vcpu,
            vm,
            kvm,
            hypercall_instruction,
        } = loop {
            attempts += 1;
            match Self::initialize_resources(&memory, cpuid_policy) {
                Ok(resources) => break resources,
                Err(Error::Kvm(error))
                    if error.errno() == libc::EINTR && attempts < INITIALIZATION_ATTEMPTS => {}
                Err(error) => return Err(error),
            }
        };
        Ok(Self {
            vcpu: crate::clock::CountedVcpu::new(vcpu),
            vm,
            memory,
            _kvm: kvm,
            cpuid_policy,
            hypercall_instruction,
            syscall_trampoline_address: SYSCALL_TRAMPOLINE_ADDRESS,
            syscall_frame_address: SYSCALL_FRAME_ADDRESS,
            thread_group: Arc::new(GuestThreadGroup::default()),
            tool_failure: None,
            thread_slot: None,
            is_guest_thread: false,
            // Effective ownership before any tool run resolves it. The direct
            // (non-tool) personality never dispatches threads through a Tool loop,
            // so Host is the correct effective value there; when a tool runs,
            // `run_static_elf_with_tool` resolves this from the override or the
            // tool's `Tool::thread_ownership` (the Tool-owned "follow children"
            // default).
            thread_ownership: ThreadOwnership::Host,
            // No explicit caller override: follow the tool at run entry.
            thread_ownership_override: None,
            intercept_rdtsc: false,
            cpuid_interception: crate::cpuid_instruction::Interception::default(),
            static_elf: None,
            stdin,
            root_pid: 1,
            exit_collector: None,
        })
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 1478..1591; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    fn snapshot_process(&self) -> Result<KvmProcessSnapshot> {
        let memory = self.memory.snapshot()?;
        hide_tool_scratch_pages(&memory)?;
        Ok(KvmProcessSnapshot {
            memory,
            registers: self.vcpu.get_regs()?,
            xsave: self.vcpu.get_xsave()?,
            stdin: self.stdin.as_ref().map(File::try_clone).transpose()?,
            cpuid_policy: self.cpuid_policy,
        })
    }

    fn from_process_snapshot(snapshot: KvmProcessSnapshot) -> Result<Self> {
        let mut child = Self::new_with_memory_and_cpuid_policy(
            snapshot.memory,
            snapshot.cpuid_policy,
            snapshot.stdin,
        )?;
        configure_long_mode(
            &mut child.memory,
            &child.vcpu,
            0,
            snapshot.registers.rsp,
            child.hypercall_instruction,
        )?;
        child.vcpu.set_regs(&snapshot.registers)?;
        // SAFETY: this guest setup does not enable dynamically sized XSTATE features.
        unsafe { child.vcpu.set_xsave(&snapshot.xsave)? };
        Ok(child)
    }

    // TODO-HUMAN-REVIEW(PR-172): Review independent vCPU creation from clone3 state.
    fn from_thread_state(
        memory: GuestMemory,
        registers: kvm_regs,
        xsave: kvm_xsave,
        stdin: Option<File>,
        cpuid_policy: CpuidPolicy,
        child_tid: i32,
        thread_group: Arc<GuestThreadGroup>,
    ) -> Result<Self> {
        let mut child = Self::new_with_memory_and_cpuid_policy(memory, cpuid_policy, stdin)?;
        child.thread_group = thread_group;
        child.is_guest_thread = true;
        let slot = child.thread_group.reserve_transport_slot(child_tid)?;
        child.thread_slot = Some(slot);
        let syscall_trampoline_address =
            THREAD_SYSCALL_AREA_START + slot as u64 * THREAD_SYSCALL_AREA_STRIDE;
        let syscall_frame_address = syscall_trampoline_address + PAGE_SIZE;
        child.syscall_trampoline_address = syscall_trampoline_address;
        child.syscall_frame_address = syscall_frame_address;
        configure_long_mode_with_syscall_area(
            &mut child.memory,
            &child.vcpu,
            0,
            registers.rsp,
            child.hypercall_instruction,
            syscall_trampoline_address,
            syscall_frame_address,
            false,
        )?;
        child.vcpu.set_regs(&registers)?;
        // SAFETY: this guest setup does not enable dynamically sized XSTATE features.
        unsafe { child.vcpu.set_xsave(&xsave)? };
        Ok(child)
    }

    // TODO-HUMAN-REVIEW(PR-156): Review lifecycle-hook exec image replacement API.
    pub(crate) fn exec_process(
        &mut self,
        executor: &mut ElfExecutor,
        executable: (&Path, Option<Arc<File>>),
        image: &[u8],
        argv: &[String],
        envp: &[String],
    ) -> Result<()> {
        if self.is_guest_thread {
            return Err(Error::GuestThreadExecUnsupported);
        }
        // ElfExecutor preflights the image before scheduling this action. From
        // this reset onward, any exceptional loader or KVM failure is fatal to
        // the backend and is never reported back to the old guest image. This
        // is the same point-of-no-return behavior as the prior zero_raw reset.
        let user_length = usize::try_from(self.memory.guest_end() - BOOT_RESERVED_END)
            .expect("guest memory length must fit usize");
        self.memory.discard_pages(BOOT_RESERVED_END, user_length)?;

        let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let envp = envp.iter().map(String::as_str).collect::<Vec<_>>();
        let mut loaded = load_static_elf(&mut self.memory, image, &argv, &envp, executor.cwd())?;
        let (executable_path, executable_file) = executable;
        loaded.executable_path = executable_file
            .as_ref()
            .and_then(|file| std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok())
            .unwrap_or_else(|| executable_path.to_owned());
        loaded.executable_file = executable_file;
        let thread_name = initial_thread_name(executable_path);
        loaded.thread_name = thread_name;
        *loaded
            .thread_group_leader_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = thread_name;
        loaded.stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
        configure_long_mode(
            &mut self.memory,
            &self.vcpu,
            loaded.entry_point,
            loaded.stack_pointer,
            self.hypercall_instruction,
        )?;
        self.memory.enable_user_access();
        executor.replace_after_exec(loaded);
        Ok(())
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 2042..2057; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    fn finish_tool_process_action_at_boundary(
        &mut self,
        executor: &mut ElfExecutor,
        starts: &SharedChildStarts,
        continuation: ProcessActionContinuation,
        action_result: Result<ProcessActionOutcome>,
    ) -> Result<ProcessActionOutcome> {
        match continuation.finish(self, action_result) {
            Ok(outcome) => Ok(outcome),
            Err(action_error) => Err(self.cleanup_unstarted_tool_children_after_error(
                executor,
                starts,
                action_error,
            )),
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 2112..2579; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    // TODO-HUMAN-REVIEW(PR-192): Review tool lifecycle for KVM fork children.
    // TODO-HUMAN-REVIEW(PR-235): Review concurrent fork-child Tool execution.
    async fn run_process_action_with_tool_inner<T>(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        park_syscall_return: bool,
        context: ToolContext<'_, T>,
        fault: Option<&PageZeroFault>,
    ) -> Result<ProcessActionOutcome>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        match action {
            ProcessAction::Fork {
                child_pid,
                child_stack,
                parent_tid,
                child_tid,
                clear_child_tid,
                clear_sighand,
                share_address_space,
            } => {
                let child = self.prepare_forked_process(
                    executor,
                    child_pid,
                    child_stack,
                    parent_tid,
                    child_tid,
                    clear_child_tid,
                    clear_sighand,
                    share_address_space,
                    park_syscall_return,
                    fault,
                )?;

                let child_pid = Pid::from_raw(child.pid);
                let global_state = context.global_state.ok_or_else(|| {
                    Error::UnexpectedVcpuExit(
                        "forked KVM Tool process requires shared global state".to_owned(),
                    )
                })?;
                let child_tool = Arc::new(T::new(child_pid, &context.config));
                let child_thread_state = child_tool
                    .init_thread_state(child_pid, Some((context.tid, context.thread_state)));
                let config = context.config;
                let subscriptions = context.subscriptions;
                let pending_child_starts = context.pending_child_starts;
                let raw_child_pid = child.pid;
                let parent_pid = context.pid;
                let lifecycle_state = global_state.clone();
                let auto_reap = executor.child_exit_policy();
                let completion_notifier = executor.child_completion_notifier();
                let completion = Arc::new(Mutex::new(None));
                let child_completion = completion.clone();
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                let handle = crate::failure::spawn_owned(
                    std::thread::Builder::new()
                        .name(format!("reverie-kvm-process-{raw_child_pid}")),
                    (
                        child,
                        child_tool,
                        child_thread_state,
                        global_state,
                        config,
                        subscriptions,
                    ),
                    move |(
                        mut child,
                        child_tool,
                        child_thread_state,
                        global_state,
                        config,
                        subscriptions,
                    )| {
                        let start = start_receiver.recv();
                        let cancel = !matches!(start, Ok(ChildStartCommand::Start));
                        let failure = match start {
                            Ok(command) => command.failure(),
                            Err(_) => Some(Error::UnexpectedVcpuExit(format!(
                                "KVM child process {raw_child_pid} lost its parent start gate"
                            ))),
                        };
                        let result = if cancel {
                            futures::executor::block_on(child.backend.finish_unstarted_tool(
                                &mut child.executor,
                                child_tool,
                                (child_pid, child_pid),
                                global_state.as_ref(),
                                &config,
                                child_thread_state,
                                failure,
                            ))
                        } else {
                            futures::executor::block_on(
                                child.backend.run_static_elf_process_with_tool(
                                    &mut child.executor,
                                    child_pid,
                                    // A forked process child is its own leader (tid == pid).
                                    child_pid,
                                    child_tool,
                                    child_thread_state,
                                    global_state,
                                    &config,
                                    &subscriptions,
                                    false,
                                ),
                            )
                        };
                        match result {
                            Ok((status, _, _)) => {
                                write_tid_best_effort(
                                    &mut child.backend.memory,
                                    child.executor.take_clear_child_tid(),
                                    0,
                                );
                                let waitable = !auto_reap.load(Ordering::SeqCst);
                                let completion =
                                    ChildCompletion::from_waitability(status, waitable);
                                *child_completion
                                    .lock()
                                    .expect("KVM child completion lock poisoned") =
                                    Some(completion);
                                let _ = completion_notifier.send(raw_child_pid);
                                futures::executor::block_on(
                                    lifecycle_state.on_backend_child_wait_event(
                                        BackendChildWaitEvent {
                                            parent: parent_pid,
                                            child: child_pid,
                                            state: BackendChildWaitState::Exited {
                                                status,
                                                waitable,
                                            },
                                        },
                                    ),
                                )
                                .map_err(|error| {
                                    child.backend.report_tool_failure(
                                        "child wait hook",
                                        Error::Reverie(error),
                                    )
                                })?;
                                Ok(())
                            }
                            Err(error) => {
                                *child_completion
                                    .lock()
                                    .expect("KVM child completion lock poisoned") =
                                    Some(ChildCompletion::Failed);
                                let _ = completion_notifier.send(raw_child_pid);
                                Err(error)
                            }
                        }
                    },
                );
                let handle = match handle {
                    Ok(handle) => handle,
                    Err((
                        error,
                        (mut child, child_tool, child_thread_state, global_state, config, _),
                    )) => {
                        return child
                            .backend
                            .finish_unstarted_tool(
                                &mut child.executor,
                                child_tool,
                                (child_pid, child_pid),
                                global_state.as_ref(),
                                &config,
                                child_thread_state,
                                Some(Error::HostIo(error)),
                            )
                            .await
                            .map(|_| unreachable!("failed spawn completed successfully"));
                    }
                };
                pending_child_starts
                    .lock()
                    .expect("KVM child-start lock poisoned")
                    .push(PendingChildStart::fork_process(
                        raw_child_pid,
                        start_gate.clone(),
                    ));
                executor.register_child_process_with_gate(
                    raw_child_pid,
                    start_gate,
                    completion,
                    handle,
                );
                configure_process_syscall_return(
                    &self.memory,
                    &self.vcpu,
                    self.syscall_frame_address,
                    i64::from(raw_child_pid),
                    None,
                )
                .map(|()| ProcessActionOutcome::returned(i64::from(raw_child_pid)))
            }
            // `ThreadOwnership::Host`: CLONE_THREAD workers run uninstrumented on
            // the direct backend personality with host-backed synchronization,
            // exactly as `run_process_action`'s Thread branch does. Execution and
            // `futex` ownership are both Host (a single `ThreadOwnership`), so the
            // joiner's real host `FUTEX_WAIT` and the exiting worker's real host
            // `CLONE_CHILD_CLEARTID` wake meet on the same futex word — no
            // deadlock. (The split-brain that deadlocks a join — a Tool-executed
            // worker whose `futex` is host-owned, so its logical CLEARTID wake
            // never reaches the host waiter — is unrepresentable now that one
            // enum drives both decisions.)
            ProcessAction::Thread { .. } if self.thread_ownership.executes_on_host() => {
                self.run_process_action_inner(executor, action, park_syscall_return, fault)
            }
            // `ThreadOwnership::Tool`: a CLONE_THREAD worker runs its own vCPU on
            // a fresh OS thread but shares the guest address space, file table,
            // and process Tool identity with its creator. It is driven through
            // the same Tool loop as the process leader so Detcore sees its
            // syscalls, shares its fd model, and schedules it; `futex` routes to
            // Detcore (runtime.rs) so a join's logical `FUTEX_WAIT` is woken by
            // the worker's logical `CLEARTID`. This mirrors the physical setup in
            // `run_process_action`'s Thread branch, but spawns the Tool loop
            // instead of the direct backend personality.
            ProcessAction::Thread {
                child_tid,
                child_stack,
                parent_tid,
                child_tid_address,
                clear_child_tid,
                tls,
            } => {
                let parent_registers = self.vcpu.get_regs()?;
                let parent_xsave = self.vcpu.get_xsave()?;
                let (parent_fs, parent_gs) = executor.segment_bases();
                let mut parent_syscall_frame = vec![0; FRAME_SIZE];
                self.memory
                    .read_raw(self.syscall_frame_address, &mut parent_syscall_frame)?;

                if park_syscall_return {
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        true,
                    )?;
                    let vcpu_exit = self.vcpu.run()?;
                    Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                    let parked = match vcpu_exit {
                        VcpuExit::Hlt => Ok(()),
                        exit => Err(Error::UnexpectedVcpuExit(format!(
                            "parent did not park at thread clone: {exit:?}"
                        ))),
                    };
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        false,
                    )?;
                    parked?;
                }
                let child_registers = self.vcpu.get_regs()?;

                write_tid_best_effort(&mut self.memory, parent_tid, child_tid);
                write_tid_best_effort(&mut self.memory, child_tid_address, child_tid);
                let child_fs = tls.unwrap_or(parent_fs);
                let mut child_executor = executor.thread_child(child_tid)?;
                child_executor.set_thread_context(child_tid, child_fs, parent_gs);
                child_executor.set_clear_child_tid(clear_child_tid);
                let child_stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
                let mut child = Self::from_thread_state(
                    self.memory.clone(),
                    child_registers,
                    parent_xsave,
                    child_stdin,
                    self.cpuid_policy,
                    child_tid,
                    self.thread_group.clone(),
                )?;
                // Thread children inherit the parent's thread ownership so
                // execution and futex classification stay consistent.
                self.debug_assert_thread_ownership_consistent();
                child.thread_ownership = self.thread_ownership;
                child.tool_failure = self.tool_failure.clone();
                child.exit_collector = self.exit_collector.clone();
                child
                    .memory
                    .write_raw(child.syscall_frame_address, &parent_syscall_frame)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Fs, child_fs)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Gs, parent_gs)?;
                configure_process_syscall_return(
                    &child.memory,
                    &child.vcpu,
                    child.syscall_frame_address,
                    0,
                    Some(child_stack),
                )?;

                if let Some(fault) = fault {
                    fault.configure_child(&mut child, Some(child_stack))?;
                }

                // CLONE_THREAD shares the process (thread-group) identity, so the
                // worker shares its creator's process Tool state, while the
                // thread state is keyed on the new child tid. This
                // mirrors reverie-ptrace's `cloned()`, where the child shares the
                // process Tool identity and receives fresh per-thread state
                // linked to the parent thread.
                let tgid = context.pid;
                let child_tid_pid = Pid::from_raw(child_tid);
                let global_state = context.global_state.ok_or_else(|| {
                    Error::UnexpectedVcpuExit(
                        "KVM CLONE_THREAD worker requires shared global state".to_owned(),
                    )
                })?;
                let config = context.config;
                let subscriptions = context.subscriptions;

                self.vcpu.set_regs(&parent_registers)?;
                configure_process_syscall_return(
                    &self.memory,
                    &self.vcpu,
                    self.syscall_frame_address,
                    i64::from(child_tid),
                    None,
                )?;

                let child_tool = context.process_state.clone();
                let child_thread_state = child_tool
                    .init_thread_state(child_tid_pid, Some((context.tid, context.thread_state)));
                if let Some(failure) = &child.tool_failure {
                    child.tool_failure = Some(failure.for_thread(child_tid_pid));
                }
                let pending_child_starts = context.pending_child_starts;
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                let handle = crate::failure::spawn_owned(
                    std::thread::Builder::new().name(format!("reverie-kvm-guest-{child_tid}")),
                    (
                        child,
                        child_executor,
                        child_tool,
                        child_thread_state,
                        global_state,
                        config,
                        subscriptions,
                    ),
                    move |(
                        mut child,
                        mut child_executor,
                        child_tool,
                        child_thread_state,
                        global_state,
                        config,
                        subscriptions,
                    )| {
                        let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                            || {
                                let start = start_receiver.recv();
                                let cancel = !matches!(start, Ok(ChildStartCommand::Start));
                                let failure = match start {
                                    Ok(command) => command.failure(),
                                    Err(_) => Some(Error::UnexpectedVcpuExit(format!(
                                        "KVM guest thread {child_tid} lost its parent start gate"
                                    ))),
                                };
                                let result = if cancel {
                                    futures::executor::block_on(child.finish_unstarted_tool(
                                        &mut child_executor,
                                        child_tool,
                                        (tgid, child_tid_pid),
                                        global_state.as_ref(),
                                        &config,
                                        child_thread_state,
                                        failure,
                                    ))
                                } else {
                                    futures::executor::block_on(
                                        child.run_static_elf_process_with_tool(
                                            &mut child_executor,
                                            tgid,
                                            child_tid_pid,
                                            child_tool,
                                            child_thread_state,
                                            global_state,
                                            &config,
                                            &subscriptions,
                                            false,
                                        ),
                                    )
                                };
                                child.release_thread_slot();
                                clear_tid_and_wake(
                                    &mut child.memory,
                                    child_executor.take_clear_child_tid(),
                                );
                                let peer_cancelled = crate::runtime::is_peer_cancelled_tool_worker(
                                    (tgid, child_tid_pid),
                                    !cancel,
                                    &result,
                                );
                                if result.is_err() && !peer_cancelled {
                                    child_executor.retire_failed_thread();
                                    child.thread_group.record_worker_failure(child_tid);
                                }
                                if let Err(error) = &result
                                    && !peer_cancelled
                                    && child.thread_group.take_worker_error_report(child_tid)
                                {
                                    eprintln!(
                                        "reverie-kvm guest thread {child_tid} tool loop failed: {error}"
                                    );
                                }
                                result
                            },
                        ));
                        match execution {
                            Ok(result) => result,
                            Err(payload) => child.finish_panicked_guest_worker(
                                &mut child_executor,
                                child_tid,
                                payload,
                            ),
                        }
                    },
                );
                let handle = match handle {
                    Ok(handle) => handle,
                    Err((
                        error,
                        (
                            mut child,
                            mut child_executor,
                            child_tool,
                            child_thread_state,
                            global_state,
                            config,
                            _,
                        ),
                    )) => {
                        return child
                            .finish_unstarted_tool(
                                &mut child_executor,
                                child_tool,
                                (tgid, child_tid_pid),
                                global_state.as_ref(),
                                &config,
                                child_thread_state,
                                Some(Error::HostIo(error)),
                            )
                            .await
                            .map(|_| unreachable!("failed spawn completed successfully"));
                    }
                };
                self.thread_group
                    .add_unstarted_worker(child_tid, start_gate.clone(), handle);
                pending_child_starts
                    .lock()
                    .expect("KVM child-start lock poisoned")
                    .push(PendingChildStart::tool_thread(child_tid, start_gate));
                Ok(ProcessActionOutcome::returned(i64::from(child_tid)))
            }
            other => self.run_process_action_inner(executor, other, park_syscall_return, fault),
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 2659..2759; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
        }))
    }

    pub(crate) fn set_rdtsc_interception(&mut self, enabled: bool) -> Result<()> {
        crate::bootstrap::set_userspace_rdtsc_interception(&self.vcpu, enabled)?;
        self.intercept_rdtsc = enabled;
        Ok(())
    }

    pub(crate) fn set_cpuid_interception(&mut self, enabled: bool) -> Result<()> {
        self.cpuid_interception.configure(&self.vcpu, enabled)
    }

    pub(crate) fn has_cpuid_interception(&self) -> bool {
        self.cpuid_interception.enabled()
    }

    pub(crate) fn tool_instruction_exception(&self) -> Result<Option<ToolInstructionBoundary>> {
        if let Some(boundary) = self.cpuid_instruction_exception()? {
            return Ok(Some(ToolInstructionBoundary::Cpuid(boundary)));
        }
        Ok(self
            .timestamp_counter_exception()?
            .map(ToolInstructionBoundary::Timestamp))
    }

    fn cpuid_instruction_exception(&self) -> Result<Option<CpuidBoundary>> {
        if !self.cpuid_interception.enabled() {
            return Ok(None);
        }
        let Some(exception) = self.static_elf_exception()? else {
            return Ok(None);
        };
        // CPUID is never an emulated #UD. LOCK and unrelated faults retain
        // their genuine exception, even when this Tool subscribes to CPUID.
        if exception.vector != 13 {
            return Ok(None);
        }
        self.cpuid_interception.verify(&self.vcpu)?;
        let halted = self.vcpu.get_regs()?;
        let special = self.vcpu.get_sregs()?;
        if special.cr0 & (1 << 31) == 0
            || special.efer & ((1 << 10) | (1 << 11)) != ((1 << 10) | (1 << 11))
            || special.cr4 & (1 << 12) != 0
        {
            return Ok(None);
        }
        let mut frame = [0; 6 * 8];
        self.memory.read_raw(halted.rsp, &mut frame)?;
        let word = |index: usize| {
            u64::from_le_bytes(
                frame[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("exception frame word"),
            )
        };
        let cs = word(2);
        let ss = word(5);
        if word(0) != 0
            || cs != u64::from(crate::signal::USER_CODE_SELECTOR)
            || ![
                u64::from(crate::signal::USER_DATA_SELECTOR),
                u64::from(crate::signal::USER_DATA_SELECTOR & !3),
            ]
            .contains(&ss)
        {
            return Ok(None);
        }
        let Some(instruction) = crate::cpuid_instruction::decode(|offset| {
            let address = exception
                .instruction_pointer
                .checked_add(u64::from(offset))?;
            crate::timestamp::fetch_user_byte(&self.memory, special.cr3, address)
        }) else {
            return Ok(None);
        };
        let mut registers = halted;
        registers.rip = exception.instruction_pointer;
        registers.rsp = exception.stack_pointer;
        registers.rflags = exception.rflags;
        Ok(Some(CpuidBoundary {
            registers,
            instruction,
            special_registers: special,
            code_segment: cs as u16,
            stack_segment: ss as u16,
        }))
    }

    pub(crate) fn timestamp_counter_exception(&self) -> Result<Option<TimestampBoundary>> {
        // This guard precedes even reading the fault frame: an unsubscribed
        // RDTSCP #UD is a genuine guest fault, not an unsolicited callback.
        if !self.intercept_rdtsc {
            return Ok(None);
        }
        let Some(exception) = self.static_elf_exception()? else {
            return Ok(None);
        };
        if !matches!(exception.vector, 6 | 13) {
            return Ok(None);
        }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 2816..2884; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    }

    pub(crate) fn resume_timestamp_counter(
        &mut self,
        boundary: TimestampBoundary,
        result: reverie::RdtscResult,
    ) -> Result<()> {
        let registers =
            crate::timestamp::result_registers(boundary.registers, boundary.instruction, result)
                .ok_or_else(|| Error::UnexpectedVcpuExit("timestamp RIP overflow".to_owned()))?;
        self.resume_instruction_registers(boundary, registers)
    }

    pub(crate) fn resume_tool_instruction(
        &mut self,
        boundary: ToolInstructionBoundary,
        result: ToolInstructionResult,
    ) -> Result<()> {
        match (boundary, result) {
            (
                ToolInstructionBoundary::Timestamp(boundary),
                ToolInstructionResult::Timestamp(result),
            ) => self.resume_timestamp_counter(boundary, result),
            (ToolInstructionBoundary::Cpuid(boundary), ToolInstructionResult::Cpuid(result)) => {
                let registers = crate::cpuid_instruction::result_registers(
                    boundary.registers,
                    boundary.instruction,
                    result,
                )
                .ok_or_else(|| Error::UnexpectedVcpuExit("CPUID RIP overflow".to_owned()))?;
                self.resume_instruction_registers(boundary, registers)
            }
            _ => Err(Error::UnexpectedVcpuExit(
                "instruction callback result kind changed".to_owned(),
            )),
        }
    }

    fn resume_instruction_registers<I>(
        &mut self,
        boundary: InstructionBoundary<I>,
        registers: kvm_regs,
    ) -> Result<()> {
        // Exception stubs preserve every GPR. Returning host-side injections
        // cannot supply a replacement user register file. Preserve the saved
        // instruction state, and any intentional injected FS/GS-base effect.
        let previous = self.vcpu.get_sregs()?;
        configure_user_segments(&self.vcpu)?;
        let mut special = self.vcpu.get_sregs()?;
        special.cs.selector = boundary.code_segment;
        special.ss.selector = boundary.stack_segment;
        special.ds = previous.ds;
        special.es = previous.es;
        special.fs = previous.fs;
        special.gs = previous.gs;
        self.vcpu.set_sregs(&special)?;
        self.vcpu.set_regs(&registers)?;
        if registers.rflags & (1 << 8) != 0 {
            // The instruction has now retired. Preserve this backend's existing
            // typed #DB boundary here, before another guest instruction can
            // run; resuming with TF would report the following instruction.
            return Err(Error::GuestException {
                vector: 1,
                instruction_pointer: registers.rip,
                fault_address: special.cr2,
            });
        }
        Ok(())
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 3317..3557; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    /// Runs the installed static ELF and its forked children until the root exits.
    pub fn run_static_elf(&mut self) -> Result<i32> {
        let loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        let mut executor = ElfExecutor::with_output(loaded, None);
        let (status, _, _) = self.run_static_elf_process(&mut executor)?;
        Ok(conventional_exit_code(status))
    }

    /// Runs the installed ELF process tree and captures its standard output streams.
    pub fn run_static_elf_captured(&mut self) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        // Declared before the executor so its private pipe identities outlive
        // executor/child cleanup, including early-return and unwind paths.
        let capture_owner = self.prepare_captured_output(true)?;
        let loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        let mut executor = ElfExecutor::with_output(loaded, capture_owner.clone());
        let (status, stdout, stderr) = self.run_static_elf_process(&mut executor)?;
        Ok((conventional_exit_code(status), stdout, stderr))
    }

    pub(crate) fn prepare_captured_output(
        &self,
        capture_output: bool,
    ) -> Result<Option<CapturedOutput>> {
        self.static_elf
            .as_ref()
            .ok_or(Error::StaticElfNotInstalled)?;
        capture_output
            .then(CapturedOutput::try_new)
            .transpose()
            .map_err(Into::into)
    }

    fn run_static_elf_process(
        &mut self,
        executor: &mut ElfExecutor,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        // This loop never dispatches Tool events, including Host-owned workers.
        self.set_rdtsc_interception(false)?;
        self.set_cpuid_interception(false)?;
        let _registration = self.register_guest_thread()?;
        if self.is_guest_thread {
            let entry_registers = self.vcpu.get_regs()?;
            while let Some(pending) = executor
                .take_pending_signal_for_delivery()
                .map_err(|errno| Error::Reverie(errno.into()))?
            {
                if self.deliver_selected_signal_before_thread_entry(
                    executor,
                    entry_registers,
                    pending,
                )? || executor.has_pending_exit()
                {
                    break;
                }
            }
            if let Some(exit) = executor.take_exit() {
                return self.finish_static_elf_thread(executor, exit);
            }
        }
        loop {
            if let Some(status) = self.guest_thread_group_exit_status() {
                return self.finish_static_elf_thread(
                    executor,
                    ProcessExit {
                        status,
                        group: true,
                    },
                );
            }
            if self.is_guest_thread && self.thread_group.cancelled.load(Ordering::Acquire) {
                return self.finish_static_elf_thread(
                    executor,
                    ProcessExit {
                        status: ExitStatus::SUCCESS,
                        group: false,
                    },
                );
            }
            let vcpu_exit = match self.vcpu.run() {
                Ok(exit) => exit,
                Err(Error::Kvm(error)) if error.errno() == libc::EINTR => continue,
                Err(error) => return Err(error),
            };
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            let (segment_update, process_action, mut signal_boundary) = match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let frame_address = exit.args[0];
                    if frame_address != self.syscall_frame_address {
                        return Err(Error::UnexpectedVcpuExit(format!(
                            "syscall frame is at unexpected address {frame_address:#x}",
                        )));
                    }
                    let return_slot = std::ptr::from_mut(exit.ret) as usize;
                    let request = SyscallRequest::read_from(&self.memory, frame_address)?;
                    // SAFETY: return_slot points into this stopped vCPU's stable KVM_RUN mapping.
                    unsafe {
                        (return_slot as *mut u64).write(0);
                    }
                    if request.number() == libc::SYS_rt_sigreturn as u64 {
                        if let Some(restored) =
                            self.restore_rt_sigreturn(executor, frame_address)?
                        {
                            let delivered = self.deliver_pending_signal_from_registers(
                                executor,
                                frame_address,
                                restored,
                            )?;
                            if !delivered {
                                stage_process_syscall_return(
                                    &mut self.memory,
                                    &self.vcpu,
                                    frame_address,
                                    restored,
                                )?;
                            }
                        }
                        if executor.has_pending_exit() {
                            self.discard_process_clear_tid_at_signal_return(executor);
                            self.clear_registered_worker_tid_before_exit(executor);
                        }
                        (None, None, None)
                    } else {
                        let userspace = process_syscall_return_registers(
                            &self.memory,
                            self.vcpu.get_regs()?,
                            frame_address,
                            0,
                            None,
                        )?;
                        executor.set_current_user_stack_pointer(userspace.rsp);
                        let result = executor.execute(&request, &self.memory);
                        SyscallRequest::write_result(&mut self.memory, frame_address, result)?;
                        (
                            executor.take_segment(),
                            executor.take_process_action(),
                            Some((frame_address, result)),
                        )
                    }
                }
                VcpuExit::Hlt => {
                    if self.try_resume_vmware_backdoor_probe()? {
                        continue;
                    }
                    let Some(fault) = self.capture_page_zero_fault(executor)? else {
                        return Err(self.static_elf_halt_error()?);
                    };
                    executor.prepare_captured_page_zero_fault();
                    self.deliver_page_zero_fault(executor, &fault, fault.pending())?;
                    if executor.has_pending_exit() {
                        self.discard_process_clear_tid_at_signal_return(executor);
                        self.clear_registered_worker_tid_before_exit(executor);
                    }
                    (None, None, None)
                }
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            };

            if let Some((segment, address)) = segment_update {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }

            let mut returns_to_original_image = process_action
                .as_ref()
                .is_none_or(ProcessAction::returns_to_original_image);
            if !returns_to_original_image && executor.has_eligible_pending_signal() {
                return Err(Error::UnexpectedVcpuExit(
                    "KVM exec with an eligible deferred signal is unsupported; \
                     delivery requires a syscall return frame"
                        .to_owned(),
                ));
            }
            let continuation = process_action
                .as_ref()
                .map(|action| {
                    let (frame_address, _) = signal_boundary
                        .expect("a process action can only originate at a syscall boundary");
                    CompletedSyscallBoundary::capture_for_action(self, frame_address, None, action)
                })
                .transpose()?;
            if let Some(action) = process_action {
                let outcome = self.run_process_action_at_boundary(
                    executor,
                    action,
                    continuation.expect("a process action has a continuation policy"),
                )?;
                returns_to_original_image = !outcome.image_replaced;
                if let Some((_, result)) = signal_boundary.as_mut() {
                    *result = outcome.syscall_result;
                }
            }

            // exit/exit_group never return to userspace, so pending delivery
            // cannot replace an exit already selected by the syscall.
            let mut pending_exit = executor.take_exit();
            if returns_to_original_image
                && pending_exit.is_none()
                && let Some((frame_address, result)) = signal_boundary
            {
                self.deliver_pending_signal_at_syscall_boundary(executor, frame_address, result)?;
            }

            pending_exit = pending_exit.or_else(|| executor.take_exit());
            if let Some(exit) = pending_exit {
                return self.finish_static_elf_thread(executor, exit);
            }
        }
    }

    fn finish_static_elf_thread(
        &mut self,
        executor: &mut ElfExecutor,
        exit: ProcessExit,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        executor.retire_current_thread(exit.status, exit.group);
        self.clear_registered_worker_tid_before_exit(executor);
        executor.release_files_on_exit();
        self.release_stdin_on_exit();
        if exit.group {
            self.request_guest_thread_group_exit(exit.status);
        }
        let status = if self.is_guest_thread {
            exit.status
        } else {
            if exit.group {
                self.cancel_guest_threads();
            } else {
                self.join_guest_threads();
            }
            self.guest_worker_teardown_result()?;
            executor.process_exit_status().ok_or_else(|| {
                Error::UnexpectedVcpuExit(
                    "KVM process has no final task exit status after joining workers".to_owned(),
                )
            })?
        };
        let (stdout, stderr) = executor.take_output();
        Ok((status, stdout, stderr))
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 3714..3737; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
    /// Runs until the guest halts, invoking `handler` for each syscall vmcall.
    pub fn run<F>(&mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(Syscall, &GuestMemory) -> i64,
    {
        self.set_rdtsc_interception(false)?;
        self.set_cpuid_interception(false)?;
        loop {
            let vcpu_exit = self.vcpu.run()?;
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let syscall =
                        SyscallRequest::read_from(&self.memory, exit.args[0])?.into_syscall()?;
                    *exit.ret = handler(syscall, &self.memory) as u64;
                }
                VcpuExit::Hlt => return Ok(()),
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            }
        }
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/vm.rs lines 3817..3842; full file SHA256 e05cbfbe1ad7890380fb7454d74c78a28ad47c11ad402100420fad9bb0665a46
        .iter()
        .find(|entry| entry.function == 0x8000_0001)
        .is_some_and(|entry| entry.ecx & (1 << 2) != 0);
    if supports_vmmcall {
        return Ok(VMMCALL);
    }
    Err(Error::HypercallInstructionUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("cpuid_runtime_tests.rs");

    mod initialization_tests {
        use std::io::Seek;
        use std::io::SeekFrom;
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;
        use std::process::Child;
        use std::process::Command;
        use std::process::Stdio;
        use std::sync::atomic::AtomicU64;
        use std::time::Duration;
        use std::time::Instant;

