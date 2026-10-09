/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Interpreter descriptor-view and genuine relative-CWD mount controls.
//! All target syscalls run in the shared parent's monitored ordinary helper.

mod exec_support;

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use exec_support::ChildSetup;
use exec_support::MountedCwd;
use exec_support::NativeObservation;
use exec_support::PrepareObservation;
use exec_support::RawFault;
use reverie_elf_loader::ExecRequest;

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn lb_prepare_child() {
    exec_support::prepare_child_entry();
}

fn directory(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join(format!("{name}-{}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}

fn request(path: &Path) -> ExecRequest {
    ExecRequest::execve(path, vec![CString::new("guest-argv0").unwrap()], Vec::new()).unwrap()
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn native_interpreter() -> PathBuf {
    let bytes = fs::read("/bin/true").unwrap();
    let header = interpreter_header(&bytes);
    let offset = u64::from_le_bytes(bytes[header + 8..header + 16].try_into().unwrap()) as usize;
    let length = u64::from_le_bytes(bytes[header + 32..header + 40].try_into().unwrap()) as usize;
    PathBuf::from(std::ffi::OsStr::from_bytes(
        &bytes[offset..offset + length - 1],
    ))
}

fn interpreter_header(bytes: &[u8]) -> usize {
    let start = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
    let size = usize::from(u16::from_le_bytes(bytes[54..56].try_into().unwrap()));
    (0..usize::from(u16::from_le_bytes(bytes[56..58].try_into().unwrap())))
        .map(|index| start + index * size)
        .find(|offset| u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap()) == 3)
        .unwrap()
}

fn with_interpreter(directory: &Path, name: &str, interpreter: &Path) -> PathBuf {
    let mut bytes = fs::read("/bin/true").unwrap();
    let header = interpreter_header(&bytes);
    let offset = bytes.len() as u64;
    let name_bytes = CString::new(interpreter.as_os_str().as_bytes()).unwrap();
    let length = name_bytes.as_bytes_with_nul().len() as u64;
    bytes.extend_from_slice(name_bytes.as_bytes_with_nul());
    bytes[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&length.to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&length.to_le_bytes());
    let path = directory.join(name);
    executable(&path, &bytes);
    path
}

#[test]
fn lb2_interpreter_symlink_and_repeated_slash_aliases_preserve_descriptor_errors() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory("lb-interpreter-alias");
    fs::create_dir(directory.join("cwd")).unwrap();
    std::os::unix::fs::symlink("/proc/self/fd", directory.join("cwd/alias")).unwrap();
    let setup = ChildSetup {
        cwd: Some(directory.join("cwd")),
        closed_fds: vec![3],
        ..ChildSetup::default()
    };
    for (index, interpreter, refusal) in [
        ("alias/3/child", "LookupMountUnverified"),
        ("alias//3///child", "LookupMountUnverified"),
        ("//proc/self//fd///3/child", "LookupMountUnverified"),
        ("/proc//self//fd//3/child", "LookupMountUnverified"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (path, refusal))| (index, path, refusal))
    {
        let target = with_interpreter(&directory, &format!("T{index}"), Path::new(interpreter));
        let script = directory.join(format!("S{index}"));
        executable(&script, format!("#!{interpreter}\n").as_bytes());
        for path in [target, script] {
            let request = request(&path);
            assert_eq!(exec_support::run_check(&request, &setup, &directory), 0);
            assert_eq!(
                exec_support::run_native(&request, &setup, &directory),
                NativeObservation::Errno(libc::ENOENT)
            );
            assert_eq!(
                exec_support::run_prepare(&request, &setup, &directory),
                PrepareObservation::Refusal(refusal.into()),
                "private reservations must never produce a native ENOTDIR"
            );
        }
    }

    // Ordinary filesystem symlinks and redundant slashes remain usable.
    let interpreter = native_interpreter();
    fs::copy(&interpreter, directory.join("local-I")).unwrap();
    std::os::unix::fs::symlink("../local-I", directory.join("cwd/ordinary-link")).unwrap();
    for (index, interpreter) in [
        PathBuf::from("ordinary-link"),
        PathBuf::from(format!("////{}", interpreter.display())),
    ]
    .into_iter()
    .enumerate()
    {
        let target = with_interpreter(&directory, &format!("control{index}"), &interpreter);
        let request = request(&target);
        assert_eq!(
            exec_support::run_native(&request, &setup, &directory),
            NativeObservation::Exited(0)
        );
        assert!(matches!(
            exec_support::run_prepare(&request, &setup, &directory),
            PrepareObservation::Prepared(_)
        ));
    }
}

#[test]
fn lb2_generic_proc_aliases_are_refused_before_original_or_interpreter_lookup() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory("lb-generic-proc-alias");
    std::os::unix::fs::symlink("/proc/self", directory.join("proc-alias")).unwrap();
    let setup = ChildSetup {
        cwd: Some(directory.clone()),
        ..ChildSetup::default()
    };
    // status is an ordinary proc regular file, rather than a magic link. A
    // NO_MAGICLINKS-only probe would already traverse its unqualified handler.
    for (index, path) in [
        PathBuf::from("/proc/self/status"),
        PathBuf::from("//proc//self///status"),
        PathBuf::from("proc-alias/status"),
        PathBuf::from("proc-alias//status"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut paths = vec![path.clone()];
        paths.push(with_interpreter(&directory, &format!("T{index}"), &path));
        let script = directory.join(format!("S{index}"));
        executable(&script, format!("#!{}\n", path.display()).as_bytes());
        paths.push(script);
        for executable in paths {
            let request = request(&executable);
            assert_eq!(
                exec_support::run_native(&request, &setup, &directory),
                NativeObservation::Errno(libc::EACCES)
            );
            assert_eq!(
                exec_support::run_prepare(&request, &setup, &directory),
                PrepareObservation::Refusal("LookupMountUnverified".into())
            );
        }
    }
}

#[test]
fn lb1_raw_filename_copy_unavailable_refuses_before_lookup() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory("lb-raw-filename-copy");
    let setup = ChildSetup {
        raw_fault: RawFault::WriteOnlyPath,
        ..ChildSetup::default()
    };
    for (path, check, native) in [
        ("/bin/true", 0, NativeObservation::Exited(0)),
        (
            "/proc/self/status",
            libc::EACCES,
            NativeObservation::Errno(libc::EACCES),
        ),
        (
            "//proc//self///status",
            libc::EACCES,
            NativeObservation::Errno(libc::EACCES),
        ),
    ] {
        let request = request(Path::new(path));
        // On x86-64 getname can read a PROT_WRITE mapping while remote GUP
        // cannot. The helper independently requires process_vm_readv EFAULT.
        assert_eq!(exec_support::run_check(&request, &setup, &directory), check);
        assert_eq!(
            exec_support::run_native(&request, &setup, &directory),
            native
        );
        assert_eq!(
            exec_support::run_prepare(&request, &setup, &directory),
            PrepareObservation::Refusal("PreparationIo".into())
        );
    }
}

fn compare_cwd(directory: &Path, target: &Path, source: &Path) {
    for detached in [false, true] {
        let mount_point = directory.join(if detached { "detached" } else { "listed" });
        fs::create_dir(&mount_point).unwrap();
        let setup = ChildSetup {
            mounted_cwd: Some(MountedCwd {
                mount_point,
                source: source.into(),
                interpreter_name: "interpreter".into(),
                detached,
            }),
            ..ChildSetup::default()
        };
        let request = request(target);
        assert!(request.path.as_bytes().starts_with(b"/"));
        assert_eq!(exec_support::run_check(&request, &setup, directory), 0);
        assert_eq!(
            exec_support::run_native(&request, &setup, directory),
            NativeObservation::Exited(0),
            "native interpreter lookup uses the retained CWD even after detach"
        );
        let prepared = exec_support::run_prepare(&request, &setup, directory);
        if detached {
            assert_eq!(
                prepared,
                PrepareObservation::Refusal("LookupMountUnverified".into())
            );
        } else {
            assert!(
                matches!(prepared, PrepareObservation::Prepared(_)),
                "{prepared:?}"
            );
        }
    }
}

#[test]
fn lb5_relative_pt_interp_qualifies_listed_and_detached_cwd_mounts() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory("lb-relative-elf");
    let target = with_interpreter(&directory, "T", Path::new("interpreter"));
    compare_cwd(&directory, &target, &native_interpreter());
}

#[test]
fn lb5_relative_script_interpreter_qualifies_listed_and_detached_cwd_mounts() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory("lb-relative-script");
    let script = directory.join("S");
    executable(&script, b"#!interpreter\n");
    compare_cwd(&directory, &script, Path::new("/bin/true"));

    // A later rewrite must qualify the CWD as well as the first rewrite.
    let nested = directory.join("nested");
    fs::create_dir(&nested).unwrap();
    let outer = nested.join("S");
    executable(&outer, format!("#!{}\n", script.display()).as_bytes());
    compare_cwd(&nested, &outer, Path::new("/bin/true"));
}
