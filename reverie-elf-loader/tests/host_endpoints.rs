/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Genuine complete mount/proc qualification, separate from security models.
//!
//! A bounded child creates private user/mount/PID namespaces, mounts fresh
//! procfs, pivots to an admitted root under target and detaches the inherited
//! root. Namespace init supervises this fixture in a child above PID 1. Its
//! complete, unfiltered mountinfo contains the root and genuine procfs, with
//! a separate leaf case adding an admitted file bind mount.
//! No live security/binfmt/watch qualification is claimed.

use std::ffi::CStr;
use std::ffi::CString;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use reverie_elf_loader::ExecCheckOutcome;
use reverie_elf_loader::ExecRefusal;
use reverie_elf_loader::ExecRequest;
use reverie_elf_loader::Limits;
use reverie_elf_loader::LoaderHostFacts;
use reverie_elf_loader::PrepareExecOptions;
use reverie_elf_loader::exec::FileIdentity;
use reverie_elf_loader::host::BinfmtAuthority;
use reverie_elf_loader::host::BinfmtRegistry;
use reverie_elf_loader::host::BinfmtRegistryIdentity;
use reverie_elf_loader::host::BpfEvidence;
use reverie_elf_loader::host::EvidenceOrigin;
use reverie_elf_loader::host::ExecContextEvidence;
use reverie_elf_loader::host::FrozenHostAttestation;
use reverie_elf_loader::host::HostEvidence;
use reverie_elf_loader::host::HostPolicyRefusal;
use reverie_elf_loader::host::HostQualification;
use reverie_elf_loader::host::IntegrityEvidence;
use reverie_elf_loader::host::NamespaceBinfmtEvidence;
use reverie_elf_loader::host::PolicyReceipt;
use reverie_elf_loader::host::PreContentWatchEvidence;
use reverie_elf_loader::host::QualifiedProcEndpoints;
use reverie_elf_loader::host::RetainedLookupRoot;
use reverie_elf_loader::host::RetainedProcEndpoints;
use reverie_elf_loader::prepare_exec;

const CHILD_STAGE: &str = "REVERIE_LB_PROC_ENDPOINT_STAGE";
const FIXTURE_ROOT: &str = "REVERIE_LB_PROC_ENDPOINT_ROOT";
const FIXTURE_CASE: &str = "REVERIE_LB_PROC_ENDPOINT_CASE";

fn receipt() -> PolicyReceipt {
    PolicyReceipt {
        approval_id: "LB-genuine-isolated-proc-fixture".into(),
        scope: "constructed private root: genuine complete mounts and exact current-task proc endpoints only; security/binfmt/watch policies are unproved".into(),
        lifetime: "single serialized fixture observation without task, root, namespace or endpoint replacement".into(),
        policy_digest: [71; 32],
        generation: 1,
    }
}

