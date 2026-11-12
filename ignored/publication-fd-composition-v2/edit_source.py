"""Prepare isolated combined candidate; no live source or tool execution."""
from pathlib import Path
D=Path(__file__).resolve().parent
OLD=D.parent/'publication-fd-composition-v1/source'
def edit(path):
 global s,where
 where=path;s=(OLD/path).read_text()
def rep(a,b,n=1):
 global s
 assert s.count(a)==n,(where,a,s.count(a),n);s=s.replace(a,b)
def save():
 (D/'source'/where).write_text(s)
edit('reverie-kvm/src/elf.rs')
rep('#[derive(Debug)]\npub(crate) struct LoadedStaticElf {',(D/'retirement-types.rs').read_text()+'#[derive(Debug)]\npub(crate) struct LoadedStaticElf {')
rep('    pub files: std::collections::BTreeMap<i32, std::fs::File>,\n','    pub files: std::collections::BTreeMap<i32, std::fs::File>,\n    pub file_retirement: FileRetirement,\n')
rep('    // Lock order: file table, transaction, lifecycle, process, thread.', '''    // Ordinary partial order: file table, transaction, lifecycle, process, thread.
    // The inactive publisher takes the run failure guard after lifecycle and
    // before process signals, and retains it through readiness I/O. Initial
    // registry/image lookups release their guards before the file table; image
    // validation and child lookup briefly reacquire them below transaction and
    // lifecycle respectively, releasing them before failure/process acquisition.''')
