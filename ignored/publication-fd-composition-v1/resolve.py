"""Resolve only the retained source-only merge; never reads or writes live source."""
from pathlib import Path
import re

root = Path(__file__).resolve().parent
pattern = r'^<<<<<<< publisher-frozen-v1\n(.*?)^=======\n(.*?)^>>>>>>> fd-final-v3\n'

for name in ('elf.rs', 'executor.rs'):
    source = (root / 'auto-merge/reverie-kvm/src' / name).read_text()
    conflicts = list(re.finditer(pattern, source, re.M | re.S))
    assert len(conflicts) == (1 if name == 'elf.rs' else 3)
    if name == 'elf.rs':
        replacements = [conflicts[0][2] + conflicts[0][1]]
    else:
        replacements = [
            conflicts[0][1] + '        self.state.fd_entry_ids.clear();\n',
            '        let retired = state.insert_file(new_fd, file);\n',
            '    if let Some(retired) = state.remove_file(fd) {\n',
        ]
    for match, replacement in reversed(list(zip(conflicts, replacements))):
        source = source[:match.start()] + replacement + source[match.end():]

    def replace_once(old, new):
        global source
        assert source.count(old) == 1, (name, old, source.count(old))
        source = source.replace(old, new)

    if name == 'elf.rs':
        replace_once(
            '    pub(crate) fn insert_file(&mut self, fd: i32, file: std::fs::File) {\n'
            '        self.files.insert(fd, file);\n'
            '        self.fd_entry_ids.insert(fd, std::sync::Arc::new(()));\n'
            '    }\n',
            '    /// Return the replaced description so callers holding signal_transaction\n'
            '    /// can release that guard before the final host close.\n'
            '    pub(crate) fn insert_file(&mut self, fd: i32, file: std::fs::File) -> Option<std::fs::File> {\n'
            '        let retired = self.files.insert(fd, file);\n'
            '        self.fd_entry_ids.insert(fd, std::sync::Arc::new(()));\n'
            '        retired\n'
            '    }\n',
        )
    else:
        replace_once('        state.insert_file(fd, file);\n',
                     '        drop(state.insert_file(fd, file));\n')
        replace_once('    state.insert_file(fd, file);\n    state.fd_object_inodes.insert(fd, object_inode);\n',
                     '    // The selected slot is vacant; this cannot retire a description,\n'
                     '    // including when the caller holds signal_transaction.\n'
                     '    drop(state.insert_file(fd, file));\n'
                     '    state.fd_object_inodes.insert(fd, object_inode);\n')
        replace_once('    state.insert_file(fd, file);\n    state.fd_object_inodes.insert(fd, source.object_inode);\n',
                     '    let retired = state.insert_file(fd, file);\n'
                     '    state.fd_object_inodes.insert(fd, source.object_inode);\n')
        replace_once('    if let Some(description) = source.fdinfo {\n'
                     '        state.fdinfo_files.insert(fd, description);\n'
                     '    }\n'
                     '    i64::from(fd)\n',
                     '    if let Some(description) = source.fdinfo {\n'
                     '        state.fdinfo_files.insert(fd, description);\n'
                     '    }\n'
                     '    drop(_transaction);\n'
                     '    drop(retired);\n'
                     '    i64::from(fd)\n')
        replace_once('        state.insert_file(guest_fd, right.file);\n',
                     '        drop(state.insert_file(guest_fd, right.file));\n')
    assert not re.search(r'^(<<<<<<<|=======|>>>>>>>)', source, re.M)
    (root / 'source/reverie-kvm/src' / name).write_text(source)