fn fixture_directory(case: &str) -> PathBuf {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/lb-host-endpoints");
    fs::create_dir_all(&parent).unwrap();
    // Different sandbox invocations can reuse the same namespace PID. Never
    // reuse an earlier fixture's FIFO, symlink or mounted-leaf backing file.
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = parent.join(format!("{}-{generation}-{case}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    directory
}

fn copy_runtime(root: &Path) {
    let executable = std::env::current_exe().unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::copy(&executable, root.join("bin/host-endpoints")).unwrap();
    let dependencies = Command::new("ldd").arg(&executable).output().unwrap();
    assert!(dependencies.status.success());
    for field in std::str::from_utf8(&dependencies.stdout)
        .unwrap()
        .split_ascii_whitespace()
        .filter(|field| field.starts_with('/'))
    {
        let source = Path::new(field);
        let destination = root.join(source.strip_prefix("/").unwrap());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
}

fn run_namespace_fixture(case: &str) -> String {
    let directory = fixture_directory(case);
    let root = directory.join("root");
    fs::create_dir_all(root.join("proc/self")).unwrap();
    fs::create_dir_all(root.join("old")).unwrap();
    fs::create_dir_all(root.join("result")).unwrap();
    copy_runtime(&root);
    // Test bootstrap captures actual legacy loader facts before the private
    // preparation window. The child receives them from an admitted-root file;
    // preparation does not discover or open generic proc/sysctl endpoints.
    let loader_host = LoaderHostFacts::current().unwrap();
    let mut facts = loader_host.mmap_min_addr.to_le_bytes().to_vec();
    facts.push(u8::from(loader_host.mdwe_inherited));
    facts.extend_from_slice(&loader_host.cmdline);
    fs::write(root.join("result/loader-facts"), facts).unwrap();
    if case == "unverified-fifo" {
        let fifo = CString::new(root.join("proc/self/mountinfo").as_os_str().as_bytes()).unwrap();
        // SAFETY: fixture pathname lives under target and is NUL terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    }
    let mut child = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lb5_proc_endpoint_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_STAGE, "setup")
        .env(FIXTURE_ROOT, &root)
        .env(FIXTURE_CASE, case)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // All lookup/mount/exec work happens in the monitored child after spawn.
    // In particular, opening the deliberately unverified FIFO would block and
    // cause this assertion to fail, rather than blocking Command::spawn.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            // SAFETY: spawn put only this fixture and its descendants in the
            // dedicated process group. Killing namespace init also causes the
            // kernel to terminate every process in that private PID namespace.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("bounded proc endpoint fixture timed out: {case}: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    fs::write(directory.join("stdout"), &output.stdout).unwrap();
    fs::write(directory.join("stderr"), &output.stderr).unwrap();
    assert!(output.status.success(), "{case}: {output:?}");
    fs::read_to_string(root.join("result/observation")).unwrap()
}

fn mount(source: Option<&Path>, target: &Path, filesystem: Option<&CStr>, flags: libc::c_ulong) {
    let source = source.map(|path| CString::new(path.as_os_str().as_bytes()).unwrap());
    let target = CString::new(target.as_os_str().as_bytes()).unwrap();
    // SAFETY: constructed test pathnames and null unused arguments.
    assert_eq!(
        unsafe {
            libc::mount(
                source
                    .as_ref()
                    .map_or(std::ptr::null(), |source| source.as_ptr()),
                target.as_ptr(),
                filesystem.map_or(std::ptr::null(), CStr::as_ptr),
                flags,
                std::ptr::null(),
            )
        },
        0,
        "mount(source={source:?}, target={target:?}, filesystem={filesystem:?}, flags={flags:#x}): {}",
        std::io::Error::last_os_error()
    );
}

fn setup_namespace() -> ! {
    let root = PathBuf::from(std::env::var_os(FIXTURE_ROOT).unwrap());
    let case = std::env::var(FIXTURE_CASE).unwrap();
    assert_eq!(
        std::process::id(),
        1,
        "setup must be private namespace init"
    );
    mount(None, Path::new("/"), None, libc::MS_PRIVATE | libc::MS_REC);
    mount(Some(&root), &root, None, libc::MS_BIND);
    if case == "genuine" || case == "genuine-leaf" {
        // Fresh procfs is tied to this new PID namespace. An inherited proc
        // mount can contain locked submounts that cannot be excluded by a
        // nonrecursive bind after entering a less privileged user namespace.
        mount(
            Some(Path::new("proc")),
            &root.join("proc"),
            Some(c"proc"),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        );
    }
    if case == "genuine-leaf" {
        File::create(root.join("mount-leaf")).unwrap();
        mount(
            Some(&root.join("bin/host-endpoints")),
            &root.join("mount-leaf"),
            None,
            libc::MS_BIND,
        );
    }
    std::env::set_current_dir(&root).unwrap();
    // SAFETY: both directories belong to this private test namespace.
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c"old".as_ptr()) },
        0
    );
    std::env::set_current_dir("/").unwrap();
    // SAFETY: detach only the inherited root in this child's private namespace.
    assert_eq!(
        unsafe { libc::umount2(c"/old".as_ptr(), libc::MNT_DETACH) },
        0
    );
    // Namespace init remains alive to supervise the actual fixture at PID>1;
    // production's namespace-init refusal therefore remains exercised normally.
    let status = Command::new("/bin/host-endpoints")
        .args([
            "--exact",
            "lb5_proc_endpoint_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_STAGE, "observe")
        .status()
        .expect("spawn isolated fixture above namespace PID 1");
    assert!(status.success(), "isolated fixture child: {status}");
    std::process::exit(0)
}

fn openat(directory: &File, name: &str, flags: i32) -> File {
    let name = CString::new(name).unwrap();
    // SAFETY: fixture is constructed from a trusted admitted root and genuine
    // proc mount; every path below is an audited constant or numeric child.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
        )
    };
    assert!(fd >= 0, "openat: {}", std::io::Error::last_os_error());
    // SAFETY: newly owned descriptor returned by a successful openat.
    unsafe { File::from_raw_fd(fd) }
}

