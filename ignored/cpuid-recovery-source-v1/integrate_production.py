from pathlib import Path
N=Path(__file__).resolve().parent;S=N/'source'
def replace(rel,a,b,count=1):
 p=S/rel;t=p.read_text();assert t.count(a)==count,(rel,a[:100],t.count(a));p.write_text(t.replace(a,b))
VM='reverie-kvm/src/vm.rs';RT='reverie-kvm/src/runtime.rs'
replace('reverie-kvm/src/lib.rs','mod cpuid;','mod cpuid;\nmod cpuid_instruction;')
replace('reverie-kvm/src/cpuid_instruction.rs','let admission = (||','let admission: Result<()> = (||')
replace(VM,'pub(crate) struct TimestampBoundary {','pub(crate) struct InstructionBoundary<I> {')
replace(VM,'pub(crate) instruction: crate::timestamp::TimestampInstruction,','pub(crate) instruction: I,')
replace(VM,'impl TimestampBoundary {','''pub(crate) type TimestampBoundary = InstructionBoundary<crate::timestamp::TimestampInstruction>;
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

impl<I> InstructionBoundary<I> {''')
replace(VM,'    intercept_rdtsc: bool,','    intercept_rdtsc: bool,\n    cpuid_interception: crate::cpuid_instruction::Interception,')
replace(VM,'            intercept_rdtsc: false,','            intercept_rdtsc: false,\n            cpuid_interception: crate::cpuid_instruction::Interception::default(),')
marker='    pub(crate) fn timestamp_counter_exception(&self) -> Result<Option<TimestampBoundary>> {'
insert='''    pub(crate) fn set_cpuid_interception(&mut self, enabled: bool) -> Result<()> {
        self.cpuid_interception.configure(&self.vcpu, enabled)
    }

    pub(crate) fn has_cpuid_interception(&self) -> bool {
        self.cpuid_interception.enabled()
    }

    pub(crate) fn tool_instruction_exception(&self) -> Result<Option<ToolInstructionBoundary>> {
        if let Some(boundary) = self.cpuid_instruction_exception()? {
            return Ok(Some(ToolInstructionBoundary::Cpuid(boundary)));
        }
        Ok(self.timestamp_counter_exception()?.map(ToolInstructionBoundary::Timestamp))
    }

    fn cpuid_instruction_exception(&self) -> Result<Option<CpuidBoundary>> {
        if !self.cpuid_interception.enabled() { return Ok(None); }
        let Some(exception) = self.static_elf_exception()? else { return Ok(None); };
        // CPUID is never an emulated #UD. LOCK and unrelated faults retain
        // their genuine exception, even when this Tool subscribes to CPUID.
        if exception.vector != 13 { return Ok(None); }
        self.cpuid_interception.verify(&self.vcpu)?;
        let halted = self.vcpu.get_regs()?;
        let special = self.vcpu.get_sregs()?;
        if special.cr0 & (1 << 31) == 0
            || special.efer & ((1 << 10) | (1 << 11)) != ((1 << 10) | (1 << 11))
            || special.cr4 & (1 << 12) != 0
        { return Ok(None); }
        let mut frame = [0; 6 * 8];
        self.memory.read_raw(halted.rsp, &mut frame)?;
        let word = |index: usize| u64::from_le_bytes(frame[index * 8..index * 8 + 8].try_into().expect("exception frame word"));
        let cs = word(2);
        let ss = word(5);
        if word(0) != 0 || cs != u64::from(crate::signal::USER_CODE_SELECTOR)
            || ![u64::from(crate::signal::USER_DATA_SELECTOR), u64::from(crate::signal::USER_DATA_SELECTOR & !3)].contains(&ss)
        { return Ok(None); }
        let Some(instruction) = crate::cpuid_instruction::decode(|offset| {
            let address = exception.instruction_pointer.checked_add(u64::from(offset))?;
            crate::timestamp::fetch_user_byte(&self.memory, special.cr3, address)
        }) else { return Ok(None); };
        let mut registers = halted;
        registers.rip = exception.instruction_pointer;
        registers.rsp = exception.stack_pointer;
        registers.rflags = exception.rflags;
        Ok(Some(CpuidBoundary { registers, instruction, special_registers: special,
            code_segment: cs as u16, stack_segment: ss as u16 }))
    }

'''
replace(VM,marker,insert+marker)
marker='''        // Exception stubs preserve every GPR. Returning host-side injections'''
replace(VM,marker,'''        self.resume_instruction_registers(boundary, registers)
    }

    pub(crate) fn resume_tool_instruction(&mut self, boundary: ToolInstructionBoundary, result: ToolInstructionResult) -> Result<()> {
        match (boundary, result) {
            (ToolInstructionBoundary::Timestamp(boundary), ToolInstructionResult::Timestamp(result)) => self.resume_timestamp_counter(boundary, result),
            (ToolInstructionBoundary::Cpuid(boundary), ToolInstructionResult::Cpuid(result)) => {
                let registers = crate::cpuid_instruction::result_registers(boundary.registers, boundary.instruction, result)
                    .ok_or_else(|| Error::UnexpectedVcpuExit("CPUID RIP overflow".to_owned()))?;
                self.resume_instruction_registers(boundary, registers)
            }
            _ => Err(Error::UnexpectedVcpuExit("instruction callback result kind changed".to_owned())),
        }
    }

    fn resume_instruction_registers<I>(&mut self, boundary: InstructionBoundary<I>, registers: kvm_regs) -> Result<()> {
'''+marker)
replace(VM,'// The timestamp has now retired. Preserve this backend\'s existing','// The instruction has now retired. Preserve this backend\'s existing')
replace(VM,'        self.set_rdtsc_interception(false)?;','        self.set_rdtsc_interception(false)?;\n        self.set_cpuid_interception(false)?;',2)
replace(RT,'use crate::vm::', 'use crate::vm::',0) if False else None
replace(RT,'trait GuestSyscallExecutor<T: Tool>: Send + Sync {\n    fn read_clock(&self) -> Result<u64>;','''trait GuestSyscallExecutor<T: Tool>: Send + Sync {
    fn read_clock(&self) -> Result<u64>;

    fn has_cpuid_interception(&self) -> bool { false }''')
