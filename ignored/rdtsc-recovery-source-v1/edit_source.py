from pathlib import Path
P=Path(__file__).resolve().parent; S=P/'source/reverie-kvm/src'
def edit(name,old,new):
 p=S/name;s=p.read_text();assert s.count(old)==1,(name,old[:70],s.count(old));p.write_text(s.replace(old,new))
edit('lib.rs','mod syscall;\n','mod syscall;\nmod timestamp;\n')
edit('bootstrap.rs','const CR4_PAE: u64 = 1 << 5;','const CR4_TSD: u64 = 1 << 2;\nconst CR4_PAE: u64 = 1 << 5;')
edit('bootstrap.rs','/// The register state Linux hands a freshly `exec`\'d x86-64 process.', '''/// Set CPL3 timestamp interception only for the loop that owns its Tool callback.
/// CR4 is updated in place; unrelated execution and paging bits are retained.
pub(crate) fn set_userspace_rdtsc_interception(vcpu: &VcpuFd, enabled: bool) -> Result<()> {
    let mut sregs = vcpu.get_sregs()?;
    if enabled {
        sregs.cr4 |= CR4_TSD;
    } else {
        sregs.cr4 &= !CR4_TSD;
    }
    vcpu.set_sregs(&sregs)?;
    Ok(())
}

/// The register state Linux hands a freshly `exec`'d x86-64 process.''')
edit('vm.rs','    thread_ownership_override: Option<ThreadOwnership>,','''    thread_ownership_override: Option<ThreadOwnership>,
    // Set by the active execution consumer, never copied to a new vCPU. A
    // Host-owned worker has no Tool dispatcher and must retain native TSC.
    intercept_rdtsc: bool,''')
edit('vm.rs','            thread_ownership_override: None,','            thread_ownership_override: None,\n            intercept_rdtsc: false,')
edit('vm.rs','#[derive(Clone)]\npub(crate) struct PageZeroFault {','''pub(crate) struct TimestampBoundary {
    pub(crate) registers: kvm_regs,
    pub(crate) instruction: crate::timestamp::TimestampInstruction,
    special_registers: kvm_bindings::kvm_sregs,
    code_segment: u16,
    stack_segment: u16,
}

impl TimestampBoundary {
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
}

#[derive(Clone)]
pub(crate) struct PageZeroFault {''')
edit('vm.rs','    pub(crate) fn capture_page_zero_fault(','''    pub(crate) fn set_rdtsc_interception(&mut self, enabled: bool) -> Result<()> {
        crate::bootstrap::set_userspace_rdtsc_interception(&self.vcpu, enabled)?;
        self.intercept_rdtsc = enabled;
        Ok(())
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
        let halted = self.vcpu.get_regs()?;
        let special = self.vcpu.get_sregs()?;
        if special.cr0 & (1 << 31) == 0
            || special.efer & ((1 << 10) | (1 << 11)) != ((1 << 10) | (1 << 11))
            || special.cr4 & (1 << 12) != 0
            || special.cr4 & (1 << 2) == 0
        {
            return Ok(None);
        }
        let mut frame = [0; 6 * 8];
        let words = if exception.vector == 13 { 6 } else { 5 };
        self.memory.read_raw(halted.rsp, &mut frame[..words * 8])?;
        let word = |index: usize| u64::from_le_bytes(
            frame[index * 8..index * 8 + 8].try_into().expect("exception frame word"));
        let first = usize::from(exception.vector == 13);
        let cs = word(first + 1);
        let ss = word(first + 4);
        if (exception.vector == 13 && word(0) != 0)
            || cs != u64::from(crate::signal::USER_CODE_SELECTOR)
            || ![u64::from(crate::signal::USER_DATA_SELECTOR),
                 u64::from(crate::signal::USER_DATA_SELECTOR & !3)].contains(&ss)
        {
            return Ok(None);
        }
        let Some(instruction) = crate::timestamp::decode(|offset| {
            let address = exception.instruction_pointer.checked_add(u64::from(offset))?;
            crate::timestamp::fetch_user_byte(&self.memory, special.cr3, address)
        }) else {
            return Ok(None);
        };
        // RDTSC is enabled by every supported CPUID policy. Only RDTSCP may
        // fault as #UD (the deterministic policy does not expose that feature).
        if exception.vector == 6 && instruction.request != reverie::Rdtsc::Tscp {
            return Ok(None);
        }
        let mut registers = halted;
        registers.rip = exception.instruction_pointer;
        registers.rsp = exception.stack_pointer;
        registers.rflags = exception.rflags;
        Ok(Some(TimestampBoundary {
            registers, instruction, special_registers: special,
            code_segment: cs as u16, stack_segment: ss as u16,
        }))
    }

    pub(crate) fn resume_timestamp_counter(
        &mut self,
        boundary: TimestampBoundary,
        result: reverie::RdtscResult,
    ) -> Result<()> {
        let registers = crate::timestamp::result_registers(
            boundary.registers, boundary.instruction, result,
        ).ok_or_else(|| Error::UnexpectedVcpuExit("timestamp RIP overflow".to_owned()))?;
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
        Ok(())
    }

    pub(crate) fn capture_page_zero_fault(''')
edit('vm.rs','''    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let _registration = self.register_guest_thread()?;''','''    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        // This loop never dispatches Tool events, including Host-owned workers.
        self.set_rdtsc_interception(false)?;
        let _registration = self.register_guest_thread()?;''')
edit('vm.rs','''    {
        loop {
            let vcpu_exit = self.vcpu.run()?;''','''    {
        self.set_rdtsc_interception(false)?;
        loop {
            let vcpu_exit = self.vcpu.run()?;''')