fn retain_genuine_endpoints() -> Arc<QualifiedProcEndpoints> {
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open("/proc")
        .unwrap();
    let process = openat(
        &root,
        &std::process::id().to_string(),
        libc::O_PATH | libc::O_DIRECTORY,
    );
    let mount_namespace = openat(&process, "ns/mnt", libc::O_RDONLY);
    let user_namespace = openat(&process, "ns/user", libc::O_RDONLY);
    let pid_namespace = openat(&process, "ns/pid", libc::O_RDONLY);
    let proc_pid_namespace = pid_namespace.metadata().unwrap().ino();
    let descriptors = RetainedProcEndpoints {
        fd_directory: openat(&process, "fd", libc::O_PATH | libc::O_DIRECTORY),
        fdinfo_directory: openat(&process, "fdinfo", libc::O_RDONLY | libc::O_DIRECTORY),
        executable_link: openat(&process, "exe", libc::O_PATH | libc::O_NOFOLLOW),
        executable: openat(&process, "exe", libc::O_PATH),
        mountinfo: openat(&process, "mountinfo", libc::O_RDONLY),
        status: openat(&process, "status", libc::O_RDONLY),
        root,
        process,
        mount_namespace,
        user_namespace,
        pid_namespace,
    };
    // SAFETY: private namespace was constructed above, inherited root detached
    // and this process started above PID 1 inside it. Setup mounted fresh real
    // procfs for this private PID namespace, so these are genuine exact
    // current-task endpoints. No namespace/path/task replacement follows.
    Arc::new(unsafe {
        QualifiedProcEndpoints::from_retained(
            descriptors,
            std::process::id(),
            proc_pid_namespace,
            receipt(),
        )
        .unwrap()
    })
}

