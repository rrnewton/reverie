from pathlib import Path
r=Path(__file__).resolve().parents[2]
p=r/'reverie-kvm/src/executor.rs';s=p.read_text()
def body(name, method=False):
 global s
 needle=('    ' if method else '')+'fn '+name+'('
 if method:
  import re
  m=re.search(r'    (?:pub\(crate\) )?fn '+name+r'\(',s);assert m,name;start=m.start()
 else:start=s.index(needle)
 b=s.index('{',start); end=s.index('\n    }' if method else '\n}',b)+ (6 if method else 2)
 return b,end

def addguard(name, method=False):
 global s
 b,e=body(name,method); prefix='self.state' if method else 'state'; indent='        ' if method else '    '
 text=f'\n{indent}let transaction = {prefix}.signal_transaction.clone();\n{indent}let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());'
 s=s[:b+1]+text+s[b+1:]

s=s.replace('        process_signals: Arc::new(std::sync::Mutex::new(ProcessSignalState::default())),','        process_signals: Arc::new(std::sync::Mutex::new(ProcessSignalState::default())),\n        signal_transaction: Default::default(),')
s=s.replace('        state.process_signals = self.state.process_signals.clone();','        state.process_signals = self.state.process_signals.clone();\n        state.signal_transaction = self.state.signal_transaction.clone();')
# Caller-owned transactions span actual removal/enqueue and readiness I/O.
for name in ['queue_child_exit_signal','queue_process_alarm_signal','prepare_filtered_signal_delivery','take_pending_signal_for_delivery','observe_ignored_signals_with_tool','prepare_captured_page_zero_fault','enter_signal_handler','restore_signal_thread_state','enable_signal_dequeues','take_exit','retire_failed_thread']:
 addguard(name,True)
for name in ['signalfd','rt_sigaction','rt_sigprocmask','sigaltstack','rt_sigtimedwait','kill_signal','duplicate_fd','duplicate_fd_at_or_above','close','take_signalfd_event']:
 addguard(name)
# Functions with several callers get explicit locking wrappers plus locked bodies.
for name,signature,call in [
 ('queue_signal_event','state: &mut LoadedStaticElf, event: reverie::SignalEvent, process_directed: bool','state, event, process_directed'),
 ('take_signal_event','state: &mut LoadedStaticElf, selection: SignalSelection','state, selection'),
 ('refresh_all_signalfd_readiness','state: &LoadedStaticElf','state')]:
 b,e=body(name);start=s.rfind('\nfn ',0,b)+1;header=s[start:b]
 wrapper=header+'{\n    let transaction = state.signal_transaction.clone();\n    let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());\n    '+name+'_locked('+call+')\n}\n\n'
 s=s[:start]+wrapper+s[start:].replace('fn '+name+'(', 'fn '+name+'_locked(',1)
# Calls from transaction-owning functions use locked implementations.
for name,method in [('prepare_filtered_signal_delivery',True),('take_pending_signal_for_delivery',True),('take_signalfd_event',False),('rt_sigtimedwait',False),('kill_signal',False),('send_thread_signal',False),('refresh_all_signalfd_readiness_locked',False)]:
 b,e=body(name,method);chunk=s[b:e]
 for callee in ['queue_signal_event','take_signal_event','refresh_all_signalfd_readiness','refresh_signalfd_readiness_for_signal']:
  chunk=chunk.replace(callee+'(',callee+'_locked(')
 if name=='take_pending_signal_for_delivery':
  chunk=chunk.replace('self.refresh_signalfd_readiness()?;', 'refresh_all_signalfd_readiness_locked(&self.state)\n            .map_err(|raw| reverie::syscalls::Errno::new(i32::try_from(-raw).unwrap_or(libc::EIO)))?;')
 s=s[:b]+chunk+s[e:]
s=s.replace('fn refresh_signalfd_readiness_for_signal(', 'fn refresh_signalfd_readiness_for_signal_locked(')
# In production take_signal_event is always paired with readiness under one guard.
s=s.replace('fn take_signal_event(\n', '#[cfg(test)]\nfn take_signal_event(\n',1)
# Destruction and group creation serialize with publication; cloning helpers below
# own no second guard when called from these enclosing transitions.
for name in ['fork_child','thread_child_with_signal_observation']:
 addguard(name,True);b,e=body(name,True);s=s[:b]+s[b:e].replace('.try_clone_for_fork(','.try_clone_for_fork_locked(')+s[e:]
b=s.index('    fn drop(&mut self) {', s.index('impl Drop for ElfExecutor'))+len('    fn drop(&mut self) {')
s=s[:b]+'\n        let transaction = self.state.signal_transaction.clone();\n        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());'+s[b:]
p.write_text(s)
p=r/'reverie-kvm/src/elf.rs';s=p.read_text()
for name,args,call in [('try_clone_for_fork','&self, child_pid: i32','child_pid'),('inherit_process_state','&mut self, previous: Self','previous')]:
 start=s.index('    pub(crate) fn '+name+'(');b=s.index('{',start);header=s[start:b];source='self' if name.startswith('try') else 'previous'
 wrapper=header+'{\n        let transaction = '+source+'.signal_transaction.clone();\n        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());\n        self.'+name+'_locked('+call+')\n    }\n\n'
 s=s[:start]+wrapper+s[start:].replace('fn '+name+'(', 'fn '+name+'_locked(',1)
p.write_text(s)
