from pathlib import Path
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text();s=s.replace('use crate::Error;','use crate::Error;\nuse crate::failure::{FailureContext, RunFailure, wait_for_failure};',1)
s=s.replace('enum HandlerOutcome<T> {\n    Returned(T),','enum HandlerOutcome<T> {\n    Returned(T),\n    RunFailed,',1)
a=s.index('async fn drive_handler<T>(');b=s.index('\npub(crate) fn start_pending_children',a)
old=s[a:b];new=old.replace('    pending_child_starts: SharedChildStarts,\n','    pending_child_starts: SharedChildStarts,\n    failure: impl Future<Output = ()>,\n',1).replace('    poll_fn(|context| match future.as_mut().poll(context) {\n        Poll::Ready(result) => Poll::Ready(HandlerOutcome::Returned(result)),','''    let mut failure = pin!(failure);
    poll_fn(|context| {
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(HandlerOutcome::RunFailed);
        }
        let result = future.as_mut().poll(context);
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(HandlerOutcome::RunFailed);
        }
        match result {
        Poll::Ready(result) => Poll::Ready(HandlerOutcome::Returned(result)),''',1).replace('    })\n    .await','    }})\n    .await',1);s=s[:a]+new+s[b:]
# Add independent failure future to each actual caller. Clone the run subscription
# before borrowing the backend through the guest executor.
funcs=[('async fn run_post_exec_handler<T>', 'backend', 'global_state.as_ref()'),('async fn run_initial_exec_handler<T>', 'backend', 'global_state.as_ref()'),('pub async fn run_with_tool<T, E>', None, '&global_state'),('async fn filter_next_pending_signal_with_tool<T>', 'self', 'global_state.as_ref()'),('pub(crate) async fn run_static_elf_process_with_tool<T>', 'self', 'global_state.as_ref()')]
for name,backend,globalref in reversed(funcs):
 a=s.index(name);b=s.find('\n    }\n',a)+7 if name.startswith(('pub async','pub(crate)','async fn filter')) else s.find('\n}\n',a)+3
 chunk=s[a:b]
 if backend:
  marker='    let tool_stack_top = '+backend+'.tool_stack_top();'
  assert marker in chunk,name
  chunk=chunk.replace(marker,marker+'\n    let failure_subscription = '+backend+'.failure_subscription();',1)
 expr=f'wait_for_failure({globalref}, '+('failure_subscription.clone()' if backend else 'None')+')'
 # Locate matching call paren, then insert a fourth argument.
 start=0
 while (i:=chunk.find('drive_handler(',start))>=0:
  j=i+len('drive_handler(');depth=1
  while depth:
   depth += (chunk[j]=='(')-(chunk[j]==')');j+=1
  close=j-1;indent=' '*(len(chunk[:close].split('\n')[-1]));chunk=chunk[:close]+expr+',\n'+indent+chunk[close:];start=close+len(expr)+3
 s=s[:a]+chunk+s[b:]
# Tests retain their prior never-failed setup.
pos=s.index('#[cfg(test)]\nmod tests')
before,tests=s[:pos],s[pos:]
start=0
while (i:=tests.find('drive_handler(',start))>=0:
 j=i+len('drive_handler(');depth=1
 while depth:
  depth+=(tests[j]=='(')-(tests[j]==')');j+=1
 close=j-1;tests=tests[:close]+'std::future::pending(),\n'+tests[close:];start=close+len('std::future::pending(),\n')+1
# All production RunFailed branches return the distinct fatal error. Callers
# that own pending children use the existing pre-join cleanup path below.
before=before.replace('HandlerOutcome::RuntimeError(error) =>', 'HandlerOutcome::RunFailed => return Err(Error::RunAborted),\n            HandlerOutcome::RuntimeError(error) =>')
# initial exec uses an expression match instead of a return statement (return is fine).
tests=tests.replace('HandlerOutcome::Returned(_) => panic!("tail injection unexpectedly returned"),','HandlerOutcome::Returned(_) => panic!("tail injection unexpectedly returned"),\n            HandlerOutcome::RunFailed => panic!("unexpected run failure"),')
s=before+tests
p.write_text(s)
p=Path('reverie-kvm/src/vm.rs');s=p.read_text();s=s.replace('    thread_group: Arc<GuestThreadGroup>,\n    thread_slot:', '    thread_group: Arc<GuestThreadGroup>,\n    pub(crate) tool_failure: Option<crate::failure::FailureContext>,\n    thread_slot:',1).replace('            thread_group: Arc::new(GuestThreadGroup::default()),','            thread_group: Arc::new(GuestThreadGroup::default()),\n            tool_failure: None,',1)
s=s.replace('        child.thread_ownership = self.thread_ownership;','        child.thread_ownership = self.thread_ownership;\n        child.tool_failure = self.tool_failure.clone();')
anchor='impl KvmBackend {\n';s=s.replace(anchor,anchor+'''    pub(crate) fn failure_subscription(&self) -> Option<crate::failure::FailureSubscription> {
        self.tool_failure.as_ref().map(|failure| failure.run.subscribe())
    }

    pub(crate) fn report_tool_failure(&self, phase: &'static str, error: Error) -> Error {
        self.tool_failure.as_ref().map_or_else(|| error, |failure| failure.publish(phase, error))
    }

''',1)
# map_or_else cannot move the same value into both closures.
s=s.replace('        self.tool_failure.as_ref().map_or_else(|| error, |failure| failure.publish(phase, error))','        match &self.tool_failure {\n            Some(failure) => failure.publish(phase, error),\n            None => error,\n        }')
p.write_text(s)