rep('    /// Return the replaced description so callers holding signal_transaction\n    /// can release that guard before the final host close.', '    /// Return the replaced description so callers can retire it after both\n    /// the authoritative file-table and signal-transaction guards are released.')
rep('    pub(crate) fn try_clone_for_fork(&self, child_pid: i32) -> Result<Self> {\n', '    pub(crate) fn try_clone_for_fork(&self, child_pid: i32) -> Result<Self> {\n        let _retirement = self.file_retirement.hold();\n')
rep('.map(|(&fd, file)| Ok((fd, file.try_clone()?)))\n            .collect::<Result<_>>()?;', '.map(|(&fd, file)| Ok((fd, self.file_retirement.stage_clone(file)?)))\n            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;\n        let cwd_fd = self.file_retirement.stage_clone(&self.cwd_fd)?;\n        let stdin = self\n            .stdin\n            .as_ref()\n            .map(|file| self.file_retirement.stage_clone(file))\n            .transpose()?;')
rep('            cwd_fd: self.cwd_fd.try_clone()?,\n            stdin: self\n                .stdin\n                .as_ref()\n                .map(std::fs::File::try_clone)\n                .transpose()?,','            cwd_fd: cwd_fd.into_file(),\n            stdin: stdin.map(StagedFile::into_file),')
rep('            files,\n            fd_entry_ids: self.fd_entry_ids.clone(),','            files: files.into_iter().map(|(fd, file)| (fd, file.into_file())).collect(),\n            file_retirement: FileRetirement::default(),\n            fd_entry_ids: self.fd_entry_ids.clone(),')
rep('    pub(crate) fn inherit_process_state(&mut self, previous: Self) {\n', '    pub(crate) fn inherit_process_state(&mut self, previous: Self) {\n        let _retirement = previous.file_retirement.hold();\n')
rep('        drop(_transaction);\n        drop(retired);\n', '        drop(_transaction);\n        self.file_retirement.retire(retired);\n')
rep('        let mut retired = Vec::new();\n        let mut stdin = previous.stdin;', '        let mut retired = Vec::new();\n        previous.file_retirement.retire_shared(previous.executable_file);\n        let mut stdin = previous.stdin;')
rep('        self.cwd_fd = previous.cwd_fd;\n        self.stdin = stdin;', '        retired.push(std::mem::replace(&mut self.cwd_fd, previous.cwd_fd));\n        retired.extend(std::mem::replace(&mut self.stdin, stdin));')
rep('        self.files = files;\n        self.fd_entry_ids = fd_entry_ids;', '        retired.extend(std::mem::replace(&mut self.files, files).into_values());\n        self.file_retirement = previous.file_retirement;\n        self.fd_entry_ids = fd_entry_ids;')
rep('        // The caller releases the transaction before final host close.', '        // The caller releases both descriptor and transaction guards before close.')
rep('        files: std::collections::BTreeMap::new(),\n        fd_entry_ids:', '        files: std::collections::BTreeMap::new(),\n        file_retirement: FileRetirement::default(),\n        fd_entry_ids:')
save()
edit('reverie-kvm/src/executor.rs')
# Staged clones retain every partial clone through the outer guard scope.
start=s.index('    fn try_from_elf(state: &LoadedStaticElf) -> std::io::Result<Self> {')
end=s.index('\n    // TODO-HUMAN-REVIEW(PR-235): Review stable host-fd preservation',start)
old=s[start:end]
new=old.replace('        Ok(Self {\n            stdin: state','        let stdin = state',1)
prefix='''    fn try_from_elf(state: &LoadedStaticElf) -> std::io::Result<Self> {
        let stdin = state
            .stdin
            .as_ref()
            .map(|file| state.file_retirement.stage_clone(file))
            .transpose()?;
        let files = state
            .files
            .iter()
            .map(|(&fd, file)| Ok((fd, state.file_retirement.stage_clone(file)?)))
            .collect::<std::io::Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(Self {
            stdin: stdin.map(crate::elf::StagedFile::into_file),
            files: files.into_iter().map(|(fd, file)| (fd, file.into_file())).collect(),
'''
remaining=old[old.index('            fd_entry_ids:'):]
rep(old,prefix+remaining)
# Table-owned File drops are handed to the same executor's outer retirement scope.
rep('impl FileTableState {\n','''impl FileTableState {
    fn retire(self, retirement: &crate::elf::FileRetirement) {
        retirement.retire(self.stdin.into_iter().chain(self.files.into_values()));
    }

''')
start=s.index('    fn install(&self, state: &mut LoadedStaticElf) -> std::io::Result<()> {');end=s.index('\n}\n\nfn mutates_file_table',start)
a=s[start:end];b=a.replace('.map(std::fs::File::try_clone)', '.map(|file| state.file_retirement.stage_clone(file))').replace('installed_files.insert(fd, shared_file.try_clone()?);','installed_files.insert(fd, state.file_retirement.stage_clone(shared_file)?);')
b=b.replace('        let mut previous_files = std::mem::take(&mut state.files);','        let mut installed_files: std::collections::BTreeMap<_, _> = installed_files\n            .into_iter()\n            .map(|(fd, file)| (fd, file.into_file()))\n            .collect();\n        let mut previous_files = std::mem::take(&mut state.files);')
b=b.replace('        state.stdin = stdin;\n        state.files = installed_files;', '        let retired_stdin = std::mem::replace(\n            &mut state.stdin,\n            stdin.map(crate::elf::StagedFile::into_file),\n        );\n        state.files = installed_files;\n        state.file_retirement.retire(retired_stdin.into_iter().chain(previous_files.into_values()));')
rep(a,b)
# Scope declarations must precede every guard whose destruction they follow.
rep('    pub(crate) fn new(mut state: LoadedStaticElf, capture_output: bool) -> Self {\n','    pub(crate) fn new(mut state: LoadedStaticElf, capture_output: bool) -> Self {\n        let file_table;\n        let _retirement = state.file_retirement.hold();\n')
rep('        let file_table = Arc::new(std::sync::Mutex::new(\n            FileTableState::try_from_elf(&state).expect("clone initial KVM file table"),', '        file_table = Arc::new(std::sync::Mutex::new(\n            FileTableState::try_from_elf(&state).expect("clone initial KVM file table"),')
rep('    fn execute_accept(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> Option<i64> {\n','    fn execute_accept(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> Option<i64> {\n        let _retirement = self.state.file_retirement.hold();\n')
rep('            Ok(file) => file,\n            Err(error) => return Some(error),','            Ok(file) => self.state.file_retirement.stage(file),\n            Err(error) => return Some(error),')
rep('            accepted,\n            flags & libc::SOCK_CLOEXEC','            accepted.into_file(),\n            flags & libc::SOCK_CLOEXEC')
rep('''            *shared_files =
                FileTableState::try_from_elf(&self.state).expect("clone updated KVM file table");''','''            let next =
                FileTableState::try_from_elf(&self.state).expect("clone updated KVM file table");
            let retired = std::mem::replace(&mut *shared_files, next);
            retired.retire(&self.state.file_retirement);''',2)
rep('''    ) -> crate::Result<Self> {
        let transaction = self.state.signal_transaction.clone();''','''    ) -> crate::Result<Self> {
        // Declare owned child state and its cleanup scope before either guard.
        let mut state;
        let child_retirement;
        let _retirement = self.state.file_retirement.hold();
        let transaction = self.state.signal_transaction.clone();''',2)
