from pathlib import Path
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text();anchor='    /// Releases a worker\'s reusable slot before its exit becomes visible to\n';s=s.replace(anchor,'''    /// Consume a constructed child whose start gate was cancelled or whose
    /// host spawn failed. Do not manufacture handle_thread_start or admission.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_unstarted_tool<T: Tool>(
        &mut self,
        executor: &mut ElfExecutor,
        tool: Arc<T>,
        identity: (Pid, Pid),
        global_state: &T::GlobalState,
        config: &<T::GlobalState as GlobalTool>::Config,
        thread_state: T::ThreadState,
        failure: Option<Error>,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let outcome = match failure {
            Some(error) => Err(error),
            None => Ok(self.cancelled_tool_thread_status(executor)),
        };
        self.finish_tool_process(executor, tool, identity, global_state, config, thread_state, outcome).await
    }

'''+anchor,1)
# Associate every child with its actual identity before its first fallible setup.
a=s.index('    pub(crate) async fn run_static_elf_process_with_tool<T>(');marker='        executor.observe_ignored_signals_with_tool();';i=s.index(marker,a);s=s[:i]+'''        if let Some(failure) = &self.tool_failure {
            self.tool_failure = Some(FailureContext::new(failure.run.clone(), pid, tid));
        }
'''+s[i:];p.write_text(s)
p=Path('reverie-kvm/src/vm.rs');s=p.read_text()
# Move child init after all reorderable fallible operations.
a=s.index('    async fn run_process_action_with_tool_inner<T>');fork=s.index('                let child_pid = Pid::from_raw(child.pid);',a);init=s.index('                let child_tool =',fork);glob=s.index('                let global_state = context.global_state',init);globend=s.index('                let config =',glob);construct=s[init:glob];s=s[:init]+s[glob:globend]+construct+s[globend:]
# Bind child's failure identity before ownership enters the spawn.
marker='                let child_thread_state = child_tool\n                    .init_thread_state(child_pid, Some((context.tid, context.thread_state)));';s=s.replace(marker,marker+'''
                if let Some(failure) = &child.backend.tool_failure {
                    child.backend.tool_failure = Some(crate::failure::FailureContext::new(failure.run.clone(), child_pid, child_pid));
                }''',1)
# Fork spawn owns a recoverable tuple rather than irretrievably moving state.
a=s.index('                let handle = std::thread::Builder::new()',fork);end=s.index('                pending_child_starts\n',a);chunk=s[a:end]
chunk=chunk.replace('''                let handle = std::thread::Builder::new()
                    .name(format!("reverie-kvm-process-{raw_child_pid}"))
                    .spawn(move || {''','''                let handle = crate::failure::spawn_owned(
                    std::thread::Builder::new().name(format!("reverie-kvm-process-{raw_child_pid}")),
                    (child, child_tool, child_thread_state, global_state, config, subscriptions),
                    move |(mut child, child_tool, child_thread_state, global_state, config, subscriptions)| {''',1)
chunk=chunk.replace('''                        match start_receiver.recv() {
                            Ok(ChildStartCommand::Start) => {}
                            Ok(ChildStartCommand::Cancel) => return Ok(()),
                            Err(_) => {
                                return Err(Error::UnexpectedVcpuExit(format!(
                                    "KVM child process {raw_child_pid} lost its parent start gate"
                                )));
                            }
                        }
                        let result = futures::executor::block_on(
                            child.backend.run_static_elf_process_with_tool(''','''                        let start = start_receiver.recv();
                        let failure = match start {
                            Ok(ChildStartCommand::Start) => None,
                            Ok(ChildStartCommand::Cancel) => Some(Error::RunAborted),
                            Err(_) => Some(Error::UnexpectedVcpuExit(format!(
                                "KVM child process {raw_child_pid} lost its parent start gate"
                            ))),
                        };
                        let result = if let Some(error) = failure {
                            futures::executor::block_on(child.backend.finish_unstarted_tool(
                                &mut child.executor, child_tool, (child_pid, child_pid),
                                global_state.as_ref(), &config, child_thread_state, Some(error),
                            ))
                        } else { futures::executor::block_on(
                            child.backend.run_static_elf_process_with_tool(''',1)
