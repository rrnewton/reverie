// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie/src/subscription.rs lines 60..65; SHA256 638a1482113e9ddc33e94319bbf1d70aa1807ad3336df16f58175719058d7acc
    /// Enable interception of the `cpuid` instruction.
    #[inline]
    pub fn cpuid(&mut self) -> &mut Self {
        self.instructions.insert(Instructions::CPUID);
        self
    }

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie/src/subscription.rs lines 73..77; SHA256 638a1482113e9ddc33e94319bbf1d70aa1807ad3336df16f58175719058d7acc
    /// Returns true if we're subscribed to CPUID events.
    #[inline]
    pub fn has_cpuid(&self) -> bool {
        self.instructions.contains(Instructions::CPUID)
    }