replace(RT,'''    fn read_clock(&self) -> Result<u64> {
        self.backend.vcpu.read_clock()
    }''','''    fn read_clock(&self) -> Result<u64> {
        self.backend.vcpu.read_clock()
    }

    fn has_cpuid_interception(&self) -> bool {
        self.backend.has_cpuid_interception()
    }''')
replace(RT,'impl<T: Tool> Guest<T> for KvmGuest<\'_, T> {','''impl<T: Tool> Guest<T> for KvmGuest<'_, T> {
    fn has_cpuid_interception(&self) -> bool {
        self.executor.has_cpuid_interception()
    }
''')
replace(RT,'    Timestamp,','    Instruction,')
p=S/RT;t=p.read_text();t=t.replace('Self::Timestamp','Self::Instruction').replace('ProcessExecutionContext::Timestamp','ProcessExecutionContext::Instruction')
t=t.replace('from a timestamp callback is unsupported','from an instruction callback is unsupported').replace('terminal timestamp injection lost its exit','terminal instruction injection lost its exit').replace('nonreturning timestamp callback outcome','nonreturning instruction callback outcome').replace('timestamp callback changed the process continuation','instruction callback changed the process continuation').replace('The original timestamp never resumes','The original instruction never resumes')
p.write_text(t)
replace(RT,'        self.set_rdtsc_interception(false)?;','        self.set_rdtsc_interception(false)?;\n        self.set_cpuid_interception(false)?;')
replace(RT,'            self.set_rdtsc_interception(subscriptions.has_rdtsc())?;','            self.set_rdtsc_interception(subscriptions.has_rdtsc())?;\n            self.set_cpuid_interception(subscriptions.has_cpuid())?;')
replace(RT,'''                        if let Some(boundary) = self.timestamp_counter_exception()? {
                            let request = boundary.instruction.request;
                            executor.set_current_user_stack_pointer(boundary.registers.rsp);''','''                        if let Some(boundary) = self.tool_instruction_exception()? {
                            executor.set_current_user_stack_pointer(boundary.user_registers().rsp);''')
replace(RT,'                                    tool.handle_rdtsc_event(&mut guest, request),','''                                    async {
                                        match &boundary {
                                            crate::vm::ToolInstructionBoundary::Timestamp(boundary) => tool
                                                .handle_rdtsc_event(&mut guest, boundary.instruction.request)
                                                .await.map(crate::vm::ToolInstructionResult::Timestamp),
                                            crate::vm::ToolInstructionBoundary::Cpuid(boundary) => tool
                                                .handle_cpuid_event(&mut guest, boundary.registers.rax as u32, boundary.registers.rcx as u32)
                                                .await.map(crate::vm::ToolInstructionResult::Cpuid),
                                        }
                                    },''')
replace(RT,'                            self.resume_timestamp_counter(boundary, value)?;','                            self.resume_tool_instruction(boundary, value)?;')
print('Prepared CPUID production integration only; no build or tests.')
