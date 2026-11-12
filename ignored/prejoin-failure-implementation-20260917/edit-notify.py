from pathlib import Path
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text();s=s.replace('async fn notify_tool_exit<T: Tool>(', '#[allow(clippy::too_many_arguments)]\nasync fn notify_tool_exit<T: Tool>(',1)
a=s.index('async fn notify_tool_exit<T: Tool>(');b=s.index('/// Result of a Tool run',a);chunk=s[a:b].replace('    exit: ToolExit,\n','    exit: ToolExit,\n    failure: Option<&FailureContext>,\n',1)
chunk=chunk.replace('''        .map_err(Error::Reverie);''','''        .map_err(|error| match failure {
            Some(failure) => failure.publish("thread exit hook", Error::Reverie(error)),
            None => {
                global_state.report_backend_failure(reverie::BackendFailure { pid, tid, phase: "thread exit hook" });
                Error::Reverie(error)
            }
        });''',1)
# This error belongs to the consuming process hook, after the thread hook has
# been reported. The caller retains it as secondary when necessary.
chunk=chunk.replace('''    match (thread_result, process_result) {''','''    let process_result = process_result.map_err(|error| match failure {
        Some(failure) => failure.publish("process exit hook", error),
        None => {
            global_state.report_backend_failure(reverie::BackendFailure { pid, tid, phase: "process exit hook" });
            error
        }
    });
    match (thread_result, process_result) {''',1)
s=s[:a]+chunk+s[b:]
# Two backend free-helper calls, distinct from methods named the same.
for marker in ['''                process_exited: pid == tid,
            },
        )''']:
 s=s.replace(marker,'''                process_exited: pid == tid,
            },
            self.tool_failure.as_ref(),
        )''')
p.write_text(s)
p=Path('reverie-kvm/src/runtime/failure_tests.rs');s=p.read_text();s=s.replace('ToolExit { status, process_exited: false }));','ToolExit { status, process_exited: false }, None));').replace('ToolExit { status, process_exited: true })).unwrap();','ToolExit { status, process_exited: true }, None)).unwrap();');p.write_text(s)
p=Path('reverie-kvm/src/vm.rs');s=p.read_text();a=s.index('                let handle = crate::failure::spawn_owned(');b=s.index('                pending_child_starts',a);chunk=s[a:b].replace('.map_err(Error::Reverie)?;', '.map_err(|error| child.backend.report_tool_failure("child wait hook", Error::Reverie(error)))?;',1);s=s[:a]+chunk+s[b:];p.write_text(s)
