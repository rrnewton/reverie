from pathlib import Path
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text()
# Preserve the execution result while returning GlobalState to its owner.
a=s.index('    pub async fn run_static_elf_with_tool<T>(');b=s.index('    #[allow(clippy::too_many_arguments)]',a);old=s[a:b]
wrapper=old[:old.index('    {')+5]+'''
        let completion = self.run_static_elf_with_tool_completion::<T>(config, capture_output).await?;
        let (status, stdout, stderr) = completion.result?;
        Ok((completion.global_state, status, stdout, stderr))
    }

    /// Runs the installed ELF and retains GlobalState even after a runtime
    /// failure, so its owner can finish scheduler/global cleanup. An outer
    /// error means setup failed before the global state existed, or ownership
    /// could not be recovered after all owned children were joined.
'''
body=old.replace('pub async fn run_static_elf_with_tool<T>','pub async fn run_static_elf_with_tool_completion<T>',1).replace('Result<(T::GlobalState, i32, Vec<u8>, Vec<u8>)>','Result<ToolRunCompletion<T::GlobalState>>',1)
body=body.replace('        let tool = Arc::new(T::new(pid, &config));','''        let failure = RunFailure::new(&global_state);
        self.tool_failure = Some(FailureContext::new(failure.clone(), pid, pid));
        let tool = Arc::new(T::new(pid, &config));''',1).replace('            .await?;\n        let global_state', '            .await;\n        let global_state',1)
body=body.replace('''        let (status, stdout, stderr) = result;
        Ok((global_state, conventional_exit_code(status), stdout, stderr))''','''        let result = match (failure.primary(), result) {
            (Some(primary), Err(error)) => Err(Error::SharedFailure(primary).with_cleanup(vec![error])),
            (Some(primary), Ok(_)) => Err(Error::SharedFailure(primary)),
            (None, result) => result.map(|(status, stdout, stderr)| (conventional_exit_code(status), stdout, stderr)),
        };
        Ok(ToolRunCompletion { global_state, result })''',1)
s=s[:a]+wrapper+body+s[b:]
anchor='impl KvmBackend {\n';s=s.replace(anchor,'''/// Result of a Tool run after owned workers and children have completed.
pub struct ToolRunCompletion<G> {
    /// Global Tool state, retained on runtime failure for consuming cleanup.
    pub global_state: G,
    /// Guest output/status, or a typed fatal runtime/Tool cause.
    pub result: Result<(i32, Vec<u8>, Vec<u8>)>,
}

'''+anchor,1)
# Bring all post-construction setup into the same consuming scope.
a=s.index('    pub(crate) async fn run_static_elf_process_with_tool<T>(');b=s.index('\n}\n',a);chunk=s[a:b]
chunk=chunk.replace('        self.vcpu.track_clock()?;\n','',1).replace('        let _registration = self.register_guest_thread()?;','        let mut registration = None;',1).replace('        let registers = kvm_registers(self.vcpu.get_regs()?, 0);\n','',1).replace('        expose_tool_scratch(&memory, tool_stack_top)?;\n','',1)
chunk=chunk.replace('        let outcome: Result<ToolProcessExit> = async {','''        let outcome: Result<ToolProcessExit> = async {
            self.vcpu.track_clock()?;
            registration = Some(self.register_guest_thread()?);
            let registers = kvm_registers(self.vcpu.get_regs()?, 0);
            expose_tool_scratch(&memory, tool_stack_top)?;''',1)
# Any error before common finish must cancel pending children, never start them.
chunk=chunk.replace('''        self.finish_tool_process(
''','''        let outcome = outcome.map_err(|error| {
            self.cleanup_unstarted_tool_children_after_error(executor, &pending_child_starts, error)
        });
        self.finish_tool_process(
''',1)
s=s[:a]+chunk+s[b:]
# Finisher publication precedes both physical joins and consuming hooks.
a=s.index('    async fn finish_tool_process<T: Tool>');b=s.index('    /// Releases a worker',a);chunk=s[a:b]
chunk=chunk.replace('        let natural_exit = outcome','        let outcome = outcome.map_err(|error| self.report_tool_failure("execution", error));\n        let natural_exit = outcome',1)
chunk=chunk.replace('''        let children = if pid == tid {''','''        let owner = owner.map_err(|error| self.report_tool_failure("owner exit", error));
        let children = if pid == tid {''',1)
