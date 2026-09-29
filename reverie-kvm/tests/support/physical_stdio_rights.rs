use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use super::*;

const CHILD_ENV: &str = "REVERIE_PHYSICAL_STDIO_RIGHTS_CHILD";
const OPEN_DONE: &[u8] = b"physical-stdio-independent-rights-ok\n";
const NATIVE_ALIAS_DONE: &[u8] = b"physical-stdio-native-alias-rights-ok\n";
const REFUSAL_DONE: &[u8] = b"physical-stdio-alias-refused-ok\n";

#[derive(Default)]
struct PhysicalStdioRightsLog {
    sends: AtomicU64,
    receives: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for PhysicalStdioRightsLog {
    type Request = bool;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _: Pid, sending: bool) {
        if sending {
            self.sends.fetch_add(1, Ordering::SeqCst);
        } else {
            self.receives.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone, Default)]
struct PhysicalStdioRightsTool;

#[reverie::tool]
impl Tool for PhysicalStdioRightsTool {
    type GlobalState = PhysicalStdioRightsLog;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        match syscall {
            Syscall::Sendmsg(_) => guest.send_rpc(true).await,
            Syscall::Recvmsg(_) => guest.send_rpc(false).await,
            _ => (),
        }
        guest.tail_inject(syscall).await
    }
}

fn isolated_child(test: &str, marker: &str) -> bool {
    match std::env::var(CHILD_ENV) {
        Ok(actual) => {
            assert_eq!(actual, test, "unexpected physical-stdio child identity");
            // These are product controls: unavailable KVM is not a passing skip.
            Kvm::new().expect("physical-stdio SCM_RIGHTS controls require usable /dev/kvm");
            true
        }
        Err(std::env::VarError::NotPresent) => {
            let output = Command::new("timeout")
                .args(["--kill-after=2s", "30s"])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, test)
                .output()
                .unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                output.status.success(),
                "{test}: status={:?} stdout={stdout} stderr={stderr}",
                output.status.code()
            );
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| *line == "running 1 test")
                    .count(),
                1
            );
            let completed = format!("test {test} ... ok");
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| *line == completed.as_str())
                    .count(),
                1
            );
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.starts_with(
                        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                    ))
                    .count(),
                1
            );
            assert_eq!(stderr.lines().filter(|line| *line == marker).count(), 1);
            false
        }
        Err(error) => panic!("invalid physical-stdio child environment: {error}"),
    }
}

struct SavedStandard {
    file: File,
    descriptor_flags: i32,
}

struct RestoreStandards([SavedStandard; 3]);

impl RestoreStandards {
    fn save() -> Self {
        Self([0, 1, 2].map(|fd| {
            let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(descriptor_flags >= 0);
            let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(saved >= 3);
            SavedStandard {
                // SAFETY: F_DUPFD_CLOEXEC returned this new owned descriptor.
                file: unsafe { File::from_raw_fd(saved) },
                descriptor_flags,
            }
        }))
    }

    fn restore_one(&self, fd: i32) {
        let saved = &self.0[fd as usize];
        assert_eq!(unsafe { libc::dup2(saved.file.as_raw_fd(), fd) }, fd);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, saved.descriptor_flags) },
            0
        );
    }
}

impl Drop for RestoreStandards {
    fn drop(&mut self) {
        for fd in [0, 1, 2] {
            self.restore_one(fd);
        }
    }
}

fn redirect(file: &File, standards: &[i32]) {
    for &fd in standards {
        assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), fd) }, fd);
    }
}

