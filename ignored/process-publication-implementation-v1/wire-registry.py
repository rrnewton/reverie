from pathlib import Path
p=Path(__file__).resolve().parents[2]/'reverie-kvm/src/executor.rs';s=p.read_text()
s=s.replace('const MAX_HOST_IO:', '#[path = "process_signal_publication.rs"]\nmod process_signal_publication;\n\nuse process_signal_publication::ProcessBinding;\nuse process_signal_publication::ProcessSignalRegistry;\n\nconst MAX_HOST_IO:',1)
s=s.replace('    process_generation: u64,\n    address_space:', '    process_generation: u64,\n    signal_registry: Arc<ProcessSignalRegistry>,\n    signal_binding: Arc<ProcessBinding>,\n    address_space:',1)
def edit(name,fn):
 global s
 import re
 start=re.search(r'    (?:pub\(crate\) )?fn '+name+r'\(',s).start();b=s.index('{',start);end=s.index('\n    }',b)+6
 s=s[:start]+fn(s[start:end])+s[end:]
def new(part):
 part=part.replace('        let task_generation =', '        let transaction = state.signal_transaction.clone();\n        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());\n        let task_generation =',1)
 part=part.replace('        let (child_completion_sender,', '        let signal_registry = Arc::new(ProcessSignalRegistry::default());\n        let signal_binding = signal_registry.register(&state, &file_table, process_generation, None);\n        let (child_completion_sender,',1)
 return part.replace('            process_generation,', '            process_generation,\n            signal_registry,\n            signal_binding,',1)
# There are other new constructors; target ElfExecutor explicitly.
start=s.index('impl ElfExecutor {');pre=s[:start];tail=s[start:];s=tail;edit('new',new);s=pre+s
def fork(part):
 part=part.replace('        let (child_completion_sender,','        let signal_binding = self.signal_registry.register(&state, &file_table, process_generation,\n            Some(reverie::SignalProcessId { tgid: reverie::Pid::from_raw(self.state.pid), generation: self.process_generation }));\n        let (child_completion_sender,',1)
 return part.replace('            process_generation,','            process_generation,\n            signal_registry: self.signal_registry.clone(),\n            signal_binding,',1)
edit('fork_child',fork)
edit('thread_child_with_signal_observation',lambda part:part.replace('            process_generation,','            process_generation,\n            signal_registry: self.signal_registry.clone(),\n            signal_binding: self.signal_binding.clone(),',1))
def exec_(part):
 part=part.replace('        let previous =','        let file_table = self.file_table.clone();\n        let mut files = file_table.lock().expect("KVM file-table lock poisoned");\n        let transaction = self.state.signal_transaction.clone();\n        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());\n        let previous =',1)
 part=part.replace('.inherit_process_state(previous)', '.inherit_process_state_locked(previous)')
 a=part.index('        *self\n            .file_table');b=part.index('        self.sigchld_auto_reap',a)
 return part[:a]+'        *files = FileTableState::try_from_elf(&self.state).expect("clone post-exec KVM file table");\n        self.signal_binding.rebind(&self.state, &file_table);\n'+part[b:]
edit('replace_after_exec',exec_)
edit('release_files_on_exit',lambda part:part.replace('{','{\n        let files = self.file_table.clone();\n        let _files = files.lock().unwrap_or_else(|p| p.into_inner());\n        let transaction = self.state.signal_transaction.clone();\n        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());',1))
p.write_text(s)
p=p.parent/'elf.rs';s=p.read_text();pos=s.index('    pub(crate) fn has_live_sibling')
s=s[:pos]+'''    pub(crate) fn contains_process(&self, tgid: i32, generation: u64) -> bool {
        self.tasks.values().any(|task| task.tgid == tgid && task.process_generation == generation)
    }

'''+s[pos:];p.write_text(s)