start=chunk.index('        let mut errors = Vec::new();');chunk=chunk[:start]+'''        let errors = [outcome.map(|_| ()), workers, process_status, owner, children]
            .into_iter().filter_map(Result::err).collect();
        Error::combine(errors)?;
        let (stdout, stderr) = executor.take_output();
        Ok((status, stdout, stderr))
    }

'''
s=s[:a]+chunk+s[b:]
s=s.replace('''        (Err(thread), Err(process)) => Err(Error::UnexpectedVcpuExit(format!(
            "KVM owner thread exit failed: {thread}; owner process exit failed: {process}"
        ))),''','''        (Err(thread), Err(process)) => Err(thread.with_cleanup(vec![process])),''',1)
s=s.replace('''            (Err(error), Err(workers)) => Err(Error::UnexpectedVcpuExit(format!(
                "KVM owner exit failed: {error}; {workers}"
            ))),''','''            (Err(error), Err(workers)) => Err(error.with_cleanup(vec![workers])),''',1)
# All callback-local starts must be cancelled before their guards drop on fatal failure.
# Plain RunFailed arms in free helper functions need the local owning backend.
for name,backend in [('async fn run_post_exec_handler<T>','backend'),('async fn run_initial_exec_handler<T>','backend'),('async fn filter_next_pending_signal_with_tool<T>','self')]:
 a=s.index(name); b=s.find('\n}\n',a)+3 if backend=='backend' else s.find('\n    }\n',a)+7
 chunk=s[a:b].replace('HandlerOutcome::RunFailed => return Err(Error::RunAborted),',f'HandlerOutcome::RunFailed => return Err({backend}.cleanup_unstarted_tool_children_after_error(executor, &pending_child_starts, Error::RunAborted)),')
 s=s[:a]+chunk+s[b:]
p.write_text(s)
p=Path('reverie-kvm/src/vm.rs');s=p.read_text();s=s.replace('worker_errors: Mutex<std::collections::BTreeMap<i32, Vec<String>>>','worker_errors: Mutex<std::collections::BTreeMap<i32, Vec<Arc<Error>>>>',1).replace('Ok(Err(error)) => Some(error.to_string()),','Ok(Err(error)) => Some(Arc::new(error)),',1).replace('Err(_) => Some("guest thread panicked during teardown".to_owned()),','Err(_) => Some(Arc::new(Error::UnexpectedVcpuExit("guest thread panicked during teardown".to_owned()))),',1)
a=s.index('        let diagnostics = errors\n',s.index('    fn teardown_result'));b=s.index('\n    fn cancel_workers',a)
s=s[:a]+'''        Error::combine(errors.values().flatten().cloned().map(Error::SharedFailure).collect())
    }
'''+s[b:]
s=s.replace('''        let result = match self.discard_unstarted_tool_children(executor, starts) {''','''        let primary = self.report_tool_failure("Tool callback", primary);
        let result = match self.discard_unstarted_tool_children(executor, starts) {''',1).replace('''            Err(cleanup) => Error::UnexpectedVcpuExit(format!(
                "KVM Tool callback failed: {primary}; unstarted-child cleanup also failed: {cleanup}"
            )),''','''            Err(cleanup) => primary.with_cleanup(vec![cleanup]),''',1)
p.write_text(s)
p=Path('reverie-kvm/src/executor.rs');s=p.read_text();a=s.index('        match errors.len() {',s.index('    pub(crate) fn join_all_child_processes'));b=s.index('\n    fn synchronize_wait4',a);s=s[:a]+'''        crate::Error::combine(errors)
    }
'''+s[b:];p.write_text(s)
