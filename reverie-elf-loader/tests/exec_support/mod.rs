/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Ordinary, bounded native/preparation children for LB. Host-policy input is
//! deliberately MODELED: it tests the inactive implementation, and does not
//! attest this machine's BPF, integrity, watch or binfmt policy for activation.

#![allow(dead_code)] // Each integration binary uses a different part of this harness.

use std::ffi::CString;
use std::fs::File;
use std::fs::{self};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie_elf_loader::ExecCheckOutcome;
use reverie_elf_loader::ExecRequest;
use reverie_elf_loader::Limits;
use reverie_elf_loader::LoaderHostFacts;
use reverie_elf_loader::PrepareExecOptions;
use reverie_elf_loader::exec::E2bigClassification;
use reverie_elf_loader::exec::FileIdentity;
use reverie_elf_loader::host::BinfmtAuthority;
use reverie_elf_loader::host::BinfmtRegistry;
use reverie_elf_loader::host::BinfmtRegistryIdentity;
use reverie_elf_loader::host::BpfEvidence;
use reverie_elf_loader::host::EvidenceOrigin;
use reverie_elf_loader::host::ExecContextEvidence;
use reverie_elf_loader::host::FrozenHostAttestation;
use reverie_elf_loader::host::HostEvidence;
use reverie_elf_loader::host::HostQualification;
use reverie_elf_loader::host::IntegrityEvidence;
use reverie_elf_loader::host::MountEvidence;
use reverie_elf_loader::host::NamespaceBinfmtEvidence;
use reverie_elf_loader::host::PolicyReceipt;
use reverie_elf_loader::host::PreContentWatchEvidence;
use reverie_elf_loader::host::ProcessContext;
use reverie_elf_loader::host::SealedMemfdEvidence;
use reverie_elf_loader::host::SecurityEvidence;
use reverie_elf_loader::host::admitted_lookup_filesystem;
use reverie_elf_loader::prepare_exec;
use reverie_elf_loader::prepare_exec_raw;

const SPEC_ENV: &str = "REVERIE_LB_EXEC_SPEC";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub enum RawFault {
    #[default]
    None,
    Path,
    Argv,
    Envp,
    ArgumentString,
    EnvironmentString,
}

