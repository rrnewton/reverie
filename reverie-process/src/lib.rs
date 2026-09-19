/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A drop-in replacement for `std::process::Command` that provides the ability
//! to set up namespaces, a seccomp filter, and more.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg(target_os = "linux")]
#![cfg_attr(feature = "nightly", feature(internal_output_capture))]

mod builder;
mod child;
mod clone;
mod container;
mod env;
mod error;
mod exit_status;
mod fd;
mod id_map;
mod mount;
mod namespace;
mod net;
mod pid;
mod pty;
pub mod seccomp;
mod spawn;
mod stdio;
mod util;

use std::ffi::CString;
use std::fmt;
use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::OwnedFd;

pub use child::Child;
pub use child::Output;
pub use container::Container;
pub use container::DeferredContainerRun;
pub use container::RunError;
pub use error::Context;
pub use error::Error;
pub use exit_status::ExitStatus;
pub use mount::Bind;
pub use mount::Mount;
pub use mount::MountFlags;
pub use mount::MountParseError;
pub use namespace::Namespace;
// Re-export Signal since it is used by `Child::signal`.
pub use nix::sys::signal::Signal;
pub use pid::Pid;
pub use pty::Pty;
pub use pty::PtyChild;
pub use stdio::ChildStderr;
pub use stdio::ChildStdin;
pub use stdio::ChildStdout;
pub use stdio::Stdio;
use syscalls::Errno;

/// Typed reason that a pathname-only consumer refused descriptor execution.
///
/// Consumers that inspect, rewrite, or wrap the path returned by
/// [`Command::get_program`] cannot safely accept [`Command::executable`]
/// unless they bind that work to the same open file description. This error
/// preserves which consumer made that explicit capability decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorExecutionUnsupported {
    consumer: &'static str,
}

impl DescriptorExecutionUnsupported {
    fn new(consumer: &'static str) -> Self {
        Self { consumer }
    }

    /// Returns the pathname-only consumer that refused descriptor execution.
    pub fn consumer(&self) -> &'static str {
        self.consumer
    }
}

impl fmt::Display for DescriptorExecutionUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} does not support descriptor-based execution",
            self.consumer
        )
    }
}

impl std::error::Error for DescriptorExecutionUnsupported {}

/// A builder for spawning a process.
// See the builder.rs for documentation of each field.
pub struct Command {
    program: CString,
    executable: Option<OwnedFd>,
    args: util::CStringArray,
    pre_exec: Vec<Box<dyn FnMut() -> Result<(), Errno> + Send + Sync>>,
    container: Container,
}

impl Command {
    /// Converts [`std::process::Command`] into [`Command`]. Note that this is a
    /// very basic and *lossy* conversion.
    ///
    /// This only preserves the
    ///  - program path,
    ///  - arguments,
    ///  - environment variables,
    ///  - and working directory.
    ///
    /// # Caveats
    ///
    /// Since [`std::process::Command`] is rather opaque and doesn't provide
    /// access to all fields, this will *not* preserve:
    ///  - stdio handles,
    ///  - `env_clear`,
    ///  - any `pre_exec` callbacks,
    ///  - `arg0` (if not the same as `program`),
    ///  - `uid`, `gid`, or `groups`.
    pub fn from_std_lossy(cmd: &std::process::Command) -> Command {
        let mut result = Command::new(cmd.get_program());
        result.args(cmd.get_args());

        for (key, value) in cmd.get_envs() {
            match value {
                Some(value) => result.env(key, value),
                None => result.env_remove(key),
            };
        }

        if let Some(dir) = cmd.get_current_dir() {
            result.current_dir(dir);
        }

        result
    }

