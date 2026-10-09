/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! LB7 uses real proc objects, sealed kernel carriers and ordinary children.
//! There is no Hermit execution, runtime activation or fabricated clock model.

use std::ffi::CString;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie_elf_loader::proc_state::AuxvSnapshot;
use reverie_elf_loader::proc_state::AuxvState;
use reverie_elf_loader::proc_state::PinnedFinalElf;
use reverie_elf_loader::proc_state::ProcAliasResolver;
use reverie_elf_loader::proc_state::ProcStateError;
use reverie_elf_loader::proc_state::prepare_exec_continuation;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn artifact(name: &str) -> PathBuf {
    Path::new(env!("ELF_LOADER_ARTIFACT_DIR")).join(format!(
        "lb7-{name}-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn result(name: &str, details: &str) {
    fs::write(
        Path::new(env!("ELF_LOADER_ARTIFACT_DIR")).join(format!("lb7-{name}.result")),
        format!("PASS {name}\n{details}\n"),
    )
    .unwrap();
}

fn pin(path: impl AsRef<Path>) -> File {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)
        .unwrap()
}

fn bytes(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn read_all(mut file: File) -> Vec<u8> {
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    data
}

fn assert_unsupported<T: std::fmt::Debug>(result: Result<T, ProcStateError>) {
    let error = result.unwrap_err();
    assert_eq!(error.name(), "InheritedVirtualProcStateUnsupported");
    assert!(matches!(
        error,
        ProcStateError::InheritedVirtualProcStateUnsupported { .. }
    ));
}

#[test]
fn lb7_auxv_snapshot_fresh_ofd_dup_cursor_and_seals() {
    let words = [3, 0x1122, 9, 0x3344, 31, 0x5566, 0, 0];
    let snapshot = AuxvSnapshot::new(41, &words).unwrap();
    assert_eq!(snapshot.words(), words);
    let seals = snapshot.seals().unwrap();
    let required = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    assert_eq!(seals & required, required);
    let expected = bytes(&words);
    let mut first = snapshot.open().unwrap();
    let mut duplicate = first.try_clone().unwrap();
    let mut prefix = [0; 11];
    first.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, expected[..11]);
    assert_eq!(duplicate.stream_position().unwrap(), 11);
    let mut next = [0; 7];
    duplicate.read_exact(&mut next).unwrap();
    assert_eq!(next, expected[11..18]);
    assert_eq!(first.stream_position().unwrap(), 18);
    assert_eq!(read_all(snapshot.open().unwrap()), expected);
    duplicate.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(first.stream_position().unwrap(), 0);
    assert_eq!(read_all(first), expected);

    // A writable reopen proves sealing independently of the public read-only
    // open flags. Omitting carrier seals would make this mutation succeed.
    let fd = snapshot.open().unwrap();
    let mut writer = OpenOptions::new()
        .write(true)
        .open(format!("/proc/self/fd/{}", fd.as_raw_fd()))
        .unwrap();
    assert_eq!(
        writer.write(&[1]).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        writer.set_len(0).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    result(
        "auxv-ofd-seals",
        "real dup shares cursor; fresh OFD starts at zero; write/shrink EPERM",
    );
}

#[test]
fn lb7_auxv_old_ofd_retains_snapshot_after_modeled_exec() {
    let old_words = [3, 0x1234, 9, 0x5678, 0, 0];
    let new_words = [3, 0x9abc, 9, 0xdef0, 0, 0];
    let mut state = AuxvState::new(AuxvSnapshot::new(10, &old_words).unwrap());
    let mut old = state.open_current().unwrap();
    let mut old_dup = old.try_clone().unwrap();
    let mut prefix = [0; 13];
    old.read_exact(&mut prefix).unwrap();
    state
        .replace(AuxvSnapshot::new(11, &new_words).unwrap())
        .unwrap();
    assert_eq!(state.snapshot().mm_generation(), 11);
    assert_eq!(read_all(state.open_current().unwrap()), bytes(&new_words));
    let mut suffix = Vec::new();
    old_dup.read_to_end(&mut suffix).unwrap();
    assert_eq!(suffix, bytes(&old_words)[13..]);
    old.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(read_all(old_dup), bytes(&old_words));
    assert_unsupported(state.replace(AuxvSnapshot::new(11, &old_words).unwrap()));
    result(
        "auxv-mm-generation",
        "old OFD keeps generation 10 and shared cursor; new open reads generation 11",
    );
}

#[test]
fn lb7_proc_aliases_resolve_pinned_kernel_objects() {
    let resolver = ProcAliasResolver::for_current_task(21).unwrap();
    let task = resolver.task().clone();
    let state = AuxvState::new(AuxvSnapshot::new(21, &[3, 0x1111, 9, 0x2222, 0, 0]).unwrap());
    let self_alias = pin("/proc/self/auxv");
    let thread_alias = pin("/proc/thread-self/auxv");
    let task_alias = pin(format!("/proc/self/task/{}/auxv", task.tid));
    let parent = pin("/proc/self");
    // SAFETY: parent is an owned directory; auxv is a static C string.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c"auxv".as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC,
        )
    };
    assert!(raw >= 0);
    // SAFETY: openat returned a new descriptor owned solely here.
    let relative = unsafe { File::from_raw_fd(raw) };
    let link = artifact("auxv-link");
    symlink("/proc/self/auxv", &link).unwrap();
    let symlink_alias = pin(&link);
    let fd_alias = pin(format!("/proc/self/fd/{}", self_alias.as_raw_fd()));
    let duplicate = thread_alias.try_clone().unwrap();
    for alias in [
        self_alias,
        thread_alias,
        task_alias,
        relative,
        symlink_alias,
        fd_alias,
        duplicate,
    ] {
        let resolved = resolver.resolve(&alias, &task).unwrap();
        assert_eq!(resolved.task, task);
        assert_eq!(
            read_all(state.open_alias(&resolver, &alias, &task).unwrap()),
            bytes(state.snapshot().words())
        );
    }
    fs::remove_file(link).unwrap();
    result(
        "proc-aliases",
        "self/thread-self/task-TID/dirfd/symlink/proc-fd/dup match registered kernel objects",
    );
}

