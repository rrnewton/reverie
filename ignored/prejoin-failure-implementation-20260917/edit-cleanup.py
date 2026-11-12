from pathlib import Path
p=Path('reverie-kvm/src/executor.rs');s=p.read_text();anchor='    pub(crate) fn join_all_child_processes(&mut self) -> crate::Result<()> {\n';s=s.replace(anchor,'''    pub(crate) fn join_all_child_processes(&mut self) -> crate::Result<()> {
        self.finish_child_processes(false)
    }

    pub(crate) fn join_child_processes_after_failure(&mut self) -> crate::Result<()> {
        self.finish_child_processes(true)
    }

    fn finish_child_processes(&mut self, failed: bool) -> crate::Result<()> {
''',1)
a=s.index('    fn finish_child_processes');b=s.index('    fn synchronize_wait4',a);chunk=s[a:b]
chunk=chunk.replace('''            if process.start.start().is_err() {''','''            if failed {
                process.start.cancel();
            } else if process.start.start().is_err() {''',1);s=s[:a]+chunk+s[b:];p.write_text(s)
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text();s=s.replace('''        let children = if pid == tid {
            executor.join_all_child_processes()
''','''        let children = if pid == tid {
            if outcome.is_err() || owner.is_err() || workers.is_err() || process_status.is_err() {
                executor.join_child_processes_after_failure()
            } else {
                executor.join_all_child_processes()
            }
''',1)
# The direct transport loop also has a consuming scope for setup and callback errors.
a=s.index('    pub async fn run_with_tool<T, E>');b=s.index('    /// Runs an installed static ELF through',a);chunk=s[a:b]
# Existing early consuming paths become returns from the inner operation.
import re
pattern=r'\s*self\.notify_tool_exit\(\s*tool,\s*\(pid, pid\),\s*&global_state,\s*&config,\s*thread_state,\s*(?:ExitStatus::SUCCESS|status),\s*\)\s*\.await\?;\s*return Ok\(global_state\);'
chunk,n=re.subn(pattern,'\n                    return Ok(ExitStatus::SUCCESS);',chunk);assert n==3,n
marker='        let registers = kvm_registers(self.vcpu.get_regs()?, 0);';chunk=chunk.replace(marker,'        let outcome: Result<ExitStatus> = async {\n'+marker,1)
# Hlt no longer needs local status binding.
chunk=chunk.replace('                    let status = ExitStatus::SUCCESS;\n','',1)
end=chunk.rindex('    }');chunk=chunk[:end]+'''        }.await;
        if outcome.is_err() {
            global_state.report_backend_failure(reverie::BackendFailure {
                pid, tid: pid, phase: "direct Tool execution",
            });
        }
        let status = outcome.as_ref().copied().unwrap_or(ExitStatus::Exited(255));
        let cleanup = self.notify_tool_exit(tool, (pid, pid), &global_state, &config, thread_state, status).await;
        Error::combine([outcome.map(|_| ()), cleanup].into_iter().filter_map(Result::err).collect())?;
        Ok(global_state)
'''+chunk[end:];s=s[:a]+chunk+s[b:];p.write_text(s)