edit('runtime.rs','    Lifecycle,\n','    Lifecycle,\n    Timestamp,\n')
edit('runtime.rs','''            Self::SignalBoundary(_) | Self::FaultBoundary(_) | Self::ThreadEntrySignal
        )''','''            Self::SignalBoundary(_) | Self::FaultBoundary(_) | Self::ThreadEntrySignal
                | Self::Timestamp
        )''')
edit('runtime.rs','''        if matches!(self, Self::ThreadEntrySignal) {''','''        if matches!(self, Self::ThreadEntrySignal | Self::Timestamp) {''')
edit('runtime.rs','''        if matches!(self.process_context, ProcessExecutionContext::Lifecycle)
''','''        if matches!(self.process_context, ProcessExecutionContext::Lifecycle | ProcessExecutionContext::Timestamp)
''')
edit('runtime.rs','''                    ProcessExecutionContext::ThreadEntrySignal => Err(Error::UnexpectedVcpuExit(''','''                    ProcessExecutionContext::Timestamp => Err(Error::UnexpectedVcpuExit(
                        "process injection from a timestamp callback is unsupported".to_owned(),
                    )),
                    ProcessExecutionContext::ThreadEntrySignal => Err(Error::UnexpectedVcpuExit(''')
edit('runtime.rs','''            self.vcpu.track_clock()?;
            _registration = Some(self.register_guest_thread()?);''','''            // Each actual Tool consumer admits its own subscription before
            // any user instruction. New fork/thread vCPUs start unarmed; the
            // tool-less Host worker loop never inherits trapping without a hook.
            self.set_rdtsc_interception(subscriptions.has_rdtsc())?;
            self.vcpu.track_clock()?;
            _registration = Some(self.register_guest_thread()?);''')
edit('runtime.rs','''                    VcpuExit::Hlt => {
                        if self.try_resume_vmware_backdoor_probe()? {''','''                    VcpuExit::Hlt => {
                        if let Some(boundary) = self.timestamp_counter_exception()? {
                            let request = boundary.instruction.request;
                            executor.set_current_user_stack_pointer(boundary.registers.rsp);
                            let handler_signal = Arc::new(Mutex::new(None));
                            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                            let mut process_completed = false;
                            expose_tool_scratch(&memory, tool_stack_top)?;
                            let result = {
                                let mut guest_executor = StaticElfSyscallExecutor {
                                    backend: self, executor, memory: memory.clone(),
                                    process_context: ProcessExecutionContext::Timestamp,
                                    callback_site: None, original_syscall: None,
                                    signal_guard: SignalGuard::Ordinary,
                                    last_result: None, process_completed: &mut process_completed,
                                };
                                let mut guest = KvmGuest::<T>::new(
                                    pid, tid, tool.clone(), memory.clone(), &auxv,
                                    boundary.user_registers(), &mut thread_state,
                                    &mut guest_executor, global_state.as_ref(),
                                    Some(global_state.clone()), config, subscriptions,
                                    handler_signal.clone(), pending_child_starts.clone(),
                                    tool_stack_top, stack_checked_out.clone(),
                                );
                                drive_handler(
                                    tool.handle_rdtsc_event(&mut guest, request),
                                    handler_signal, pending_child_starts,
                                    wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
                                ).await
                            };
                            // Hide scratch even on callback failure. The outer
                            // process supervisor retains any committed effects.
                            let hidden = hide_tool_scratch(&memory, tool_stack_top);
                            let value = match result {
                                HandlerOutcome::Returned(value) => value.map_err(|error| Error::Reverie(error.into()))?,
                                HandlerOutcome::ThreadCancelled => {
                                    hidden?;
                                    return Ok(self.cancelled_tool_thread_status(executor));
                                }
                                HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                                HandlerOutcome::RuntimeError(error) => return Err(error),
                                HandlerOutcome::TailInjected { .. }
                                | HandlerOutcome::ParkedFatal(_)
                                | HandlerOutcome::ParkedCancelled(_) => {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "nonreturning timestamp callback outcome".to_owned(),
                                    ));
                                }
                            };
                            hidden?;
                            if process_completed {
                                return Err(Error::UnexpectedVcpuExit(
                                    "timestamp callback changed the process continuation".to_owned(),
                                ));
                            }
                            if let Some((segment, address)) = executor.take_segment() {
                                set_user_segment_base(&self.vcpu, segment, address)?;
                            }
                            self.resume_timestamp_counter(boundary, value)?;
                            continue;
                        }
                        if self.try_resume_vmware_backdoor_probe()? {''')
# Retain every historical static control, unchanged, as additions to the current suite.
test=S.parent/'tests/static_elf.rs';s=test.read_text()
for use in ['use reverie::Rdtsc;','use reverie::RdtscResult;']:
 assert use not in s;s=s.replace('use reverie::Pid;','use reverie::Pid;\n'+use)
for name in ['841107ac.patch','d65ab382.patch','96306604.patch']:
 body=(P/'evidence'/name).read_text();part=body.split('diff --git a/reverie-kvm/tests/static_elf.rs b/reverie-kvm/tests/static_elf.rs\n',1)[1]
 added='\n'.join(line[1:] for line in part.splitlines() if line.startswith('+') and not line.startswith('+++') and not line.startswith('+use reverie::'))
 s+='\n\n'+added+'\n'
test.write_text(s)
print('Ported static Tool dispatch, consumer-local TSD admission, decoder and unchanged historical controls; no execution.')