start=s.index('    pub(crate) fn fork_child(');end=s.index('    // TODO-HUMAN-REVIEW(PR-172)',start)
a=s[start:end];b=a.replace('        let mut state;\n','        let mut state;\n        let file_table;\n').replace('        let file_table = Arc::new(std::sync::Mutex::new(FileTableState::try_from_elf(&state)?));','        file_table = Arc::new(std::sync::Mutex::new(FileTableState::try_from_elf(&state)?));')
rep(a,b)
rep('        let mut state = self.state.try_clone_for_fork_locked(child_pid)?;', '        state = self.state.try_clone_for_fork_locked(child_pid)?;\n        child_retirement = state.file_retirement.hold();')
rep('        let mut state = self.state.try_clone_for_fork_locked(child_tid)?;', '        state = self.state.try_clone_for_fork_locked(child_tid)?;\n        child_retirement = state.file_retirement.hold();')
# Keep bindings live until scope exit without triggering unused-variable warnings.
s=s.replace('        let child_retirement;\n','        let _child_retirement;\n').replace('        child_retirement = state.file_retirement.hold();','        _child_retirement = state.file_retirement.hold();')
rep('    pub(crate) fn release_files_on_exit(&mut self) {\n','    pub(crate) fn release_files_on_exit(&mut self) {\n        let _retirement = self.state.file_retirement.hold();\n')
rep('''        self.process_action = None;
        drop(_transaction);
        drop(retired);
        drop(stdin);''','''        let action = self.process_action.take();
        drop(_transaction);
        drop(_files);
        self.state.file_retirement.retire(retired.into_values().chain(stdin));
        if let Some(ProcessAction::Exec { executable_file, .. }) = action {
            self.state.file_retirement.retire_shared(executable_file);
        }''')
rep('    pub(crate) fn replace_after_exec(&mut self, state: LoadedStaticElf) {\n','    pub(crate) fn replace_after_exec(&mut self, state: LoadedStaticElf) {\n        let _retirement = self.state.file_retirement.hold();\n')
rep('        let retired = self.state.inherit_process_state_locked(previous);', '        let retired = self.state.inherit_process_state_locked(previous);\n        self.state.file_retirement.retire(retired);')
rep('        self.signal_binding.rebind(&self.state, &file_table);', '        retired_table.retire(&self.state.file_retirement);\n        self.signal_binding.rebind(&self.state, &file_table);')
rep('''        drop(_transaction);
        drop(retired);
        drop(retired_table);''','''        drop(_transaction);
        drop(files);''')
rep('    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {\n', '    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {\n        let _retirement = self.state.file_retirement.hold();\n')
rep('''        }

        let mut memory = memory.clone();
        let accepted = match accept_socket''', '''        }
        self.state.file_retirement.drain_unlocked();

        let mut memory = memory.clone();
        let accepted = match accept_socket''')
rep('''        if !mutating_file_table {
            shared_files.take();
        }''','''        if !mutating_file_table {
            shared_files.take();
            self.state.file_retirement.drain_unlocked();
        }''')
# Helper handoffs keep already-released transaction ordering and defer to outer table owner.
rep('drop(state.insert_file(fd, file));', 'state.file_retirement.retire(state.insert_file(fd, file));',2)
# Avoid overlapping mutable/immutable receiver borrows in these helper calls.
rep('        state.file_retirement.retire(state.insert_file(fd, file));','        let retired = state.insert_file(fd, file);\n        state.file_retirement.retire(retired);')
rep('    state.file_retirement.retire(state.insert_file(fd, file));','    let retired = state.insert_file(fd, file);\n    state.file_retirement.retire(retired);')
rep('        drop(state.insert_file(guest_fd, right.file));','        let retired = state.insert_file(guest_fd, right.file);\n        state.file_retirement.retire(retired);')
rep('    state.remove_file(fd);\n    state.fd_object_inodes', '    let retired = state.remove_file(fd);\n    state.file_retirement.retire(retired);\n    state.fd_object_inodes')
# All remaining explicitly retired local descriptions are in these descriptor helpers.
rep('    drop(retired);\n    i64::from(fd)', '    state.file_retirement.retire(retired);\n    i64::from(fd)')
rep('        drop(retired);\n        i64::from(new_fd)', '        state.file_retirement.retire(retired);\n        i64::from(new_fd)')
rep('        drop(retired);\n        return 0;', '        state.file_retirement.retire(retired);\n        return 0;',2)
rep('''        set_output_alias(state, fd, None);
        drop(_transaction);
        state.file_retirement.retire(retired);
        return 0;
    }
    if is_open_standard''', '''        set_output_alias(state, fd, None);
        drop(_transaction);
        state.file_retirement.retire([retired]);
        return 0;
    }
    if is_open_standard''')