#[derive(Clone, Debug, Default)]
pub struct ChildSetup {
    pub cwd: Option<PathBuf>,
    pub inherited_fds: Vec<RawFd>,
    pub writer_fds: Vec<RawFd>,
    pub raw_fault: RawFault,
    pub fill_fd_table: bool,
    pub inherited_virtual_proc_state: bool,
    pub sealed_memfd: Option<(RawFd, i32)>,
    pub trace_library: Option<PathBuf>,
    pub closed_fds: Vec<RawFd>,
    pub stack_limit: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareObservation {
    NativeErrno {
        errno: i32,
        original_check: bool,
        e2big: Option<E2bigClassification>,
    },
    Refusal(String),
    Prepared(PreparedObservation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedObservation {
    pub argv: Vec<Vec<u8>>,
    pub execfn: Vec<u8>,
    pub scripts: usize,
    pub original_check: i32,
    pub program_inode: u64,
    pub interpreter_inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeObservation {
    Errno(i32),
    Exited(i32),
    Signaled(i32),
}

pub fn fixture_dir(name: &str) -> PathBuf {
    let path = Path::new(env!("ELF_LOADER_ARTIFACT_DIR")).join(name);
    fs::create_dir_all(&path).unwrap();
    path
}

pub fn prepare_observation(
    request: &ExecRequest,
    cwd: Option<&Path>,
    inherited_fds: &[RawFd],
    directory: &Path,
) -> PrepareObservation {
    run_prepare(
        request,
        &ChildSetup {
            cwd: cwd.map(Path::to_owned),
            inherited_fds: inherited_fds.to_vec(),
            ..ChildSetup::default()
        },
        directory,
    )
}

pub fn run_prepare(
    request: &ExecRequest,
    setup: &ChildSetup,
    directory: &Path,
) -> PrepareObservation {
    let (report, status, _) = run_child(request, setup, directory, 0);
    assert!(status.success(), "preparation child failed: {status}");
    decode_observation(&report)
}

/// Run with public libc open/statx symbol tracing. The returned log does not
/// observe kernel-internal CHECK opens or unhooked direct caller syscalls.
pub fn run_prepare_with_trace(
    request: &ExecRequest,
    setup: &ChildSetup,
    directory: &Path,
) -> (PrepareObservation, PathBuf) {
    assert!(setup.trace_library.is_some(), "tracing library is required");
    let (report, status, trace_path) = run_child(request, setup, directory, 0);
    assert!(status.success(), "preparation child failed: {status}");
    (decode_observation(&report), trace_path)
}

pub fn run_native(
    request: &ExecRequest,
    setup: &ChildSetup,
    directory: &Path,
) -> NativeObservation {
    let (report, status, _) = run_child(request, setup, directory, 1);
    if report.len() > 1 {
        assert!(status.success(), "native errno child failed: {status}");
        assert_eq!(report.len(), 5, "malformed native errno report");
        assert_eq!(report[0], 1);
        return NativeObservation::Errno(i32::from_le_bytes(report[1..5].try_into().unwrap()));
    }
    assert_eq!(report, [0], "native child never reached exec");
    match status.code() {
        Some(code) => NativeObservation::Exited(code),
        None => NativeObservation::Signaled(status.signal().unwrap()),
    }
}

pub fn run_check(request: &ExecRequest, setup: &ChildSetup, directory: &Path) -> i32 {
    let (report, status, _) = run_child(request, setup, directory, 2);
    assert!(status.success(), "CHECK child failed: {status}");
    i32::from_le_bytes(report.try_into().unwrap())
}

fn run_child(
    request: &ExecRequest,
    setup: &ChildSetup,
    directory: &Path,
    operation: u8,
) -> (Vec<u8>, std::process::ExitStatus, PathBuf) {
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stem = format!("exec-{}-{sequence}-{operation}", std::process::id());
    let spec_path = directory.join(format!("{stem}.spec"));
    let result_path = directory.join(format!("{stem}.result"));
    let output_path = directory.join(format!("{stem}.output"));
    let trace_path = directory.join(format!("{stem}.opens"));
    let mut inherited = setup.inherited_fds.clone();
    inherited.extend(&setup.writer_fds);
    if let Some((fd, _)) = setup.sealed_memfd {
        inherited.push(fd);
    }
    if request.dirfd != libc::AT_FDCWD && unsafe { libc::fcntl(request.dirfd, libc::F_GETFD) } >= 0
    {
        inherited.push(request.dirfd);
    }
    inherited.sort_unstable();
    inherited.dedup();
    let flags: Vec<_> = inherited
        .iter()
        .map(|fd| {
            let flag = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            assert!(flag >= 0, "invalid inherited fixture fd: {fd}");
            (*fd, flag)
        })
        .collect();
    let mut spec = Vec::new();
    spec.push(operation);
    put_i32(&mut spec, request.dirfd);
    put_i32(&mut spec, request.flags);
    put_bytes(&mut spec, request.path.as_bytes());
    put_strings(&mut spec, &request.argv);
    put_strings(&mut spec, &request.envp);
    put_bytes(&mut spec, result_path.as_os_str().as_encoded_bytes());
    put_usize(&mut spec, flags.len());
    for (fd, flag) in &flags {
        put_i32(&mut spec, *fd);
        put_i32(&mut spec, *flag);
    }
    put_usize(&mut spec, setup.writer_fds.len());
    for fd in &setup.writer_fds {
        put_i32(&mut spec, *fd);
    }
    spec.push(setup.raw_fault as u8);
    spec.push(u8::from(setup.fill_fd_table));
    spec.push(u8::from(setup.inherited_virtual_proc_state));
    if let Some((fd, seals)) = setup.sealed_memfd {
        spec.push(1);
        put_i32(&mut spec, fd);
        put_i32(&mut spec, seals);
    } else {
        spec.push(0);
    }
    put_usize(&mut spec, setup.closed_fds.len());
    for fd in &setup.closed_fds {
        put_i32(&mut spec, *fd);
    }
    match setup.stack_limit {
        Some(limit) => {
            spec.push(1);
            spec.extend(limit.to_le_bytes());
        }
        None => spec.push(0),
    }
    fs::write(&spec_path, spec).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "lb_prepare_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SPEC_ENV, spec_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&output_path).unwrap()))
        .stderr(Stdio::from(
            File::create(output_path.with_extension("stderr")).unwrap(),
        ));
    if let Some(cwd) = &setup.cwd {
        command.current_dir(cwd);
    }
    if let Some(library) = &setup.trace_library {
        command.env("LD_PRELOAD", library);
        command.env("REVERIE_LB_OPEN_TRACE", &trace_path);
    }
    // SAFETY: pre_exec only changes the listed inherited flags using fcntl.
    // Exact original flags are restored in the fresh ordinary test process.
    unsafe {
        command.pre_exec(move || {
            for (fd, flag) in &flags {
                if libc::fcntl(*fd, libc::F_SETFD, flag & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!("LB child exceeded 2 seconds ({operation}): {status}, {output_path:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let report = fs::read(result_path).unwrap_or_else(|error| {
        panic!("child produced no report: {status}, {error}, {output_path:?}")
    });
    (report, status, trace_path)
}

/// Entry in a fresh one-test ordinary process. The parent returns immediately
/// when the spec is absent, so this is also a harmless normal cargo test.
pub fn prepare_child_entry() {
    let Some(spec_path) = std::env::var_os(SPEC_ENV) else {
        return;
    };
    let no_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &no_core) }, 0);
    let spec = fs::read(spec_path).unwrap();
    let mut input = Decoder(&spec);
    let operation = input.byte();
    let dirfd = input.i32();
    let flags = input.i32();
    let path = CString::new(input.bytes()).unwrap();
    let argv = input.strings();
    let envp = input.strings();
    let result_path = PathBuf::from(std::ffi::OsString::from_vec(input.bytes()));
    let inherited = input.usize();
    for _ in 0..inherited {
        let fd = input.i32();
        let flag = input.i32();
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, flag) }, 0);
    }
    let writer_count = input.usize();
    let writer_fds: Vec<_> = (0..writer_count).map(|_| input.i32()).collect();
    let fault = input.byte();
    let full = input.byte() != 0;
    let inherited_virtual_proc_state = input.byte() != 0;
    let sealed_memfd = (input.byte() != 0).then(|| (input.i32(), input.i32()));
    let close_count = input.usize();
    let closed_fds: Vec<_> = (0..close_count).map(|_| input.i32()).collect();
    let stack_limit = (input.byte() != 0).then(|| input.usize() as u64);
    assert!(input.0.is_empty());
    let result_file = File::create(&result_path).unwrap();
    // Keep the report outside low-number request/placeholder test cases.
    let report_fd = unsafe { libc::fcntl(result_file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
    assert!(report_fd >= 256);
    let mut report = unsafe { File::from_raw_fd(report_fd) };
    drop(result_file);
    let request = ExecRequest {
        dirfd,
        path,
        argv,
        envp,
        flags,
    };
    let mut argv = pointers(&request.argv);
    let mut envp = pointers(&request.envp);
    let path_ptr = if fault == RawFault::Path as u8 {
        std::ptr::dangling::<libc::c_char>()
    } else {
        request.path.as_ptr()
    };
    if fault == RawFault::ArgumentString as u8 {
        argv = vec![std::ptr::dangling(), std::ptr::null()];
    }
    if fault == RawFault::EnvironmentString as u8 {
        envp = vec![std::ptr::dangling(), std::ptr::null()];
    }
    let argv_ptr = if fault == RawFault::Argv as u8 {
        std::ptr::dangling::<*const libc::c_char>()
    } else {
        argv.as_ptr()
    };
    let envp_ptr = if fault == RawFault::Envp as u8 {
        std::ptr::dangling::<*const libc::c_char>()
    } else {
        envp.as_ptr()
    };
    // Capture modeled qualification and loader facts before filling the table.
    let host = modeled_host(sealed_memfd);
    let loader_host = LoaderHostFacts::current().unwrap();
    let limits = Limits::current().unwrap();
    if let Some(limit) = stack_limit {
        let value = libc::rlimit {
            rlim_cur: limit,
            rlim_max: limit,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_STACK, &value) }, 0);
    }
    let options = PrepareExecOptions {
        launcher_link: Path::new("/lb"),
        host: &host,
        limits,
        loader_host: &loader_host,
        inherited_virtual_proc_state,
        interpreter_writer_fds: &writer_fds,
    };
    let filled = if full {
        fill_table(report.as_raw_fd())
    } else {
        Vec::new()
    };
    for fd in closed_fds {
        if unsafe { libc::close(fd) } < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
    }
    match operation {
        0 => {
            // SAFETY: only this child test allocates descriptors during the
            // call; its buffers are stable. Modeled host inputs are explicitly
            // limited to inactive tests, never production qualification.
            let outcome = unsafe {
                if fault == RawFault::None as u8 {
                    prepare_exec(&request, &options)
                } else {
                    prepare_exec_raw(dirfd, path_ptr, argv_ptr, envp_ptr, flags, &options)
                }
            };
            let observation = match outcome {
                ExecCheckOutcome::NativeErrno(error) => PrepareObservation::NativeErrno {
                    errno: error.errno,
                    original_check: error.stage
                        == reverie_elf_loader::exec::NativeErrorStage::OriginalCheck,
                    e2big: error.e2big,
                },
                ExecCheckOutcome::Refuse(refusal) => {
                    eprintln!("preparation refusal: {refusal:?}");
                    PrepareObservation::Refusal(refusal.name().into())
                }
                ExecCheckOutcome::Prepared(start) => {
                    start.verify_objects().unwrap();
                    PrepareObservation::Prepared(PreparedObservation {
                        argv: start
                            .arguments
                            .argv
                            .iter()
                            .map(|s| s.as_bytes().to_vec())
                            .collect(),
                        execfn: start.arguments.execfn.as_bytes().to_vec(),
                        scripts: start.scripts.len(),
                        original_check: start.evidence.original_check.unwrap(),
                        program_inode: start.program_identity.inode,
                        interpreter_inode: start.interpreter_identity.inode,
                    })
                }
            };
            report.write_all(&encode_observation(&observation)).unwrap();
        }
        1 | 2 => {
            if operation == 1 {
                report.write_all(&[0]).unwrap();
                report.flush().unwrap();
                assert_eq!(
                    unsafe { libc::fcntl(report.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
                    0
                );
            }
            // Native exec and CHECK receive exactly the same user input as
            // preparation, including faults. This process may change image.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_execveat,
                    dirfd,
                    path_ptr,
                    argv_ptr,
                    envp_ptr,
                    flags
                        | if operation == 2 {
                            reverie_elf_loader::exec::AT_EXECVE_CHECK
                        } else {
                            0
                        },
                )
            };
            let errno = if result < 0 {
                std::io::Error::last_os_error().raw_os_error().unwrap()
            } else {
                0
            };
            if operation == 1 {
                assert_ne!(errno, 0, "a successful native exec cannot return");
                // Replace the attempt marker with the returned native error.
                report.set_len(0).unwrap();
                use std::io::Seek;
                use std::io::SeekFrom;
                report.seek(SeekFrom::Start(0)).unwrap();
                report.write_all(&[1]).unwrap();
                report.write_all(&errno.to_le_bytes()).unwrap();
            } else {
                report.write_all(&errno.to_le_bytes()).unwrap();
            }
        }
        _ => panic!("invalid child operation"),
    }
    drop(filled);
}

pub fn model_receipt() -> PolicyReceipt {
    PolicyReceipt {
        approval_id: "MODELED-LB-native-comparison-tests".into(),
        scope: "MODEL ONLY: admitted mounts, empty binfmt, inactive BPF/integrity, no watches"
            .into(),
        lifetime: "MODEL ONLY: stable for one inactive preparation call".into(),
        policy_digest: [31; 32],
        generation: 1,
    }
}

pub fn modeled_host(sealed_memfd: Option<(RawFd, i32)>) -> HostQualification {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let filtered: String = mountinfo
        .lines()
        .filter(|line| {
            let (_, tail) = line.split_once(" - ").unwrap();
            admitted_lookup_filesystem(tail.split_ascii_whitespace().next().unwrap())
        })
        .map(|line| format!("{line}\n"))
        .collect();
    let namespace = |name: &str| fs::metadata(format!("/proc/self/ns/{name}")).unwrap().ino();
    let context = ProcessContext {
        namespace_pid: std::process::id(),
        mount_namespace: namespace("mnt"),
        user_namespace: namespace("user"),
        pid_namespace: namespace("pid"),
        real_uid: unsafe { libc::getuid() },
        effective_uid: unsafe { libc::geteuid() },
        real_gid: unsafe { libc::getgid() },
        effective_gid: unsafe { libc::getegid() },
        securebits: unsafe { libc::prctl(libc::PR_GET_SECUREBITS) } as u32,
        inheritable_capabilities: 0,
        bounding_capabilities: 0,
        permitted_capabilities: 0,
        no_new_privs: unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 0,
    };
    let registry = BinfmtRegistryIdentity {
        owner_user_namespace: context.user_namespace,
        mount_id: 1,
        policy_digest: [32; 32],
        generation: 1,
    };
    let mut endpoints = Vec::new();
    if let Some((fd, seals)) = sealed_memfd {
        // Borrow the live fixture descriptor without closing or duplicating it.
        let file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
        let identity = FileIdentity::of(&file).unwrap();
        endpoints.push(SealedMemfdEvidence {
            mount_id: identity.mount_id,
            inode: identity.inode,
            seals,
            receipt: model_receipt(),
        });
    }
    let evidence = HostEvidence {
        mounts: MountEvidence::parse_complete(filtered.as_bytes(), Vec::new()).unwrap(),
        security: SecurityEvidence {
            active_lsms: Some(vec!["capability".into()]),
            bpf: BpfEvidence::Inactive,
            integrity: IntegrityEvidence::Inactive,
        },
        binfmt: BinfmtAuthority::NearestAncestor {
            ancestry: vec![NamespaceBinfmtEvidence {
                user_namespace: context.user_namespace,
                parent: None,
                registry: Some(BinfmtRegistry {
                    identity: registry.clone(),
                    enabled: true,
                    entries: Vec::new(),
                }),
            }],
            visible_registry: registry,
        },
        context: ExecContextEvidence::Current(context),
        watches: PreContentWatchEvidence::AbsentForLifetime(model_receipt()),
        sealed_memfds: endpoints,
    };
    unsafe {
        HostQualification::from_frozen_evidence(
            evidence,
            FrozenHostAttestation {
                origin: EvidenceOrigin::Modeled {
                    fixture: "LB ordinary native/preparation children".into(),
                },
                receipt: model_receipt(),
                context_digest: [33; 32],
            },
        )
        .unwrap()
    }
}

fn fill_table(anchor: RawFd) -> Vec<File> {
    let limit = libc::rlimit {
        rlim_cur: 128,
        rlim_max: 128,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut descriptors = Vec::new();
    loop {
        let fd = unsafe { libc::fcntl(anchor, libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EMFILE)
            );
            break;
        }
        descriptors.push(unsafe { File::from_raw_fd(fd) });
    }
    descriptors
}

fn pointers(strings: &[CString]) -> Vec<*const libc::c_char> {
    strings
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect()
}

fn put_i32(output: &mut Vec<u8>, value: i32) {
    output.extend(value.to_le_bytes());
}
fn put_usize(output: &mut Vec<u8>, value: usize) {
    output.extend((value as u64).to_le_bytes());
}
fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    put_usize(output, bytes.len());
    output.extend(bytes);
}
fn put_strings(output: &mut Vec<u8>, strings: &[CString]) {
    put_usize(output, strings.len());
    for string in strings {
        put_bytes(output, string.as_bytes());
    }
}

struct Decoder<'a>(&'a [u8]);
impl Decoder<'_> {
    fn byte(&mut self) -> u8 {
        let value = self.0[0];
        self.0 = &self.0[1..];
        value
    }
    fn i32(&mut self) -> i32 {
        let value = i32::from_le_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        value
    }
    fn usize(&mut self) -> usize {
        let value = u64::from_le_bytes(self.0[..8].try_into().unwrap()) as usize;
        self.0 = &self.0[8..];
        value
    }
    fn bytes(&mut self) -> Vec<u8> {
        let len = self.usize();
        let value = self.0[..len].to_vec();
        self.0 = &self.0[len..];
        value
    }
    fn strings(&mut self) -> Vec<CString> {
        let count = self.usize();
        (0..count)
            .map(|_| CString::new(self.bytes()).unwrap())
            .collect()
    }
}

