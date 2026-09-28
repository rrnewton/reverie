// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie/src/tool.rs lines 433..448; SHA256 8d7d1bc3c70d31aa498b265fc5de6a5ae61dad1c9aa4d821422a09b9f6147207
    /// CPUID is trapped, the tool should implement this function to return
    /// `[eax, ebx, ecx, edx]`.
    ///
    /// NOTE:
    ///  * This is never called by default unless cpuid events are subscribed
    ///    to.
    ///  * This is only available on x86_64.
    #[cfg(target_arch = "x86_64")]
    async fn handle_cpuid_event<T: Guest<Self>>(
        &self,
        _guest: &mut T,
        eax: u32,
        ecx: u32,
    ) -> Result<raw_cpuid::CpuIdResult, Errno> {
        Ok(raw_cpuid::cpuid!(eax, ecx))
    }

