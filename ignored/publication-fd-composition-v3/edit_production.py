from pathlib import Path
D=Path(__file__).resolve().parent
p=D/'source/reverie-kvm/src/elf.rs';s=p.read_text()
def replace(old,new,count=1):
 global s
 assert s.count(old)==count,(old[:90],s.count(old),count)
 s=s.replace(old,new)
replace('    pub stdin: Option<std::fs::File>,','''    pub stdin: Option<std::fs::File>,
    /// Identity of the inherited stdin slot, distinct from its inode/OFD.
    /// Loader setup precedes table publication; fork and exec retain this
    /// identity, while closing or replacing the slot creates a new identity.
    pub stdin_entry_id: std::sync::Arc<()>,''')
replace('''    pub(crate) fn insert_file(&mut self, fd: i32, file: std::fs::File) -> Option<std::fs::File> {
        let retired = self.files.insert(fd, file);
        self.fd_entry_ids.insert(fd, std::sync::Arc::new(()));
        retired
    }''','''    pub(crate) fn insert_file(&mut self, fd: i32, file: std::fs::File) -> Vec<std::fs::File> {
        let mut retired: Vec<_> = self.files.insert(fd, file).into_iter().collect();
        if fd == libc::STDIN_FILENO {
            retired.extend(self.take_stdin());
        }
        self.fd_entry_ids.insert(fd, std::sync::Arc::new(()));
        retired
    }

    pub(crate) fn take_stdin(&mut self) -> Option<std::fs::File> {
        let stdin = self.stdin.take();
        if stdin.is_some() {
            self.stdin_entry_id = std::sync::Arc::new(());
        }
        stdin
    }''')
replace('            stdin: stdin.map(StagedFile::into_file),','            stdin: stdin.map(StagedFile::into_file),\n            stdin_entry_id: self.stdin_entry_id.clone(),')
replace('        let mut stdin = previous.stdin;','        let mut stdin = previous.stdin;\n        let mut stdin_entry_id = previous.stdin_entry_id;')
replace('''            retired.extend(stdin.take());
            closed_standard_fds.insert(libc::STDIN_FILENO);''','''            retired.extend(stdin.take());
            stdin_entry_id = std::sync::Arc::new(());
            closed_standard_fds.insert(libc::STDIN_FILENO);''')
replace('        retired.extend(std::mem::replace(&mut self.stdin, stdin));','        retired.extend(std::mem::replace(&mut self.stdin, stdin));\n        self.stdin_entry_id = stdin_entry_id;')
replace('        stdin: None,','        stdin: None,\n        stdin_entry_id: std::sync::Arc::new(()),')
p.write_text(s)
p=D/'source/reverie-kvm/src/executor.rs';s=p.read_text()
replace('''pub(crate) struct FileTableState {
    stdin: Option<std::fs::File>,''','''pub(crate) struct FileTableState {
    stdin: Option<std::fs::File>,
    stdin_entry_id: Arc<()>,''')
replace('''    fn try_from_elf(state: &LoadedStaticElf) -> std::io::Result<Self> {
        let stdin = state
            .stdin
            .as_ref()
            .map(|file| state.file_retirement.stage_clone(file))
            .transpose()?;''','''    fn try_from_elf(state: &LoadedStaticElf) -> std::io::Result<Self> {
        Self::prepare_from_elf(state, false)
    }

    fn same_stdin_entry(&self, state: &LoadedStaticElf) -> bool {
        self.stdin.is_some() == state.stdin.is_some()
            && Arc::ptr_eq(&self.stdin_entry_id, &state.stdin_entry_id)
    }

    fn update_from_elf(&mut self, state: &LoadedStaticElf) -> std::io::Result<()> {
        let same_stdin = self.same_stdin_entry(state);
        // Stage every required clone before taking an authoritative handle.
        // A failed clone leaves the entire previous table intact.
        let mut next = Self::prepare_from_elf(state, same_stdin)?;
        if same_stdin {
            next.stdin = self.stdin.take();
        }
        let retired = std::mem::replace(self, next);
        retired.retire(&state.file_retirement);
        Ok(())
    }

    fn prepare_from_elf(state: &LoadedStaticElf, preserve_stdin: bool) -> std::io::Result<Self> {
        let stdin = if preserve_stdin {
            None
        } else {
            state
                .stdin
                .as_ref()
                .map(|file| state.file_retirement.stage_clone(file))
                .transpose()?
        };''')
replace('            stdin: stdin.map(crate::elf::StagedFile::into_file),','            stdin: stdin.map(crate::elf::StagedFile::into_file),\n            stdin_entry_id: state.stdin_entry_id.clone(),')
replace('''        // Finish every fallible clone, including stdin, before replacing state.''','''        // Finish every required fallible clone before replacing state.''')
replace('''        let stdin = self
            .stdin
            .as_ref()
            .map(|file| state.file_retirement.stage_clone(file))
            .transpose()?;''','''        let same_stdin = self.same_stdin_entry(state);
        let stdin = if same_stdin {
            None
        } else {
            self.stdin
                .as_ref()
                .map(|file| state.file_retirement.stage_clone(file))
                .transpose()?
        };''')
replace('''        let retired_stdin = std::mem::replace(
            &mut state.stdin,
            stdin.map(crate::elf::StagedFile::into_file),
        );''','''        let retired_stdin = if same_stdin {
            None
        } else {
            std::mem::replace(&mut state.stdin, stdin.map(crate::elf::StagedFile::into_file))
        };
        state.stdin_entry_id.clone_from(&self.stdin_entry_id);''')
replace('''            let next =
                FileTableState::try_from_elf(&self.state).expect("clone updated KVM file table");
            let retired = std::mem::replace(&mut *shared_files, next);
            retired.retire(&self.state.file_retirement);''','''            shared_files
                .update_from_elf(&self.state)
                .expect("clone updated KVM file table");''',2)
replace('''        let retired_table = std::mem::replace(
            &mut *files,
            FileTableState::try_from_elf(&self.state).expect("clone post-exec KVM file table"),
        );
        retired_table.retire(&self.state.file_retirement);''','''        files
            .update_from_elf(&self.state)
            .expect("clone post-exec KVM file table");''')
replace('        let stdin = self.state.stdin.take();','        let stdin = self.state.take_stdin();')
replace('            state.stdin.take()','            state.take_stdin()')
replace('        stdin: Some(std::fs::File::open("/dev/null").unwrap()),','        stdin: Some(std::fs::File::open("/dev/null").unwrap()),\n        stdin_entry_id: Arc::new(()),')
p.write_text(s)