fn encode_observation(observation: &PrepareObservation) -> Vec<u8> {
    let mut bytes = Vec::new();
    match observation {
        PrepareObservation::NativeErrno {
            errno,
            original_check,
            e2big,
        } => {
            bytes.push(0);
            put_i32(&mut bytes, *errno);
            bytes.push(u8::from(*original_check));
            bytes.push(match e2big {
                None => 0,
                Some(E2bigClassification::NativeBudget) => 1,
                Some(E2bigClassification::BothModelsPass) => 2,
                Some(E2bigClassification::Unclassified) => 3,
            });
        }
        PrepareObservation::Refusal(name) => {
            bytes.push(1);
            put_bytes(&mut bytes, name.as_bytes());
        }
        PrepareObservation::Prepared(start) => {
            bytes.push(2);
            put_usize(&mut bytes, start.argv.len());
            for argument in &start.argv {
                put_bytes(&mut bytes, argument);
            }
            put_bytes(&mut bytes, &start.execfn);
            put_usize(&mut bytes, start.scripts);
            put_i32(&mut bytes, start.original_check);
            bytes.extend(start.program_inode.to_le_bytes());
            bytes.extend(start.interpreter_inode.to_le_bytes());
        }
    }
    bytes
}

fn decode_observation(bytes: &[u8]) -> PrepareObservation {
    let mut input = Decoder(bytes);
    let observation = match input.byte() {
        0 => {
            let errno = input.i32();
            let original_check = input.byte() != 0;
            let e2big = match input.byte() {
                0 => None,
                1 => Some(E2bigClassification::NativeBudget),
                2 => Some(E2bigClassification::BothModelsPass),
                3 => Some(E2bigClassification::Unclassified),
                _ => panic!("invalid classifier"),
            };
            PrepareObservation::NativeErrno {
                errno,
                original_check,
                e2big,
            }
        }
        1 => PrepareObservation::Refusal(String::from_utf8(input.bytes()).unwrap()),
        2 => {
            let argc = input.usize();
            let argv = (0..argc).map(|_| input.bytes()).collect();
            let execfn = input.bytes();
            let scripts = input.usize();
            let original_check = input.i32();
            let program_inode = input.usize() as u64;
            let interpreter_inode = input.usize() as u64;
            PrepareObservation::Prepared(PreparedObservation {
                argv,
                execfn,
                scripts,
                original_check,
                program_inode,
                interpreter_inode,
            })
        }
        _ => panic!("invalid preparation observation"),
    };
    assert!(input.0.is_empty());
    observation
}
