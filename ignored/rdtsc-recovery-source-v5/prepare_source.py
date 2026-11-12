from pathlib import Path
import shutil
N=Path(__file__).resolve().parent;P=N.parent/'rdtsc-recovery-source-v4'
shutil.copytree(P/'source',N/'source',symlinks=True)
p=N/'source/reverie-kvm/src/runtime.rs';s=p.read_text()
s=s.replace('fn tail_injection_allowed(&self) -> bool {','fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {')
old='''    fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        !matches!(
            self,
            Self::SignalBoundary(_)
                | Self::FaultBoundary(_)
                | Self::ThreadEntrySignal
                | Self::Timestamp
        )
    }
'''
new='''    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Timestamp) {
            // Terminal exits do not need a syscall return frame. Every other
            // tail still requires a transport that can consume its result.
            return injection_is_explicit_exit(request);
        }
        !matches!(
            self,
            Self::SignalBoundary(_) | Self::FaultBoundary(_) | Self::ThreadEntrySignal
        )
    }
'''
assert s.count(old)==1;s=s.replace(old,new)
old='''    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::ThreadEntrySignal | Self::Timestamp) {'''
new='''    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Timestamp) && injection_is_explicit_exit(request) {
            // inject() itself is nonreturning once the real exit is staged;
            // tail_inject() uses that same path and both executor preflights.
            return true;
        }
        if matches!(self, Self::ThreadEntrySignal | Self::Timestamp) {'''
assert s.count(old)==1;s=s.replace(old,new)
needle='''/// Returns whether a successful injected syscall can abandon the current Tool
'''
new='''fn injection_is_explicit_exit(request: &SyscallRequest) -> bool {
    matches!(request.number() as libc::c_long, libc::SYS_exit | libc::SYS_exit_group)
}

'''+needle
assert s.count(needle)==1;s=s.replace(needle,new)
old='''    fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        self.signal_guard == SignalGuard::Ordinary
            && !self.executor.has_prepared_signal()
            && self.process_context.tail_injection_allowed()
    }'''
new='''    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.signal_guard == SignalGuard::Ordinary
            && !self.executor.has_prepared_signal()
            && self.process_context.tail_injection_allowed(request)
    }'''
assert s.count(old)==1;s=s.replace(old,new)
old='''    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        if !self.executor.tail_injection_allowed() {'''
new='''    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        let request = SyscallRequest::from_syscall(syscall);
        if !self.executor.tail_injection_allowed(&request) {'''
assert s.count(old)==1;s=s.replace(old,new)
old='''    {
        self.vcpu.track_clock()?;
        let tool_stack_top = self.tool_stack_top();'''
new='''    {
        // This public non-ELF loop has no timestamp consumer. Establish its
        // ownership locally even if the vCPU previously ran a subscribed Tool.
        self.set_rdtsc_interception(false)?;
        self.vcpu.track_clock()?;
        let tool_stack_top = self.tool_stack_top();'''
assert s.count(old)==1;s=s.replace(old,new)
old='''                                HandlerOutcome::TailInjected { .. }
                                | HandlerOutcome::ParkedFatal(_)'''
new='''                                HandlerOutcome::TailInjected {
                                    result: Ok(_),
                                    image_replaced: false,
                                    process_exited: true,
                                } => {
                                    hidden?;
                                    let exit = executor.take_exit().ok_or_else(|| {
                                        Error::UnexpectedVcpuExit(
                                            "terminal timestamp injection lost its exit".to_owned(),
                                        )
                                    })?;
                                    if exit.group {
                                        self.request_guest_thread_group_exit(exit.status);
                                    }
                                    // The original timestamp never resumes. Normal
                                    // retirement and consuming hooks own cleanup.
                                    return Ok(exit.into());
                                }
                                HandlerOutcome::TailInjected { .. }
                                | HandlerOutcome::ParkedFatal(_)'''
assert s.count(old)==1;s=s.replace(old,new)
old='''        let boundary = test_process_boundary();
        assert!(
            !ProcessExecutionContext::SignalBoundary(boundary.clone()).tail_injection_allowed()
        );
        assert!(
            ProcessExecutionContext::SyscallBoundary(boundary.clone()).tail_injection_allowed()
        );
        assert!(ProcessExecutionContext::Lifecycle.tail_injection_allowed());'''
new='''        let boundary = test_process_boundary();
        let request = SyscallRequest::new(libc::SYS_write as u64, [1, 0x100, 4, 0, 0, 0]);
        assert!(
            !ProcessExecutionContext::SignalBoundary(boundary.clone())
                .tail_injection_allowed(&request)
        );
        assert!(
            ProcessExecutionContext::SyscallBoundary(boundary.clone())
                .tail_injection_allowed(&request)
        );
        assert!(ProcessExecutionContext::Lifecycle.tail_injection_allowed(&request));'''
assert s.count(old)==1;s=s.replace(old,new)
assert 'tail_injection_allowed()' not in s
p.write_text(s)
print('Prepared isolated F1/F2 production changes; no compile or test execution')
