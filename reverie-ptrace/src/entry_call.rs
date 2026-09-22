// Byte-exact call preparation and stop-order predicates for the private
// implementation's actual stopped-task adapter in task/after_loader_task.rs.
// No method writes a guest clock, selects a scheduler turn, or resumes a tracee.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImageIdentity {
    pub tid: i32,
    pub start_ticks: u64,
    pub generation: u64,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub at_entry: u64,
    pub at_phdr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TrapObservation {
    pub identity: ImageIdentity,
    pub signal: i32,
    pub si_code: i32,
    pub rip: u64,
    pub rsp: u64,
    pub r10: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    Identity,
    Address,
    Instruction,
    Signal,
    Order,
    Return,
    CounterWentBackwards,
}

type Result<T> = std::result::Result<T, Refusal>;

fn breakpoint(observed: &TrapObservation) -> Result<()> {
    // Linux x86 INT3 may report TRAP_BRKPT or SI_KERNEL. A user/tkill signal,
    // single-step trap, hardware breakpoint and ptrace event are not this stop.
    if observed.signal != 5 || !matches!(observed.si_code, 1 | 128) {
        return Err(Refusal::Signal);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct EntryGuard {
    identity: ImageIdentity,
    original: [u8; 8],
}

/// Proof that the stopped task reached the controller-installed executable
/// entry breakpoint with the exact image identity and complete guarded word.
/// Only `Calls::at_entry` can mint this value.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedEntryWord {
    identity: ImageIdentity,
    original: [u8; 8],
    guarded: [u8; 8],
}

impl AuthenticatedEntryWord {
    pub fn identity(&self) -> ImageIdentity {
        self.identity
    }

    pub fn original(&self) -> [u8; 8] {
        self.original
    }

    pub fn guarded(&self) -> [u8; 8] {
        self.guarded
    }
}
impl EntryGuard {
    // Adapter must capture identity + original bytes at the real exec stop,
    // verify complete executable/readable mapping, install INT3, and read back
    // guarded_word() before publishing this state or allowing a resume.
    pub fn prepare(identity: ImageIdentity, original: [u8; 8]) -> Result<Self> {
        if identity.tid <= 0
            || identity.start_ticks == 0
            || identity.generation == 0
            || identity.executable_device == 0
            || identity.executable_inode == 0
            || identity.at_phdr == 0
            || identity.at_entry == 0
            || identity.at_entry.checked_add(8).is_none()
            || original[0] == 0xcc
        {
            return Err(Refusal::Address);
        }
        Ok(Self { identity, original })
    }
    pub fn guarded_word(&self) -> [u8; 8] {
        let mut word = self.original;
        word[0] = 0xcc;
        word
    }
    pub fn authenticate(&self, stop: &TrapObservation, word: [u8; 8]) -> Result<()> {
        breakpoint(stop)?;
        if stop.identity != self.identity {
            return Err(Refusal::Identity);
        }
        if stop.rip != self.identity.at_entry + 1 {
            return Err(Refusal::Address);
        }
        if word != self.guarded_word() {
            return Err(Refusal::Instruction);
        }
        Ok(())
    }
    pub fn original(&self) -> [u8; 8] {
        self.original
    }
}

// The prepared code uses a real CALL/RET pair. Forging a return address on the
// guest's stack would corrupt the red zone and would not balance CET shadow
// stack state. The first fixture still requires CET to be inactive: complete
// CET state preservation is a separate unimplemented adapter requirement.
#[derive(Debug)]
pub(crate) struct CallCode {
    image: ImageIdentity,
    pub bytes: [u8; 30],
    pub entry: u64,
    pub return_rip: u64,
    pub call_stack_top: u64,
    marker: u64,
}
impl CallCode {
    pub fn prepare(
        guard: &EntryGuard,
        code: u64,
        stack_top: u64,
        function: u64,
        marker: u64,
    ) -> Result<Self> {
        if code == 0 || function == 0 || stack_top < 4096 || stack_top & 15 != 0 {
            return Err(Refusal::Address);
        }
        let return_rip = code.checked_add(28).ok_or(Refusal::Address)?;
        let _ = code.checked_add(30).ok_or(Refusal::Address)?;
        let mut bytes = [0u8; 30];
        bytes[0..4].copy_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // endbr64
        bytes[4..6].copy_from_slice(&[0x49, 0xbb]); // movabs function,%r11
        bytes[6..14].copy_from_slice(&function.to_le_bytes());
        bytes[14..17].copy_from_slice(&[0x41, 0xff, 0xd3]); // call *%r11
        bytes[17..19].copy_from_slice(&[0x49, 0xba]); // movabs marker,%r10
        bytes[19..27].copy_from_slice(&marker.to_le_bytes());
        bytes[27] = 0xcc; // int3; stopped RIP is code + 28
        bytes[28..30].copy_from_slice(&[0x0f, 0x0b]); // ud2 if erroneously resumed
        Ok(Self {
            image: guard.identity,
            bytes,
            entry: code,
            return_rip,
            call_stack_top: stack_top,
            marker,
        })
    }
    pub fn authenticate_return(&self, stop: &TrapObservation, readback: &[u8]) -> Result<()> {
        breakpoint(stop)?;
        if stop.identity != self.image {
            return Err(Refusal::Identity);
        }
        if stop.rip != self.return_rip || stop.rsp != self.call_stack_top || stop.r10 != self.marker
        {
            return Err(Refusal::Return);
        }
        if readback != self.bytes {
            return Err(Refusal::Instruction);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    EntryHeld,
    Dlopen,
    Loaded,
    Initializing,
    BeginObserved,
    ReadyObserved,
    InitializerReturned,
    Restored,
    Failed,
}

#[derive(Debug)]
pub(crate) struct Calls {
    guard: EntryGuard,
    authenticated_entry: Option<AuthenticatedEntryWord>,
    phase: Phase,
    // Keep the dlopen reference alive for this image; never call dlclose after
    // any initializer attempt. Remote handle is not a host pointer to dereference.
    pub handle: Option<u64>,
}
impl Calls {
    pub fn at_entry(guard: EntryGuard, stop: &TrapObservation, word: [u8; 8]) -> Result<Self> {
        guard.authenticate(stop, word)?;
        let authenticated_entry = AuthenticatedEntryWord {
            identity: guard.identity,
            original: guard.original(),
            guarded: guard.guarded_word(),
        };
        Ok(Self {
            guard,
            authenticated_entry: Some(authenticated_entry),
            phase: Phase::EntryHeld,
            handle: None,
        })
    }
    pub fn guard(&self) -> &EntryGuard {
        &self.guard
    }
    pub fn take_authenticated_entry(&mut self) -> Result<AuthenticatedEntryWord> {
        if self.phase != Phase::EntryHeld {
            self.phase = Phase::Failed;
            return Err(Refusal::Order);
        }
        match self.authenticated_entry.take() {
            Some(authenticated) => Ok(authenticated),
            None => {
                self.phase = Phase::Failed;
                Err(Refusal::Order)
            }
        }
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    fn advance(&mut self, expected: Phase, next: Phase) -> Result<()> {
        if self.phase != expected {
            self.phase = Phase::Failed;
            return Err(Refusal::Order);
        }
        self.phase = next;
        Ok(())
    }
    pub fn start_dlopen(&mut self) -> Result<()> {
        self.advance(Phase::EntryHeld, Phase::Dlopen)
    }
    fn check_return(
        &mut self,
        code: &CallCode,
        stop: &TrapObservation,
        readback: &[u8],
    ) -> Result<()> {
        if code.image != self.guard.identity {
            self.phase = Phase::Failed;
            return Err(Refusal::Identity);
        }
        if let Err(error) = code.authenticate_return(stop, readback) {
            self.phase = Phase::Failed;
            return Err(error);
        }
        Ok(())
    }
    pub fn errno_returned(
        &mut self,
        code: &CallCode,
        stop: &TrapObservation,
        readback: &[u8],
    ) -> Result<()> {
        self.check_return(code, stop, readback)?;
        if !matches!(self.phase, Phase::EntryHeld | Phase::InitializerReturned) {
            self.phase = Phase::Failed;
            return Err(Refusal::Order);
        }
        Ok(())
    }
    // The adapter also renews its all-task quiescence and mapping checks.
    pub fn dlopen_returned(
        &mut self,
        code: &CallCode,
        stop: &TrapObservation,
        readback: &[u8],
        rax: u64,
    ) -> Result<()> {
        self.check_return(code, stop, readback)?;
        self.advance(Phase::Dlopen, Phase::Loaded)?;
        if rax == 0 {
            self.phase = Phase::Failed;
            return Err(Refusal::Return);
        }
        self.handle = Some(rax);
        Ok(())
    }
    // Adapter resolves/revalidates the exact ordinary initializer in the loaded
    // runtime before calling this. A dlopen handle or pathname alone is not proof.
    pub fn start_initializer(&mut self) -> Result<()> {
        self.advance(Phase::Loaded, Phase::Initializing)
    }
    // Both calls require existing exact DSO/RIP/frame validation plus identity.
    pub fn begin(&mut self) -> Result<()> {
        self.advance(Phase::Initializing, Phase::BeginObserved)
    }
    pub fn ready(&mut self) -> Result<()> {
        self.advance(Phase::BeginObserved, Phase::ReadyObserved)
    }
    pub fn initializer_returned(
        &mut self,
        code: &CallCode,
        stop: &TrapObservation,
        readback: &[u8],
        eax: u32,
    ) -> Result<()> {
        self.check_return(code, stop, readback)?;
        self.advance(Phase::ReadyObserved, Phase::InitializerReturned)?;
        // The exported return type is C int; high RAX bits are not its ABI result.
        if eax != 0 {
            self.phase = Phase::Failed;
            return Err(Refusal::Return);
        }
        Ok(())
    }
    // Adapter calls only after byte/state/policy/identity readback passes. It then
    // publishes runtime Ready for this generation and resumes saved AT_ENTRY.
    pub fn restored(&mut self) -> Result<()> {
        self.advance(Phase::InitializerReturned, Phase::Restored)
    }
}

// Diagnostic-only samples of the EXISTING ptrace Timer. Do not lazy-initialize
// an absent Timer merely to measure it: add a non-initializing adapter read.
// These differences include every branch, including any callback. They are not
// automatically private branches and MUST NOT be subtracted from guest time.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RawClockInterval {
    pub before: u64,
    pub after: u64,
}
impl RawClockInterval {
    pub fn delta(&self) -> Result<u64> {
        self.after
            .checked_sub(self.before)
            .ok_or(Refusal::CounterWentBackwards)
    }
}

// Explicit ABI bytes copied to owned target data storage, never its environment.
pub(crate) fn host_config() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&1u64.to_le_bytes());
    // straddler_staleness_ticks = 0; no concurrent publication authorized.
    bytes
}

#[cfg(test)]
mod tests;
