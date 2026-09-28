from pathlib import Path
import difflib
D=Path(__file__).resolve().parent
p=D/'BASE-runtime.rs';base=p.read_text();s=base
old='''            ProcessExecutionContext::SignalBoundary(CompletedSyscallBoundary::capture(
                self,
                frame_address,
                Some(registers),
            )?)'''
new='''            let mut stop = pin!(wait_for_failure(
                global_state.as_ref(),
                failure_subscription.clone(),
            ));
            let Some(boundary) = CompletedSyscallBoundary::capture_admitted(
                self,
                frame_address,
                Some(registers),
                stop.as_mut(),
            )
            .await?
            else {
                hide_tool_scratch(memory, tool_stack_top)?;
                return Ok(CallbackOutcome::ThreadCancelled);
            };
            ProcessExecutionContext::SignalBoundary(boundary)'''
assert s.count(old)==1;s=s.replace(old,new)
old='''                        let boundary =
                            CompletedSyscallBoundary::capture(self, frame_address, None)?;'''
new='''                        let boundary = {
                            let mut stop = pin!(wait_for_failure(
                                global_state.as_ref(),
                                failure_subscription.clone(),
                            ));
                            CompletedSyscallBoundary::capture_admitted(
                                self, frame_address, None, stop.as_mut(),
                            )
                            .await?
                        };
                        let Some(boundary) = boundary else {
                            hide_tool_scratch(&memory, tool_stack_top)?;
                            return Ok(self.cancelled_tool_thread_status(executor));
                        };'''
assert s.count(old)==1;s=s.replace(old,new)
old='''                    let continuation = CompletedSyscallBoundary::capture_for_action(
                        self,
                        frame_address,
                        None,
                        &action,
                    )?;'''
new='''                    let continuation = {
                        let mut stop = pin!(wait_for_failure(
                            global_state.as_ref(),
                            failure_subscription.clone(),
                        ));
                        CompletedSyscallBoundary::capture_for_action_admitted(
                            self, frame_address, None, &action, stop.as_mut(),
                        )
                        .await?
                    };
                    let Some(continuation) = continuation else {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    };'''
assert s.count(old)==1;s=s.replace(old,new)
(D/'PROPOSED-runtime.rs').write_text(s)
(D/'RUNTIME-CALLER.patch').write_text(''.join(difflib.unified_diff(base.splitlines(keepends=True),s.splitlines(keepends=True),fromfile='a/reverie-kvm/src/runtime.rs',tofile='b/reverie-kvm/src/runtime.rs')))