#[test]
fn lb7_unregistered_proc_mount_task_and_mm_are_refused() {
    let resolver = ProcAliasResolver::for_current_task(22).unwrap();
    let task = resolver.task().clone();
    let auxv = pin("/proc/self/auxv");
    assert_unsupported(resolver.resolve(&pin("/proc/self/status"), &task));
    let fake = artifact("fake-proc-self-auxv");
    fs::write(&fake, b"not procfs").unwrap();
    assert_unsupported(resolver.resolve(&pin(&fake), &task));
    fs::remove_file(fake).unwrap();
    let mut stale = task.clone();
    stale.process_start_time += 1;
    assert_unsupported(resolver.resolve(&auxv, &stale));
    stale = task.clone();
    stale.mm_generation += 1;
    assert_unsupported(resolver.resolve(&auxv, &stale));
    let state = AuxvState::new(AuxvSnapshot::new(23, &[3, 1, 9, 2, 0, 0]).unwrap());
    assert_unsupported(state.open_alias(&resolver, &auxv, &task));
    for words in [&[][..], &[3, 1, 0][..], &[0, 0, 3, 1, 0, 0][..]] {
        assert_unsupported(AuxvSnapshot::new(1, words));
    }
    result(
        "proc-refusals",
        "unregistered endpoint/non-proc mount/stale task/stale mm refuse by design name",
    );
}