chunk=chunk.replace('''                        );
                        match result {''','''                        ) };
                        match result {''',1)
chunk=chunk.replace('''                    })?;
''','''                    });
                let handle = match handle {
                    Ok(handle) => handle,
                    Err((error, (mut child, child_tool, child_thread_state, global_state, config, _))) => {
                        return child.backend.finish_unstarted_tool(
                            &mut child.executor, child_tool, (child_pid, child_pid), global_state.as_ref(),
                            &config, child_thread_state, Some(Error::HostIo(error)),
                        ).await.map(|_| unreachable!("failed spawn completed successfully"));
                    }
                };
''',1)
s=s[:a]+chunk+s[end:]
# Thread setup: restore parent frame and validate GlobalState before constructing ThreadState.
a=s.index('                let tgid = context.pid;',a);b=s.index('                let pending_child_starts = context.pending_child_starts;',a);chunk=s[a:b];initstart=chunk.index('                let child_tool =');initend=chunk.index('                let global_state =',initstart);construct=chunk[initstart:initend];chunk=chunk[:initstart]+chunk[initend:]+construct+'''                if let Some(failure) = &child.tool_failure {
                    child.tool_failure = Some(crate::failure::FailureContext::new(failure.run.clone(), tgid, child_tid_pid));
                }
''';s=s[:a]+chunk+s[b:]
a=s.index('                let handle = std::thread::Builder::new()',a);end=s.index('                self.thread_group\n',a);chunk=s[a:end]
chunk=chunk.replace('''                let handle = std::thread::Builder::new()
                    .name(format!("reverie-kvm-guest-{child_tid}"))
                    .spawn(move || {''','''                let handle = crate::failure::spawn_owned(
                    std::thread::Builder::new().name(format!("reverie-kvm-guest-{child_tid}")),
                    (child, child_executor, child_tool, child_thread_state, global_state, config, subscriptions),
                    move |(mut child, mut child_executor, child_tool, child_thread_state, global_state, config, subscriptions)| {''',1)
start=chunk.index('                            match start_receiver.recv() {');last=chunk.index('                            child.release_thread_slot();',start)
chunk=chunk[:start]+'''                            let start = start_receiver.recv();
                            let cancel = !matches!(start, Ok(ChildStartCommand::Start));
                            let failure = match start {
                                Ok(_) => None,
                                Err(_) => Some(Error::UnexpectedVcpuExit(format!(
                                    "KVM guest thread {child_tid} lost its parent start gate"
                                ))),
                            };
                            let result = if cancel {
                                let failure = failure.or_else(|| child.tool_failure.as_ref()
                                    .and_then(|failure| failure.run.primary()).map(Error::SharedFailure));
                                futures::executor::block_on(child.finish_unstarted_tool(
                                    &mut child_executor, child_tool, (tgid, child_tid_pid), global_state.as_ref(),
                                    &config, child_thread_state, failure,
                                ))
                            } else {
                                futures::executor::block_on(child.run_static_elf_process_with_tool(
                                    &mut child_executor, tgid, child_tid_pid, child_tool, child_thread_state,
                                    global_state, &config, &subscriptions, false,
                                ))
                            };
'''+chunk[last:]
chunk=chunk.replace('''                    })?;
''','''                    });
                let handle = match handle {
                    Ok(handle) => handle,
                    Err((error, (mut child, mut child_executor, child_tool, child_thread_state, global_state, config, _))) => {
                        return child.finish_unstarted_tool(
                            &mut child_executor, child_tool, (tgid, child_tid_pid), global_state.as_ref(),
                            &config, child_thread_state, Some(Error::HostIo(error)),
                        ).await.map(|_| unreachable!("failed spawn completed successfully"));
                    }
                };
''',1)
s=s[:a]+chunk+s[end:];p.write_text(s)