fn physical_file(directory: &Path, dev_null: bool) -> (File, PathBuf) {
    let path = if dev_null {
        PathBuf::from("/dev/null")
    } else {
        directory.join("physical-log")
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    if !dev_null {
        options.create_new(true);
    }
    let file = options.open(&path).unwrap();
    if !dev_null {
        assert_eq!(file.write_at(b"supervisor", 0).unwrap(), 10);
    }
    (file, path)
}

fn fd_metadata(fd: i32) -> (u64, u64, u32, i64) {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    assert_eq!(unsafe { libc::fstat(fd, metadata.as_mut_ptr()) }, 0);
    // SAFETY: successful fstat initialized metadata.
    let metadata = unsafe { metadata.assume_init() };
    (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
    )
}

struct PhysicalSnapshot {
    metadata: (u64, u64, u32, i64),
    flags: i32,
    offset: i64,
    contents: Option<Vec<u8>>,
}

impl PhysicalSnapshot {
    fn new(physical: &File, path: &Path) -> Self {
        let metadata = fd_metadata(physical.as_raw_fd());
        let flags = unsafe { libc::fcntl(physical.as_raw_fd(), libc::F_GETFL) };
        let offset = unsafe { libc::lseek(physical.as_raw_fd(), 0, libc::SEEK_CUR) };
        assert!(flags >= 0 && offset >= 0);
        let contents =
            (metadata.2 & libc::S_IFMT == libc::S_IFREG).then(|| std::fs::read(path).unwrap());
        Self {
            metadata,
            flags,
            offset,
            contents,
        }
    }

    fn assert_unchanged(&self, physical: &File, path: &Path) {
        for fd in [physical.as_raw_fd(), 1, 2] {
            assert_eq!(fd_metadata(fd), self.metadata, "physical fd={fd}");
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFL) }, self.flags);
            assert_eq!(unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) }, self.offset);
        }
        if let Some(contents) = &self.contents {
            assert_eq!(&std::fs::read(path).unwrap(), contents);
        }
    }
}