    /// Converts this command to [`std::process::Command`].
    ///
    /// This fails if the command contains container configuration that cannot
    /// be represented by [`std::process::Command`], rather than silently
    /// discarding that configuration. This includes namespaces, mounts,
    /// seccomp filters, pseudoterminals, CPU affinity, and descriptor-based
    /// execution.
    pub fn try_into_std(self) -> io::Result<std::process::Command> {
        if self.executable.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot convert to std::process::Command without losing: executable file descriptor",
            ));
        }
        let blockers = self.container.std_conversion_blockers();
        if !blockers.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cannot convert to std::process::Command without losing: {}",
                    blockers.join(", ")
                ),
            ));
        }

        Ok(self.into_std())
    }

    /// Converts this command to [`std::process::Command`], refusing to discard
    /// any container configuration.
    ///
    /// This compatibility shim preserves the former return type for callers
    /// whose commands are representable. It panics instead of silently losing
    /// unsupported configuration. New callers should use [`Self::try_into_std`]
    /// to handle that refusal explicitly.
    pub fn into_std_lossy(self) -> std::process::Command {
        self.try_into_std().unwrap_or_else(|error| {
            panic!("Command::into_std_lossy refused unsupported configuration: {error}")
        })
    }

    fn into_std(self) -> std::process::Command {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // Keep this exhaustive: adding Command state must fail to compile until
        // the standard-command conversion explicitly preserves or refuses it.
        let Self {
            program,
            executable: _,
            args,
            pre_exec,
            container,
        } = self;

        let mut result = std::process::Command::new(OsStr::from_bytes(program.to_bytes()));
        result.args(
            args.iter()
                .skip(1)
                .map(|arg| OsStr::from_bytes(arg.to_bytes())),
        );

        if container.env.is_cleared() {
            result.env_clear();
        }

        for (key, value) in container.get_envs() {
            match value {
                Some(value) => result.env(key, value),
                None => result.env_remove(key),
            };
        }

        if let Some(dir) = container.get_current_dir() {
            result.current_dir(dir);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            result.arg0(OsStr::from_bytes(args.get(0).to_bytes()));

            for mut f in pre_exec {
                unsafe {
                    result.pre_exec(move || f().map_err(Into::into));
                }
            }
        }

        result.stdin(container.stdin);
        result.stdout(container.stdout);
        result.stderr(container.stderr);

        result
    }

    /// Returns the file descriptor selected for descriptor-based execution.
    ///
    /// When present, [`Command::spawn`] executes this open file description
    /// with `execveat(AT_EMPTY_PATH)` and uses `program` only as argument zero.
    pub fn get_executable(&self) -> Option<BorrowedFd<'_>> {
        self.executable
            .as_ref()
            .map(|executable| executable.as_fd())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::fs::File;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::str::from_utf8;
    use std::time::Duration;
    use std::time::Instant;

    use super::*;
    use crate::ExitStatus;

    const DESCRIPTOR_CONTROL_ROLE: &str = "REVERIE_PROCESS_DESCRIPTOR_CONTROL_ROLE";
    const DESCRIPTOR_CONTROL_CASE: &str = "REVERIE_PROCESS_DESCRIPTOR_CONTROL_CASE";
    const DESCRIPTOR_CONTROL_RECORD: &str = "REVERIE_PROCESS_DESCRIPTOR_CONTROL_RECORD";
    const DESCRIPTOR_CONTROL_INPUT: &str = "REVERIE_PROCESS_DESCRIPTOR_CONTROL_INPUT";
    const DESCRIPTOR_CONTROL_LAUNCH: &str = "REVERIE_PROCESS_DESCRIPTOR_CONTROL_LAUNCH";
    const DESCRIPTOR_CONTROL_SEALS: i32 =
        libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum DescriptorControlInput {
        Standard(i32),
        Ordinary,
    }

    impl DescriptorControlInput {
        fn name(self) -> &'static str {
            match self {
                Self::Standard(0) => "stdin",
                Self::Standard(1) => "stdout",
                Self::Standard(2) => "stderr",
                Self::Standard(_) => unreachable!("only standard descriptors are named"),
                Self::Ordinary => "ordinary",
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum StandardDescriptorSnapshot {
        Open {
            device: u64,
            inode: u64,
            mode: u32,
            rdev: u64,
            flags: i32,
        },
        Closed {
            fstat_errno: Option<i32>,
            fcntl_errno: Option<i32>,
        },
    }

    fn standard_descriptor_snapshot(descriptor: i32) -> StandardDescriptorSnapshot {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        let fstat_result = unsafe { libc::fstat(descriptor, status.as_mut_ptr()) };
        let fstat_errno = (fstat_result == -1)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten();
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        let fcntl_errno = (flags == -1)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten();
        if fstat_result == 0 && flags != -1 {
            let status = unsafe { status.assume_init() };
            StandardDescriptorSnapshot::Open {
                device: status.st_dev,
                inode: status.st_ino,
                mode: status.st_mode,
                rdev: status.st_rdev,
                flags,
            }
        } else {
            StandardDescriptorSnapshot::Closed {
                fstat_errno,
                fcntl_errno,
            }
        }
    }

    fn standard_descriptors() -> [StandardDescriptorSnapshot; 3] {
        [
            standard_descriptor_snapshot(libc::STDIN_FILENO),
            standard_descriptor_snapshot(libc::STDOUT_FILENO),
            standard_descriptor_snapshot(libc::STDERR_FILENO),
        ]
    }

    fn sealed_self_executable() -> File {
        let descriptor = unsafe {
            libc::memfd_create(
                c"reverie-process-descriptor-control".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        assert_ne!(
            descriptor,
            -1,
            "failed to create descriptor-control memfd: {}",
            io::Error::last_os_error()
        );
        let mut retained = unsafe { File::from_raw_fd(descriptor) };
        let mut source = File::open(std::env::current_exe().unwrap()).unwrap();
        let copied = io::copy(&mut source, &mut retained).unwrap();
        assert!(copied > 0, "descriptor-control executable was empty");
        assert_eq!(
            unsafe { libc::fchmod(descriptor, 0o500) },
            0,
            "failed to make descriptor-control memfd executable: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_ADD_SEALS, DESCRIPTOR_CONTROL_SEALS) },
            0,
            "failed to seal descriptor-control executable: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_GET_SEALS) },
            DESCRIPTOR_CONTROL_SEALS
        );
        retained
    }

    fn duplicate_to_control_input(retained: &File, input: DescriptorControlInput) -> OwnedFd {
        let target = match input {
            DescriptorControlInput::Standard(target) => {
                assert_eq!(
                    unsafe { libc::dup2(retained.as_raw_fd(), target) },
                    target,
                    "failed to install retained executable at descriptor {target}: {}",
                    io::Error::last_os_error()
                );
                assert_eq!(
                    unsafe { libc::fcntl(target, libc::F_SETFD, libc::FD_CLOEXEC) },
                    0,
                    "failed to set CLOEXEC on input descriptor {target}: {}",
                    io::Error::last_os_error()
                );
                target
            }
            DescriptorControlInput::Ordinary => {
                let target =
                    unsafe { libc::fcntl(retained.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
                assert!(
                    target > libc::STDERR_FILENO,
                    "failed to create ordinary descriptor-control input: {}",
                    io::Error::last_os_error()
                );
                target
            }
        };
        unsafe { OwnedFd::from_raw_fd(target) }
    }

    fn wait_reverie_child_bounded(child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let pid = child.id();
                child.signal(Signal::SIGKILL).unwrap();
                let status = child.wait_blocking().unwrap();
                panic!("descriptor-control child {pid} timed out and was reaped as {status:?}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_std_child_bounded(child: &mut std::process::Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let pid = child.id();
                child.kill().unwrap();
                let status = child.wait().unwrap();
                panic!("descriptor-control setup child {pid} timed out and was reaped as {status}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn descriptor_control_exec(input: DescriptorControlInput) {
        assert_eq!(
            std::env::var(DESCRIPTOR_CONTROL_CASE).unwrap(),
            input.name()
        );
        let input_descriptor = std::env::var(DESCRIPTOR_CONTROL_INPUT)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let launch_descriptor = std::env::var(DESCRIPTOR_CONTROL_LAUNCH)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert!(launch_descriptor > libc::STDERR_FILENO);
        match input {
            DescriptorControlInput::Standard(expected) => {
                assert_eq!(input_descriptor, expected);
                assert_ne!(launch_descriptor, input_descriptor);
            }
            DescriptorControlInput::Ordinary => {
                assert!(input_descriptor > libc::STDERR_FILENO);
                assert_eq!(launch_descriptor, input_descriptor);
            }
        }
        assert_eq!(
            unsafe { libc::fcntl(launch_descriptor, libc::F_GETFD) },
            0,
            "exec did not clear FD_CLOEXEC on the retained executable"
        );
        assert_eq!(
            unsafe { libc::fcntl(launch_descriptor, libc::F_GET_SEALS) },
            DESCRIPTOR_CONTROL_SEALS,
            "exec child observed the wrong retained executable seals"
        );

        let descriptor_path = format!("/proc/self/fd/{launch_descriptor}");
        let descriptor_metadata = fs::metadata(&descriptor_path).unwrap();
        let executable_metadata = fs::metadata("/proc/self/exe").unwrap();
        assert_eq!(descriptor_metadata.dev(), executable_metadata.dev());
        assert_eq!(descriptor_metadata.ino(), executable_metadata.ino());
        assert_eq!(descriptor_metadata.mode(), executable_metadata.mode());
        assert_eq!(descriptor_metadata.len(), executable_metadata.len());
        let descriptor_bytes = fs::read(&descriptor_path).unwrap();
        let executable_bytes = fs::read("/proc/self/exe").unwrap();
        assert!(!descriptor_bytes.is_empty());
        assert_eq!(descriptor_bytes, executable_bytes);

        if let DescriptorControlInput::Standard(descriptor) = input {
            let stdio_metadata = fs::metadata(format!("/proc/self/fd/{descriptor}")).unwrap();
            assert_ne!(
                (stdio_metadata.dev(), stdio_metadata.ino()),
                (descriptor_metadata.dev(), descriptor_metadata.ino()),
                "configured standard I/O did not replace descriptor {descriptor}"
            );
        }

        let record = std::env::var_os(DESCRIPTOR_CONTROL_RECORD).unwrap();
        let mut record = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(record)
            .unwrap();
        write!(
            record,
            "case={}\ninput={}\nlaunch={}\ncloexec=0\nbytes={}\n",
            input.name(),
            input_descriptor,
            launch_descriptor,
            descriptor_bytes.len()
        )
        .unwrap();
        record.sync_all().unwrap();
    }

    fn descriptor_control_setup(input: DescriptorControlInput, test_name: &'static str) {
        assert_eq!(
            std::env::var(DESCRIPTOR_CONTROL_CASE).unwrap(),
            input.name()
        );
        let retained = sealed_self_executable();
        let executable = duplicate_to_control_input(&retained, input);
        let input_descriptor = executable.as_raw_fd();
        let mut command = Command::new("/diagnostic/path/must-not-be-executed");
        command.executable(executable).unwrap();
        let launch_descriptor = command.get_executable().unwrap().as_raw_fd();
        assert!(launch_descriptor > libc::STDERR_FILENO);
        match input {
            DescriptorControlInput::Standard(expected) => {
                assert_eq!(input_descriptor, expected);
                assert_ne!(launch_descriptor, input_descriptor);
            }
            DescriptorControlInput::Ordinary => assert_eq!(launch_descriptor, input_descriptor),
        }
        assert_eq!(
            unsafe { libc::fcntl(launch_descriptor, libc::F_GETFD) },
            libc::FD_CLOEXEC,
            "parent launch descriptor must remain close-on-exec before clone"
        );

        command
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(DESCRIPTOR_CONTROL_ROLE, "exec")
            .env(DESCRIPTOR_CONTROL_INPUT, input_descriptor.to_string())
            .env(DESCRIPTOR_CONTROL_LAUNCH, launch_descriptor.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        assert_eq!(
            wait_reverie_child_bounded(&mut child),
            ExitStatus::Exited(0)
        );
    }

    fn parse_descriptor_control_record(bytes: &[u8], input: DescriptorControlInput) -> (i32, i32) {
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.ends_with('\n'));
        let mut lines = text.lines();
        let expected_case = format!("case={}", input.name());
        assert_eq!(lines.next(), Some(expected_case.as_str()));
        let input_descriptor = lines
            .next()
            .and_then(|line| line.strip_prefix("input="))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let launch_descriptor = lines
            .next()
            .and_then(|line| line.strip_prefix("launch="))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(lines.next(), Some("cloexec=0"));
        let bytes = lines
            .next()
            .and_then(|line| line.strip_prefix("bytes="))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(bytes > 0);
        assert_eq!(lines.next(), None);
        (input_descriptor, launch_descriptor)
    }

    fn descriptor_execution_control(input: DescriptorControlInput, test_name: &'static str) {
        match std::env::var(DESCRIPTOR_CONTROL_ROLE) {
            Ok(role) if role == "setup" => {
                descriptor_control_setup(input, test_name);
                return;
            }
            Ok(role) if role == "exec" => {
                descriptor_control_exec(input);
                return;
            }
            Ok(role) => panic!("unknown descriptor-control role {role:?}"),
            Err(std::env::VarError::NotPresent) => {}
            Err(error) => panic!("invalid descriptor-control role: {error}"),
        }

        let standard_before = standard_descriptors();
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("descriptor-record");
        let mut setup = std::process::Command::new(std::env::current_exe().unwrap());
        setup
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(DESCRIPTOR_CONTROL_ROLE, "setup")
            .env(DESCRIPTOR_CONTROL_CASE, input.name())
            .env(DESCRIPTOR_CONTROL_RECORD, &record)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut setup = setup.spawn().unwrap();
        assert!(wait_std_child_bounded(&mut setup).success());
        assert_eq!(
            standard_descriptors(),
            standard_before,
            "sacrificial descriptor setup changed the parent test process"
        );

        let (input_descriptor, launch_descriptor) =
            parse_descriptor_control_record(&fs::read(record).unwrap(), input);
        assert!(launch_descriptor > libc::STDERR_FILENO);
        match input {
            DescriptorControlInput::Standard(expected) => {
                assert_eq!(input_descriptor, expected);
                assert_ne!(launch_descriptor, input_descriptor);
            }
            DescriptorControlInput::Ordinary => {
                assert!(input_descriptor > libc::STDERR_FILENO);
                assert_eq!(launch_descriptor, input_descriptor);
            }
        }
    }

    #[test]
    fn descriptor_execution_relocates_input_fd_zero() {
        descriptor_execution_control(
            DescriptorControlInput::Standard(libc::STDIN_FILENO),
            "tests::descriptor_execution_relocates_input_fd_zero",
        );
    }

    #[test]
    fn descriptor_execution_relocates_input_fd_one() {
        descriptor_execution_control(
            DescriptorControlInput::Standard(libc::STDOUT_FILENO),
            "tests::descriptor_execution_relocates_input_fd_one",
        );
    }

    #[test]
    fn descriptor_execution_relocates_input_fd_two() {
        descriptor_execution_control(
            DescriptorControlInput::Standard(libc::STDERR_FILENO),
            "tests::descriptor_execution_relocates_input_fd_two",
        );
    }

    #[test]
    fn descriptor_execution_preserves_ordinary_input_fd() {
        descriptor_execution_control(
            DescriptorControlInput::Ordinary,
            "tests::descriptor_execution_preserves_ordinary_input_fd",
        );
    }

    #[test]
    fn descriptor_execution_refuses_standard_command_conversion() {
        let retained = sealed_self_executable();
        let descriptor = unsafe { libc::fcntl(retained.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
        assert!(descriptor > libc::STDERR_FILENO);
        let mut command = Command::new("/diagnostic/path/must-not-be-executed");
        command
            .executable(unsafe { OwnedFd::from_raw_fd(descriptor) })
            .unwrap();
        let error = command.try_into_std().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "cannot convert to std::process::Command without losing: executable file descriptor"
        );
    }

    #[tokio::test]
    async fn spawn() {
        assert_eq!(
            Command::new("true").spawn().unwrap().wait().await.unwrap(),
            ExitStatus::Exited(0)
        );

        assert_eq!(
            Command::new("false").spawn().unwrap().wait().await.unwrap(),
            ExitStatus::Exited(1)
        );
    }

    #[test]
    fn wait_blocking() {
        assert_eq!(
            Command::new("true")
                .spawn()
                .unwrap()
                .wait_blocking()
                .unwrap(),
            ExitStatus::Exited(0)
        );

        assert_eq!(
            Command::new("false")
                .spawn()
                .unwrap()
                .wait_blocking()
                .unwrap(),
            ExitStatus::Exited(1)
        );
    }

    #[tokio::test]
    async fn spawn_fail() {
        assert_eq!(
            Command::new("/iprobablydonotexist").spawn().unwrap_err(),
            Error::new(Errno::ENOENT, Context::Exec)
        );
    }

    #[tokio::test]
    async fn double_wait() {
        let mut child = Command::new("true").spawn().unwrap();
        assert_eq!(child.wait().await.unwrap(), ExitStatus::Exited(0));
        assert_eq!(child.wait().await.unwrap(), ExitStatus::Exited(0));
    }

    #[tokio::test]
    async fn output() {
        let output = Command::new("echo")
            .arg("foo")
            .arg("bar")
            .output()
            .await
            .unwrap();
        assert_eq!(output.stdout, b"foo bar\n");
        assert_eq!(output.stderr, b"");
        assert_eq!(output.status, ExitStatus::Exited(0));
    }

    fn parse_proc_status(stdout: &[u8]) -> BTreeMap<&str, &str> {
        from_utf8(stdout)
            .unwrap()
            .trim_end()
            .split('\n')
            .map(|line| {
                let (first, second) = line.split_once(':').unwrap();
                (first, second.trim())
            })
            .collect()
    }

    #[tokio::test]
    async fn uid_namespace() {
        let output = Command::new("cat")
            .arg("/proc/self/status")
            .map_root()
            .output()
            .await
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));

        let proc_status = parse_proc_status(&output.stdout);

        // We should be root user inside of the container.
        assert_eq!(proc_status["Uid"], "0\t0\t0\t0");
    }

    #[tokio::test]
    async fn pid_namespace() {
        let output = Command::new("cat")
            .arg("/proc/self/status")
            .map_root()
            .unshare(Namespace::PID)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));

        let proc_status = parse_proc_status(&output.stdout);

        assert_eq!(proc_status["NSpid"].split('\t').nth(1), Some("1"),);

        // Note that, since we haven't mounted a fresh /proc into the container,
        // the child still sees what the parent sees and so the PID will *not*
        // be 1.
        assert_ne!(proc_status["Pid"], "1");
    }

    #[tokio::test]
    async fn mount_proc() {
        let output = Command::new("cat")
            .arg("/proc/self/status")
            .map_root()
            .unshare(Namespace::PID)
            .mount(Mount::proc())
            .output()
            .await
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));

        let proc_status = parse_proc_status(&output.stdout);

        // With /proc mounted, the child really believes it is the root process.
        assert_eq!(proc_status["NSpid"], "1");
        assert_eq!(proc_status["Pid"], "1");
    }

    #[tokio::test]
    async fn hostname() {
        let output = Command::new("cat")
            .arg("/proc/sys/kernel/hostname")
            .map_root()
            .hostname("foobar.local")
            .output()
            .await
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));

        let hostname = from_utf8(&output.stdout).unwrap().trim();

        assert_eq!(hostname, "foobar.local");
    }

    #[tokio::test]
    async fn domainname() {
        let output = Command::new("cat")
            .arg("/proc/sys/kernel/domainname")
            .map_root()
            .domainname("foobar")
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));

        let domainname = from_utf8(&output.stdout).unwrap().trim();

        assert_eq!(domainname, "foobar");
    }

    #[tokio::test]
    async fn pty() {
        use tokio::io::AsyncReadExt;

        let mut pty = Pty::open().unwrap();
        let pty_child = pty.child().unwrap();

        let mut tty = pty_child.terminal_params().unwrap();
        // Prevent post-processing of output so `\n` isn't translated to `\r\n`.
        tty.c_oflag &= !libc::OPOST;
        pty_child.set_terminal_params(&tty).unwrap();

        pty_child.set_window_size(40, 80).unwrap();

        // stty is in coreutils and should be available on most systems.
        let mut child = Command::new("stty")
            .arg("size")
            .pty(pty_child)
            .spawn()
            .unwrap();

        // NOTE: read_to_end returns an EIO error once the child has exited.
        let mut buf = Vec::new();
        assert!(pty.read_to_end(&mut buf).await.is_err());

        assert_eq!(from_utf8(&buf).unwrap(), "40 80\n");

        assert_eq!(child.wait().await.unwrap(), ExitStatus::SUCCESS);
    }

    #[tokio::test]
    async fn mount_devpts_basic() {
        let output = Command::new("ls")
            .arg("/dev/pts")
            .map_root()
            .mount(Mount::devpts("/dev/pts"))
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));

        // Should be totally empty except for `/dev/pts/ptmx` since we mounted a
        // new devpts.
        assert_eq!(output.stderr, b"");
        assert_eq!(output.stdout, b"ptmx\n");
    }

    #[tokio::test]
    async fn mount_devpts_isolated() {
        let output = Command::new("ls")
            .arg("/dev/pts")
            .map_root()
            .mount(Mount::devpts("/dev/pts").data("newinstance,ptmxmode=0666"))
            .mount(Mount::bind("/dev/pts/ptmx", "/dev/ptmx"))
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));

        // Should be totally empty except for `/dev/pts/ptmx` since we mounted a
        // new devpts.
        assert_eq!(output.stderr, b"");
        assert_eq!(output.stdout, b"ptmx\n");
    }

    #[tokio::test]
    async fn mount_tmpfs() {
        let mount = "type=tmpfs,target=/tmp"
            .parse::<Mount>()
            .expect("tmpfs mount syntax should parse");
        let output = Command::new("ls")
            .arg("/tmp")
            .map_root()
            .mount(mount)
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));

        // Should be totally empty since we mounted a new tmpfs.
        assert_eq!(output.stderr, b"");
        assert_eq!(output.stdout, b"");
    }

    #[tokio::test]
    async fn mount_and_move_tmpfs() {
        let tmpfs = tempfile::tempdir().unwrap();

        // Create a temporary directory that will be the only thing to remain in
        // the `/tmp` mount.
        let persistent = tempfile::tempdir().unwrap();
        fs::write(persistent.path().join("foobar"), b"").unwrap();

        let output = Command::new("ls")
            .arg("/tmp")
            .map_root()
            .mount(Mount::tmpfs(tmpfs.path()))
            // Bind-mount a directory from our upper /tmp to our new /tmp.
            .mount(Mount::bind(persistent.path(), tmpfs.path().join("my-dir")).touch_target())
            // Move our newly-created tmpfs to hide the upper /tmp folder.
            .mount(Mount::rename(tmpfs.path(), Path::new("/tmp")))
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));

        // The only thing there should be our bind-mounted directory.
        assert_eq!(output.stderr, b"");
        assert_eq!(output.stdout, b"my-dir\n");
    }

    #[tokio::test]
    async fn mount_bind() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");

        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();

        fs::write(a.join("foobar"), "im a test").unwrap();

        let output = Command::new("ls")
            .arg(&b)
            .map_root()
            .mount(Mount::bind(&a, &b))
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(output.stdout, b"foobar\n");
        assert_eq!(output.stderr, b"");
    }

    #[tokio::test]
    async fn mount_bind_readonly_rejects_writes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("data"), "original").unwrap();

        let output = Command::new("sh")
            .args(["-c", "printf changed > \"$TARGET\""])
            .env("TARGET", target.join("data"))
            .map_root()
            .mount(Mount::bind(&source, &target).readonly())
            .output()
            .await
            .unwrap();

        assert_ne!(output.status, ExitStatus::Exited(0));
        assert_eq!(fs::read(source.join("data")).unwrap(), b"original");
    }

    #[tokio::test]
    async fn local_networking_ping() {
        const CHILD_ENV: &str = "REVERIE_PROCESS_LOOPBACK_TEST_CHILD";

        if std::env::var_os(CHILD_ENV).is_some() {
            let socket = std::net::UdpSocket::bind("[::1]:0").unwrap();
            let address = socket.local_addr().unwrap();
            assert_eq!(socket.send_to(b"ping", address).unwrap(), 4);

            let mut buffer = [0; 4];
            let (length, source) = socket.recv_from(&mut buffer).unwrap();
            assert_eq!(source, address);
            assert_eq!(&buffer[..length], b"ping");
            return;
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::local_networking_ping")
            .env(CHILD_ENV, "1")
            .map_root()
            .local_networking_only()
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0), "{:?}", output);
    }

    #[tokio::test]
    async fn local_networking_loopback_flags() {
        let output = Command::new("cat")
            .arg("/sys/class/net/lo/flags")
            .map_root()
            .local_networking_only()
            .output()
            .await
            .unwrap();

        assert_eq!(output.status, ExitStatus::Exited(0), "{:?}", output);
        assert_eq!(output.stdout, b"0x9\n", "{:?}", output);
    }

    /// Show that processes in two separate network namespaces can bind to the
    /// same port.
    #[tokio::test]
    async fn port_isolation() {
        use std::thread::sleep;
        use std::time::Duration;

        let mut command = Command::new("nc");
        command
            .arg("-l")
            .arg("127.0.0.1")
            // Can bind to a low port without real root inside the namespace.
            .arg("80")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .map_root()
            .local_networking_only();

        let server1 = match command.spawn() {
            // If netcat is not installed just exit successfully.
            Err(error) if error.errno() == Errno::ENOENT => return,
            other => other,
        }
        .unwrap();

        let server2 = command.spawn().unwrap();

        // Give them both time to start up.
        sleep(Duration::from_millis(100));

        // Stop them with a signal that cannot be ignored. A test binary launched
        // as a background shell job inherits SIGINT as ignored, and that
        // disposition survives exec into nc.
        server1.signal(Signal::SIGKILL).unwrap();
        server2.signal(Signal::SIGKILL).unwrap();

        let (output1, output2) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(1), server1.wait_with_output()),
            tokio::time::timeout(Duration::from_secs(1), server2.wait_with_output()),
        );
        let output1 = output1
            .expect("port_isolation: server 1 did not exit within 1 second after SIGKILL")
            .unwrap();
        let output2 = output2
            .expect("port_isolation: server 2 did not exit within 1 second after SIGKILL")
            .unwrap();

        // Without network isolation, one of the servers would exit with an
        // "Address already in use" (exit status 2) error.
        assert_eq!(
            output1.status,
            ExitStatus::Signaled(Signal::SIGKILL, false),
            "{:?}",
            output1
        );
        assert_eq!(
            output2.status,
            ExitStatus::Signaled(Signal::SIGKILL, false),
            "{:?}",
            output2
        );
    }

    /// Make sure we can call `.local_networking_only` more than once.
    #[tokio::test]
    async fn local_networking_there_can_be_only_one() {
        let output = Command::new("true")
            .map_root()
            .local_networking_only()
            // If calling this twice mounted /sys twice, then we'd get a "Device
            // or resource busy" error.
            .local_networking_only()
            .output()
            .await
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0), "{:?}", output);
        assert_eq!(output.stdout, b"", "{:?}", output);
        assert_eq!(output.stderr, b"", "{:?}", output);
    }

    #[test]
    fn from_std_lossy() {
        let mut stdcmd = std::process::Command::new("echo");
        stdcmd.args(["arg1", "arg2"]);
        stdcmd.current_dir("/foo/bar");
        stdcmd.env_clear();
        stdcmd.env("FOO", "1");
        stdcmd.env("BAR", "2");

        let cmd = Command::from_std_lossy(&stdcmd);

        assert_eq!(cmd.get_program(), "echo");
        assert_eq!(cmd.get_arg0(), "echo");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["arg1", "arg2"]);

        let envs = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v.and_then(|v| v.to_str()))))
            .collect::<Vec<_>>();
        assert_eq!(envs, [("BAR", Some("2")), ("FOO", Some("1"))]);
    }

    #[test]
    fn into_std_lossy_compatibility() {
        let mut cmd = Command::new("env");
        cmd.args(["-0"]);
        cmd.current_dir("/foo/bar");
        cmd.env_clear();
        cmd.env("FOO", "1");
        cmd.env("BAR", "2");

        let stdcmd = cmd.into_std_lossy();

        assert_eq!(stdcmd.get_program(), "env");
        assert_eq!(stdcmd.get_args().collect::<Vec<_>>(), ["-0"]);

        let envs = stdcmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v.and_then(|v| v.to_str()))))
            .collect::<Vec<_>>();

        assert_eq!(envs, [("BAR", Some("2")), ("FOO", Some("1"))]);
    }

    #[test]
    fn try_into_std_refuses_container_configuration() {
        use syscalls::Sysno;

        use super::seccomp::Action;
        use super::seccomp::FilterBuilder;

        let filter = FilterBuilder::new()
            .default_action(Action::Allow)
            .syscalls([(Sysno::brk, Action::KillProcess)])
            .build();
        let mut command = Command::new("true");
        command.seccomp(filter);
        let error = command.try_into_std().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "cannot convert to std::process::Command without losing: seccomp filter"
        );

        let filter = FilterBuilder::new()
            .default_action(Action::Allow)
            .syscalls([(Sysno::brk, Action::KillProcess)])
            .build();
        let mut command = Command::new("true");
        command.seccomp(filter);
        let panic =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| command.into_std_lossy()))
                .unwrap_err();
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("legacy conversion panic must carry a string diagnostic");
        assert_eq!(
            message,
            "Command::into_std_lossy refused unsupported configuration: cannot convert to std::process::Command without losing: seccomp filter"
        );

        let mut command = Command::new("true");
        command.unshare(Namespace::MOUNT);
        let error = command.try_into_std().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "cannot convert to std::process::Command without losing: Linux namespaces"
        );

        let mut command = Command::new("true");
        command.container.affinity(0);
        let error = command.try_into_std().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "cannot convert to std::process::Command without losing: CPU affinity"
        );
    }

    #[tokio::test]
    async fn seccomp() {
        use syscalls::Sysno;

        use super::seccomp::*;

        let filter = FilterBuilder::new()
            .default_action(Action::Allow)
            .syscalls([(Sysno::brk, Action::KillProcess)])
            .build();

        let output = Command::new("cat")
            .arg("/proc/self/status")
            .seccomp(filter)
            .output()
            .await
            .unwrap();
        assert!(
            matches!(output.status, ExitStatus::Signaled(Signal::SIGSYS, _)),
            "Expected Signaled(SIGSYS, _), got {:?}",
            output.status
        );
    }

    #[tokio::test]
    async fn seccomp_notify() {
        use std::collections::HashMap;

        use futures::future::Either;
        use futures::future::select;
        use futures::stream::TryStreamExt;
        use syscalls::Sysno;

        use super::seccomp::*;

        let filter = FilterBuilder::new()
            .default_action(Action::Notify)
            .syscalls([
                // FIXME: Because the first execve happens when the child is
                // spawned, we must allow this through. Otherwise, the
                // `.spawn()` below will deadlock because we can't process
                // seccomp notifications until after it returns.
                (Sysno::execve, Action::Allow),
            ])
            .build();

        let mut child = Command::new("cat")
            .arg("/proc/self/status")
            .seccomp(filter)
            .seccomp_notify()
            .stdout(Stdio::null())
            .spawn()
            .unwrap();

        let mut summary = HashMap::new();

        let exit_status = {
            let seccomp_notif = child.seccomp_notif.take();

            let notifier = async {
                if let Some(mut notifier) = seccomp_notif {
                    while let Some(notif) = notifier.try_next().await.unwrap() {
                        *summary.entry(Sysno::from(notif.data.nr)).or_insert(0u64) += 1;

                        // Simply let the syscall through.
                        let resp = seccomp_notif_resp {
                            id: notif.id,
                            val: 0,
                            error: 0,
                            flags: SECCOMP_USER_NOTIF_FLAG_CONTINUE,
                        };
                        notifier.send(&resp).unwrap();
                    }
                }
            };

            let exit_status = child.wait();

            futures::pin_mut!(notifier);
            futures::pin_mut!(exit_status);

            match select(notifier, exit_status).await {
                Either::Left((_, _)) => unreachable!(),
                Either::Right((exit_status, _)) => exit_status.unwrap(),
            }
        };

        assert_eq!(exit_status, ExitStatus::SUCCESS);

        assert!(summary[&Sysno::read] > 0);
        assert!(summary[&Sysno::write] > 0);
        assert!(summary[&Sysno::close] > 0);
    }
}