fn run_native(mut command: Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.env("PYTHONDONTWRITEBYTECODE", "1");
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "bounded native proc control timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "native child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn auxv_value(data: &[u8], kind: u64) -> u64 {
    data.as_chunks::<16>()
        .0
        .iter()
        .find_map(|pair| {
            (u64::from_le_bytes(pair[..8].try_into().unwrap()) == kind)
                .then(|| u64::from_le_bytes(pair[8..].try_into().unwrap()))
        })
        .unwrap()
}

fn python_binary() -> PathBuf {
    // PATH python3 may be a process-launching wrapper on this host. Discover
    // the actual ELF, then exec it directly when PID/mm identity is observed.
    let mut discovery = Command::new("python3");
    discovery
        .arg("-B")
        .arg("-c")
        .arg("import sys; print(sys.executable)");
    let discovery = run_native(discovery);
    PathBuf::from(std::str::from_utf8(&discovery.stdout).unwrap().trim())
}

#[test]
fn lb7_native_exec_keeps_old_auxv_ofd_and_path_reopen_mutation_changes_mm() {
    let old = File::open("/proc/self/auxv").unwrap();
    let mut expected = [0; 8192];
    let count = old.read_at(&mut expected, 0).unwrap();
    let expected = &expected[..count];
    let mut inherited_dup = old.try_clone().unwrap();
    let fd = inherited_dup.as_raw_fd();
    // This owned slot is replaced only in the forked child with that child's
    // own pre-exec auxv OFD. Exec then detaches the very mm retained by it.
    let mut child_slot_owner = File::open("/proc/self/auxv").unwrap();
    let child_slot = child_slot_owner.as_raw_fd();
    let script = r#"import os,sys
fd=int(sys.argv[1])
child_fd=int(sys.argv[2])
child_dup=os.dup(child_fd)
old=os.read(fd,8192)
child_old=os.read(child_fd,8192)
new=open('/proc/self/auxv','rb').read()
parent_reopened=open('/proc/self/fd/'+str(fd),'rb').read()
child_reopened=open('/proc/self/fd/'+str(child_fd),'rb').read()
print(old.hex())
print(child_old.hex())
print(new.hex())
print(parent_reopened.hex())
print(child_reopened.hex())
print(os.lseek(child_dup,0,os.SEEK_CUR))
"#;
    let mut command = Command::new(python_binary());
    command
        .arg("-B")
        .arg("-c")
        .arg(script)
        .arg(fd.to_string())
        .arg(child_slot.to_string());
    // SAFETY: pre_exec uses only async-signal-safe open/dup2/close/fcntl with
    // static strings and owned descriptor slots. Both owners live through
    // completion; no guest or unrelated descriptor is overwritten.
    unsafe {
        command.pre_exec(move || {
            let child_auxv = libc::open(
                c"/proc/self/auxv".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if child_auxv < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(child_auxv, child_slot) < 0 {
                let error = std::io::Error::last_os_error();
                libc::close(child_auxv);
                return Err(error);
            }
            libc::close(child_auxv);
            for descriptor in [fd, child_slot] {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let output = run_native(command);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 6);
    let child_old = unhex(lines[0]);
    let own_child_old = unhex(lines[1]);
    let child_new = unhex(lines[2]);
    let parent_procfd_reopen = unhex(lines[3]);
    let path_reopen_mutation = unhex(lines[4]);
    assert_eq!(child_old, expected);
    assert_eq!(own_child_old, expected);
    assert_ne!(
        (auxv_value(&child_old, 3), auxv_value(&child_old, 9)),
        (auxv_value(&child_new, 3), auxv_value(&child_new, 9))
    );
    assert_eq!(path_reopen_mutation, child_new);
    assert_ne!(path_reopen_mutation, child_old);
    assert_eq!(parent_procfd_reopen, child_old);
    assert_eq!(lines[5].parse::<usize>().unwrap(), count);
    assert_eq!(inherited_dup.stream_position().unwrap(), count as u64);
    assert_eq!(child_slot_owner.stream_position().unwrap(), 0);
    result(
        "native-old-auxv-ofd",
        &format!(
            "old parent and child mm bytes={} preserved exactly; both dup cursors agree; new mm PHDR/ENTRY differ; child-object reopen reads new vector while parent object stays bound to parent\n{text}",
            count
        ),
    );
}

#[test]
fn lb7_native_script_final_elf_identity_and_original_execfn() {
    let python = python_binary();
    let script = artifact("original-script-F.py");
    let body = format!(
        "#!{} -B\nimport os,struct,ctypes\na=open('/proc/self/auxv','rb').read()\nv=dict(struct.iter_unpack('QQ',a))\nprint(ctypes.string_at(v[31]).hex())\ns=os.stat('/proc/self/exe')\nprint(str(s.st_dev)+':'+str(s.st_ino))\n",
        python.display()
    );
    fs::write(&script, body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let final_elf = PinnedFinalElf::new(
        File::open(&python).unwrap(),
        CString::new(script.as_os_str().as_bytes()).unwrap(),
    )
    .unwrap();
    let metadata = final_elf.file().metadata().unwrap();
    let output = run_native(Command::new(&script));
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(unhex(lines[0]), final_elf.original_execfn().to_bytes());
    assert_eq!(lines[1], format!("{}:{}", metadata.dev(), metadata.ino()));
    assert_ne!(
        final_elf.original_execfn().to_bytes(),
        python.as_os_str().as_bytes()
    );
    result(
        "final-script-elf",
        "native proc exe inode matches pinned final ELF T; native AT_EXECFN matches original script F",
    );
}

#[test]
fn lb7_actual_clock_counter_continuation_is_explicitly_refused() {
    assert_unsupported(prepare_exec_continuation());
    result(
        "continuation-refusal",
        "InheritedVirtualProcStateUnsupported; no actual Hermit clock/counter carrier or offset-restoration claim",
    );
}