fn native_control(executable: &Path, mode: &str, path: &Path, physical: &File, expected: &[u8]) {
    let metadata = physical.metadata().unwrap();
    let mut descriptors = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    assert!(descriptors.iter().all(|fd| *fd >= 3));
    // SAFETY: pipe2 returned these two distinct, newly owned descriptors.
    let mut report = unsafe { File::from_raw_fd(descriptors[0]) };
    let writer = unsafe { File::from_raw_fd(descriptors[1]) };
    let report_fd = writer.as_raw_fd();
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=2s", "10s"])
        .arg(executable)
        .arg(mode)
        .arg(path)
        .arg(metadata.dev().to_string())
        .arg(metadata.ino().to_string())
        .arg(report_fd.to_string())
        .stdin(Stdio::from(physical.try_clone().unwrap()))
        .stdout(Stdio::from(physical.try_clone().unwrap()))
        .stderr(Stdio::from(physical.try_clone().unwrap()));
    // SAFETY: the child closure uses only fcntl and captures an already-open
    // descriptor. It changes CLOEXEC only in the forked child. The dedicated
    // report pipe leaves native 0/1/2 sharing the actual physical OFD.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(report_fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command.status().unwrap();
    drop(writer);
    let mut bytes = Vec::new();
    report
        .by_ref()
        .take((expected.len() + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(status.code(), Some(0), "native mode={mode} path={path:?}");
    assert_eq!(bytes, expected, "native mode={mode} path={path:?}");
}

fn guest_control(
    mut backend: KvmBackend,
    executable: &Path,
    mode: &str,
    path: &Path,
    physical: &File,
    with_tool: bool,
    expected: &[u8],
) {
    let metadata = physical.metadata().unwrap();
    let dev = metadata.dev().to_string();
    let ino = metadata.ino().to_string();
    backend
        .install_static_elf_file_with_context(
            File::open(executable).unwrap(),
            &[
                executable.to_str().unwrap(),
                mode,
                path.to_str().unwrap(),
                &dev,
                &ino,
                "1",
            ],
            &[],
            executable.parent().unwrap(),
        )
        .unwrap();
    let (code, stdout, stderr) = if with_tool {
        let (log, code, stdout, stderr) = futures::executor::block_on(
            backend.run_static_elf_with_tool::<PhysicalStdioRightsTool>((), true),
        )
        .unwrap();
        assert_eq!(
            log.sends.load(Ordering::SeqCst),
            1,
            "actual sendmsg callbacks"
        );
        assert_eq!(
            log.receives.load(Ordering::SeqCst),
            1,
            "actual recvmsg callbacks"
        );
        (code, stdout, stderr)
    } else {
        backend.run_static_elf_captured().unwrap()
    };
    assert_eq!(
        code, 0,
        "mode={mode} path={path:?} with_tool={with_tool} stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        stdout, expected,
        "mode={mode} path={path:?} with_tool={with_tool}"
    );
    assert!(
        stderr.is_empty(),
        "mode={mode} with_tool={with_tool} stderr={stderr:?}"
    );
}

#[test]
fn independent_opens_of_physical_stdio_inode_transfer() {
    const TEST: &str = "physical_stdio_rights::independent_opens_of_physical_stdio_inode_transfer";
    const DONE: &str = "physical stdio independent inode native and KVM controls complete";
    if !isolated_child(TEST, DONE) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "physical-stdio-rights",
        include_str!("../fixtures/physical_stdio_rights.c"),
    );
    let restore = RestoreStandards::save();
    for dev_null in [false, true] {
        let (physical, path) = physical_file(&directory.0, dev_null);
        redirect(&physical, &[1, 2]);
        let before = PhysicalSnapshot::new(&physical, &path);
        native_control(&executable, "open", &path, &physical, OPEN_DONE);
        before.assert_unchanged(&physical, &path);
        for with_tool in [false, true] {
            let backend = KvmBackend::new_with_stdin(256 * 1024 * 1024, None).unwrap();
            guest_control(
                backend,
                &executable,
                "open",
                &path,
                &physical,
                with_tool,
                OPEN_DONE,
            );
            before.assert_unchanged(&physical, &path);
        }
    }
    drop(restore);
    eprintln!("{DONE}");
}

#[test]
fn supplied_and_inherited_physical_stdin_aliases_are_refused() {
    const TEST: &str =
        "physical_stdio_rights::supplied_and_inherited_physical_stdin_aliases_are_refused";
    const DONE: &str = "physical stdio supplied and inherited alias controls complete";
    if !isolated_child(TEST, DONE) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "physical-stdio-rights",
        include_str!("../fixtures/physical_stdio_rights.c"),
    );
    let restore = RestoreStandards::save();
    for dev_null in [false, true] {
        let (physical, path) = physical_file(&directory.0, dev_null);
        redirect(&physical, &[1, 2]);
        let before = PhysicalSnapshot::new(&physical, &path);
        // Native Linux permits this alias. KVM's refusal below is its explicit
        // supervisor-isolation boundary, not a claim of native parity.
        native_control(
            &executable,
            "stdin-native",
            &path,
            &physical,
            NATIVE_ALIAS_DONE,
        );
        before.assert_unchanged(&physical, &path);
        for inherited in [false, true] {
            for with_tool in [false, true] {
                let backend = if inherited {
                    redirect(&physical, &[0]);
                    KvmBackend::new(256 * 1024 * 1024).unwrap()
                } else {
                    restore.restore_one(0);
                    KvmBackend::new_with_stdin(
                        256 * 1024 * 1024,
                        Some(physical.try_clone().unwrap()),
                    )
                    .unwrap()
                };
                guest_control(
                    backend,
                    &executable,
                    "stdin-refuse",
                    &path,
                    &physical,
                    with_tool,
                    REFUSAL_DONE,
                );
                before.assert_unchanged(&physical, &path);
                if inherited {
                    assert_eq!(fd_metadata(0), before.metadata);
                    assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, before.flags);
                    assert_eq!(unsafe { libc::lseek(0, 0, libc::SEEK_CUR) }, before.offset);
                }
            }
        }
    }
    drop(restore);
    eprintln!("{DONE}");
}