# Pending paired/imported ownership must also survive early insertion errors.
rep('    let [first_file, second_file] = files;\n', '    let [first_file, second_file] = files.map(|file| state.file_retirement.stage(file));\n')
rep('insert_file_with_flags(state, first_file, close_on_exec, None)', 'insert_file_with_flags(state, first_file.into_file(), close_on_exec, None)')
rep('insert_file_with_flags(state, second_file, close_on_exec, None)', 'insert_file_with_flags(state, second_file.into_file(), close_on_exec, None)')
start=s.index('fn install_received_rights(');end=s.index('\n// AUTONOMOUS-BOT-IMPLEMENTED',start)
a=s[start:end];b=a.replace('    let guest_fds = available_guest_fds_with_limit(state, rights.len(), GUEST_NOFILE_LIMIT)?;', '    let rights: Vec<_> = rights.into_iter().map(|right| (right.control_offset, state.file_retirement.stage(right.file))).collect();\n    let guest_fds = available_guest_fds_with_limit(state, rights.len(), GUEST_NOFILE_LIMIT)?;').replace('for (right, guest_fd) in rights.into_iter().zip(guest_fds)', 'for ((control_offset, file), guest_fd) in rights.into_iter().zip(guest_fds)').replace('received_proc_inode(&right.file)', 'received_proc_inode(file.as_file())').replace('allocate_fd_object_inode(state, &right.file)', 'allocate_fd_object_inode(state, file.as_file())').replace('state.insert_file(guest_fd, right.file)', 'state.insert_file(guest_fd, file.into_file())').replace('write_control_fd(control, right.control_offset, guest_fd)', 'write_control_fd(control, control_offset, guest_fd)')
rep(a,b)
# Early insertion failure also owns an unopened-to-the-guest host description.
start=s.index('fn insert_file_with_flags(');end=s.index('\n// AUTONOMOUS-BOT-IMPLEMENTED',start)
a=s[start:end];b=a.replace('        return negative_errno(libc::EMFILE);','        state.file_retirement.retire([file]);\n        return negative_errno(libc::EMFILE);').replace('        Err(error) => return error,','        Err(error) => {\n            state.file_retirement.retire([file]);\n            return error;\n        }')
rep(a,b)
# Direct helper callers also release their own guard before draining failed insertions.
for marker in ['fn duplicate_fd_at_or_above(','fn duplicate_fd(','fn signalfd(']:
 start=s.index(marker);loc=s.index('    let transaction = state.signal_transaction.clone();',start)
 s=s[:loc]+'    let _retirement = state.file_retirement.hold();\n'+s[loc:]
rep('        files: std::collections::BTreeMap::new(),\n        fd_entry_ids:', '        files: std::collections::BTreeMap::new(),\n        file_retirement: crate::elf::FileRetirement::default(),\n        fd_entry_ids:')
rep('    #[test]\n    fn shared_file_table_reopen_same_inode_replaces_description()', (D/'retirement-tests.rs').read_text()+'    #[test]\n    fn shared_file_table_reopen_same_inode_replaces_description()')
save()
# Document actual lock ordering and refuse inconsistent poisoned authoritative state.
edit('reverie-kvm/src/process_signal_publication.rs')
rep('//! Activating it requires the separately reviewed scheduler/selection protocol.','''//! Activating it requires the separately reviewed scheduler/selection protocol.
//!
//! Initial registry/image lookups release their guards before taking files.
//! Publication takes file table -> transaction -> lifecycle -> run failure ->
//! process signals. Image validation briefly reacquires image below transaction;
//! child lookup briefly reacquires registry below lifecycle. Those guards are
//! released before failure/process acquisition. The run failure guard remains
//! held through readiness I/O after process/lifecycle guards are released,
//! serializing publications across
//! processes in this run. Existing executor process/thread locks remain below
//! the transaction. No registry or retirement mutex is held during host closes.''')
rep('        let files = files.lock().unwrap_or_else(|p| p.into_inner());', '''        // Ordinary execution treats an authoritative-table poison as failure.
        // Do not let this inactive endpoint publish through an inconsistent
        // table after a failed ordinary update.
        let files = match files.lock() {
            Ok(files) => files,
            Err(_) => return Rejected(Backend(Errno::EIO)),
        };''')
rep('    #[test]\n    fn inactive_publication_alarm_preserves_masks_dispositions_and_coalesces()', (D/'poison-test.rs').read_text()+'    #[test]\n    fn inactive_publication_alarm_preserves_masks_dispositions_and_coalesces()')
save()
print('Prepared isolated source only')