fn native_check(request: &ExecRequest) -> i32 {
    let argv: Vec<_> = request
        .argv
        .iter()
        .map(|string| string.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp: Vec<_> = request
        .envp
        .iter()
        .map(|string| string.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    // SAFETY: exact request and live buffers in the parent's monitored child.
    // CHECK observes authorization without recursively executing this test.
    let result = unsafe {
        libc::syscall(
            libc::SYS_execveat,
            request.dirfd,
            request.path.as_ptr(),
            argv.as_ptr(),
            envp.as_ptr(),
            request.flags | reverie_elf_loader::exec::AT_EXECVE_CHECK,
        )
    };
    if result == 0 {
        0
    } else {
        assert_eq!(result, -1);
        std::io::Error::last_os_error().raw_os_error().unwrap()
    }
}

fn prepare_proc_companions(host: &HostQualification, executable: &File) {
    let facts = fs::read("/result/loader-facts").unwrap();
    assert!(facts.len() >= 9);
    assert!(facts[8] <= 1);
    let loader_host = LoaderHostFacts {
        mmap_min_addr: u64::from_le_bytes(facts[..8].try_into().unwrap()),
        mdwe_inherited: facts[8] == 1,
        cmdline: facts[9..].to_vec(),
    };
    let options = PrepareExecOptions {
        launcher_link: Path::new("/lb"),
        host,
        limits: Limits::current().unwrap(),
        loader_host: &loader_host,
        inherited_virtual_proc_state: false,
        interpreter_writer_fds: &[],
    };
    let expected = FileIdentity::of(executable).unwrap();
    let fd = executable.as_raw_fd();
    assert_ne!(fd, host.proc_executable().unwrap());
    assert_ne!(fd, host.proc_fd_directory().unwrap());
    let roots = host.retained_lookup_roots().unwrap();
    assert!(roots.iter().all(|root| root.directory.as_raw_fd() != fd));
    for path in [
        "/proc/self/exe".to_owned(),
        "//proc//self//exe".to_owned(),
        format!("/proc/self/fd/{fd}"),
        format!("//proc//self//fd//{fd}"),
    ] {
        let request = ExecRequest::execve(
            &path,
            vec![CString::new("genuine-proc-argv0").unwrap()],
            vec![CString::new("LB_GENUINE_PROC=exact-original-environment").unwrap()],
        )
        .unwrap();
        assert_eq!(native_check(&request), 0, "native CHECK companion: {path}");
        // SAFETY: ordinary serialized fixture with genuine complete mount,
        // proc and context facts. Only security/binfmt/watch policy is modeled;
        // this inactive call cannot authorize or invoke a production loader.
        let prepared = match unsafe { prepare_exec(&request, &options) } {
            ExecCheckOutcome::Prepared(prepared) => prepared,
            other => panic!("genuine proc preparation must be Prepared for {path}: {other:?}"),
        };
        assert_eq!(prepared.evidence.original_check, Some(0), "{path}");
        assert_eq!(
            prepared.program_identity, expected,
            "bound executable: {path}"
        );
        assert_eq!(
            prepared.program.metadata().unwrap().ino(),
            expected.inode,
            "{path}"
        );
        assert_eq!(
            prepared.arguments.execfn.to_bytes(),
            request.path.to_bytes(),
            "literal proc filename: {path}"
        );
        assert_eq!(
            prepared.start.native_execfn.to_bytes(),
            request.path.to_bytes(),
            "{path}"
        );
        assert!(prepared.scripts.is_empty());
        prepared.verify_objects().unwrap();
    }
    // Native CHECK sees these helper-owned descriptors in the actual test
    // process. Preparation must keep them out of its original guest view.
    let helper_fds = [
        (host.proc_executable().unwrap(), 0),
        (host.proc_fd_directory().unwrap(), libc::EACCES),
    ]
    .into_iter()
    .chain(roots.iter().map(|root| {
        (
            root.directory.as_raw_fd(),
            if root.directory.metadata().unwrap().is_dir() {
                libc::EACCES
            } else {
                0
            },
        )
    }));
    for (helper_fd, native_errno) in helper_fds {
        for path in [
            format!("/proc/self/fd/{helper_fd}"),
            format!("//proc//self//fd//{helper_fd}"),
        ] {
            let request = ExecRequest::execve(
                &path,
                vec![CString::new("helper-fd-original-argv0").unwrap()],
                Vec::new(),
            )
            .unwrap();
            assert_eq!(
                native_check(&request),
                native_errno,
                "native helper FD: {path}"
            );
            // SAFETY: same isolated inactive fixture and stable bootstrap
            // handles; the helper descriptor cannot represent a guest FD.
            let outcome = unsafe { prepare_exec(&request, &options) };
            assert!(
                matches!(
                    outcome,
                    ExecCheckOutcome::Refuse(ExecRefusal::Host(
                        HostPolicyRefusal::LookupMountUnverified { .. }
                    ))
                ),
                "helper-owned original proc FD must receive a named refusal: {path}: {outcome:?}"
            );
        }
    }
}

fn observe_genuine() {
    let leaf = std::env::var(FIXTURE_CASE).unwrap() == "genuine-leaf";
    std::os::unix::fs::symlink("/proc", "/proc-alias").unwrap();
    let endpoints = retain_genuine_endpoints();
    let mut evidence = HostEvidence::collect_current_from(&endpoints).unwrap();
    assert_eq!(
        evidence.mounts.records().len(),
        if leaf { 3 } else { 2 },
        "complete namespace must contain exactly new root, genuine proc and the declared leaf bind mount"
    );
    assert_eq!(
        evidence
            .mounts
            .records()
            .iter()
            .filter(|record| record.filesystem == "proc")
            .count(),
        1
    );
    evidence.mounts.qualify().unwrap();
    assert_eq!(
        evidence.mounts.proc_namespaces(),
        &[endpoints.evidence().clone()]
    );
    assert!(matches!(
        evidence
            .mounts
            .verify_mount_id(endpoints.evidence().mount_id),
        Err(HostPolicyRefusal::UnsafeLookupMount { .. })
    ));
    // Every complete row is retained. Removing endpoint authority must restore
    // the generic proc refusal, rather than deleting the proc row to pass.
    let unqualified = evidence.mounts.clone().with_proc_namespaces(Vec::new());
    assert!(matches!(
        unqualified.qualify(),
        Err(HostPolicyRefusal::UnsafeLookupMount { .. })
    ));
    for mutation in ["pid", "mount", "task", "receipt"] {
        let mut changed = endpoints.evidence().clone();
        match mutation {
            "pid" => changed.proc_pid_namespace += 1,
            "mount" => changed.mount_id += 1,
            "task" => changed.namespace_pid = 0,
            "receipt" => changed.receipt.lifetime.clear(),
            _ => unreachable!(),
        }
        assert!(
            evidence
                .mounts
                .clone()
                .with_proc_namespaces(vec![changed])
                .qualify()
                .is_err(),
            "{mutation} mutation must fail"
        );
    }
    // Live collection does not infer any privileged security or policy proof.
    assert!(evidence.security.active_lsms.is_none());
    assert!(matches!(
        HostQualification::collect_current_from(&endpoints),
        Err(HostPolicyRefusal::SecurityPolicyUnverified { .. })
    ));
    let context = match &evidence.context {
        ExecContextEvidence::Current(context) => context.clone(),
        other => panic!("genuine retained process context: {other:?}"),
    };
    assert_eq!(
        context.mount_namespace,
        endpoints.evidence().mount_namespace
    );
    assert_eq!(
        context.pid_namespace,
        endpoints.evidence().proc_pid_namespace
    );
    // Only the security/binfmt/watch half below is a labeled inactive model;
    // genuine full mount/proc evidence above is reused without filtering.
    evidence.security.active_lsms = Some(vec!["capability".into()]);
    evidence.security.bpf = BpfEvidence::Inactive;
    evidence.security.integrity = IntegrityEvidence::Inactive;
    let identity = BinfmtRegistryIdentity {
        owner_user_namespace: context.user_namespace,
        mount_id: evidence.mounts.records()[0].mount_id,
        policy_digest: [72; 32],
        generation: 1,
    };
    evidence.binfmt = BinfmtAuthority::NearestAncestor {
        visible_registry: identity.clone(),
        ancestry: vec![NamespaceBinfmtEvidence {
            user_namespace: context.user_namespace,
            parent: None,
            registry: Some(BinfmtRegistry {
                identity,
                enabled: true,
                entries: Vec::new(),
            }),
        }],
    };
    evidence.watches = PreContentWatchEvidence::AbsentForLifetime(receipt());
    let attestation = FrozenHostAttestation {
        origin: EvidenceOrigin::Modeled { fixture: "genuine complete mount/proc fixture; security/binfmt/watch only are modeled, never activation evidence".into() },
        receipt: receipt(),
        context_digest: [73; 32],
    };
    // SAFETY: this is an explicitly labeled inactive policy model. It reuses
    // real mount/proc/context evidence but cannot authorize production exec.
    let host =
        unsafe { HostQualification::from_frozen_evidence(evidence.clone(), attestation.clone()) }
            .unwrap()
            .with_proc_endpoints(endpoints.clone())
            .unwrap();
    let root_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open("/")
        .unwrap();
    // SAFETY: this exact admitted root was established by the fixture's pivot;
    // genuine complete mount evidence binds it and no root replacement occurs.
    let mut roots = vec![RetainedLookupRoot {
        mount_point: b"/".to_vec(),
        directory: root_directory,
    }];
    if leaf {
        let mount = evidence
            .mounts
            .records()
            .iter()
            .find(|record| record.mount_point == b"/mount-leaf")
            .expect("complete real snapshot includes declared leaf bind mount");
        assert_eq!(mount.filesystem, evidence.mounts.records()[0].filesystem);
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open("/mount-leaf")
            .unwrap();
        assert!(directory.metadata().unwrap().is_file());
        roots.push(RetainedLookupRoot {
            mount_point: b"/mount-leaf".to_vec(),
            directory,
        });
    }
    // SAFETY: setup created these exact roots in the private namespace before
    // qualification. The complete unfiltered table includes every mount.
    let host = unsafe { host.with_lookup_roots(roots).unwrap() };
    for (path, expected) in [
        ("/", "."),
        ("/bin/host-endpoints", "bin/host-endpoints"),
        ("///bin//host-endpoints", "bin//host-endpoints"),
        ("/ leading space", " leading space"),
    ] {
        let (_, relative) = host
            .absolute_lookup_root(&CString::new(path).unwrap())
            .unwrap();
        assert_eq!(relative.to_bytes(), expected.as_bytes());
    }
    if leaf {
        let (fd, relative) = host.absolute_lookup_root(c"/mount-leaf").unwrap();
        assert!(
            relative.to_bytes().is_empty(),
            "a leaf mount must use its retained object, never '.'"
        );
        let empty = [std::ptr::null::<libc::c_char>()];
        // SAFETY: both CHECKs use stable exact fixture inputs inside the
        // parent's monitored child. The retained leaf and original pathname
        // must have the same native classification without an extra lookup.
        for (dirfd, path, flags) in [
            (libc::AT_FDCWD, c"/mount-leaf", 0),
            (fd, c"", libc::AT_EMPTY_PATH),
        ] {
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_execveat,
                        dirfd,
                        path.as_ptr(),
                        empty.as_ptr(),
                        empty.as_ptr(),
                        flags | reverie_elf_loader::exec::AT_EXECVE_CHECK,
                    )
                },
                0
            );
        }
        for path in [c"/mount-leaf/child", c"/mount-leaf/"] {
            assert!(
                matches!(
                    host.absolute_lookup_root(path),
                    Err(HostPolicyRefusal::LookupMountUnverified { .. })
                ),
                "leaf suffix must be a named refusal: {path:?}"
            );
            // SAFETY: exact native companion in the monitored ordinary child.
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_execveat,
                        libc::AT_FDCWD,
                        path.as_ptr(),
                        empty.as_ptr(),
                        empty.as_ptr(),
                        reverie_elf_loader::exec::AT_EXECVE_CHECK,
                    )
                },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOTDIR)
            );
        }
        let (_, relative) = host.absolute_lookup_root(c"/mount-leaf-next").unwrap();
        assert_eq!(
            relative.to_bytes(),
            b"mount-leaf-next",
            "component-boundary prefix must keep the ordinary directory root"
        );
    }
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x01 | 0x02, // NO_XDEV | NO_MAGICLINKS.
    };
    for path in [
        "/proc/self/status",
        "/proc//self//status",
        "/proc-alias/self/status",
        "/proc-alias//self//status",
    ] {
        let (directory, relative) = host
            .absolute_lookup_root(&CString::new(path).unwrap())
            .unwrap();
        // SAFETY: generated relative CString and exact openat2 ABI. This must
        // fail at the proc mount crossing before looking up a generic endpoint.
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    directory,
                    relative.as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EXDEV),
            "{path}"
        );
    }
    let proc_directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open("/proc")
        .unwrap();
    // SAFETY: rejection control for the already-qualified genuine proc root;
    // exception authority must not turn it into an admitted generic lookup root.
    assert!(matches!(
        unsafe {
            host.clone().with_lookup_roots(vec![RetainedLookupRoot {
                mount_point: b"/proc".to_vec(),
                directory: proc_directory,
            }])
        },
        Err(HostPolicyRefusal::LookupMountUnverified { .. })
    ));
    let executable = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open("/bin/host-endpoints")
        .unwrap();
    let name = CString::new(executable.as_raw_fd().to_string()).unwrap();
    // SAFETY: exact generated FD child below the retained genuine proc FD dir.
    let reopened = unsafe {
        libc::openat(
            host.proc_fd_directory().unwrap(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    assert!(reopened >= 0);
    // SAFETY: successful openat returned a newly owned readable descriptor.
    let mut reopened = unsafe { File::from_raw_fd(reopened) };
    let mut elf_magic = [0_u8; 4];
    reopened.read_exact(&mut elf_magic).unwrap();
    assert_eq!(elf_magic, *b"\x7fELF");
    assert_eq!(
        reopened.metadata().unwrap().ino(),
        executable.metadata().unwrap().ino()
    );
    assert_eq!(
        // SAFETY: read-only identity query on the retained executable.
        unsafe { libc::fcntl(host.proc_executable().unwrap(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        libc::FD_CLOEXEC
    );
    let empty = [std::ptr::null::<libc::c_char>()];
    // SAFETY: CHECK only, retained current executable and valid empty vectors.
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_execveat,
                host.proc_executable().unwrap(),
                c"".as_ptr(),
                empty.as_ptr(),
                empty.as_ptr(),
                libc::AT_EMPTY_PATH | reverie_elf_loader::exec::AT_EXECVE_CHECK,
            )
        },
        0
    );
    prepare_proc_companions(&host, &executable);
    let mut changed_context = evidence.clone();
    if let ExecContextEvidence::Current(context) = &mut changed_context.context {
        context.mount_namespace += 1;
    }
    assert!(matches!(
        unsafe { HostQualification::from_frozen_evidence(changed_context, attestation) },
        Err(HostPolicyRefusal::ProcEndpointUnverified { .. })
    ));
    let rows: Vec<_> = evidence
        .mounts
        .records()
        .iter()
        .map(|record| {
            format!(
                "{} {} {}",
                record.mount_id,
                record.filesystem,
                String::from_utf8_lossy(&record.mount_point)
            )
        })
        .collect();
    fs::write("/result/observation", format!("PASS genuine complete unfiltered namespace: {}\nretained exe CHECK and numeric fd reopen; four exact native CHECK/preparation guest proc path companions including repeated slashes; Prepared original_check=0 and full executable identity preserved; helper-owned proc endpoint/root FD paths have native CHECK companions and named LookupMountUnverified refusals; missing/mismatched authority controls fail; security authority remains unproved\n", rows.join("; "))).unwrap();
}

#[test]
fn lb5_proc_endpoint_child() {
    match std::env::var(CHILD_STAGE).ok().as_deref() {
        None => {}
        Some("setup") => setup_namespace(),
        Some("observe") => {
            if std::env::var(FIXTURE_CASE).unwrap() == "unverified-fifo" {
                for collect in [
                    HostEvidence::collect_current().map(|_| ()),
                    HostQualification::collect_current().map(|_| ()),
                ] {
                    assert!(matches!(
                        collect,
                        Err(HostPolicyRefusal::ProcEndpointUnverified { .. })
                    ));
                }
                fs::write("/result/observation", "PASS ProcEndpointUnverified before opening rebound mountinfo FIFO; parent timeout bounds attempted blocking lookup\n").unwrap();
            } else {
                observe_genuine();
            }
        }
        other => panic!("invalid fixture stage: {other:?}"),
    }
}

#[test]
fn lb5_genuine_complete_proc_namespace_and_exact_retained_endpoints() {
    assert!(
        run_namespace_fixture("genuine").starts_with("PASS genuine complete unfiltered namespace:")
    );
}

#[test]
fn lb5_unverified_proc_endpoint_is_refused_before_opening_rebound_fifo() {
    assert!(
        run_namespace_fixture("unverified-fifo")
            .starts_with("PASS ProcEndpointUnverified before opening rebound mountinfo FIFO")
    );
}

#[test]
fn lb5_genuine_leaf_bind_mount_uses_exact_retained_object() {
    let report = run_namespace_fixture("genuine-leaf");
    assert!(report.starts_with("PASS genuine complete unfiltered namespace:"));
    assert!(report.contains(" /mount-leaf"));
}
