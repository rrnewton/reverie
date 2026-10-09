/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Public libc-symbol instrumentation for inactive LB2 preparation. Every
//! ordinary child has a two-second bound; no Hermit or launcher executes.
//! The tracer records at most 4096 events, each in an 8192-byte buffer, and
//! leaves no persistent descriptor. Complete evidence must stay below that
//! cap and have a final footer proving every attempted event was written.
//! Kernel-internal CHECK opens are outside
//! this instrumentation: Linux may_open(MAY_EXEC) rejects nonregular files
//! before vfs_open (6.17 fs/namei.c:3439-3466,3885-3887; fs/exec.c:764-802).

mod exec_support;

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::File;
use std::fs::{self};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use exec_support::ChildSetup;
use exec_support::NativeObservation;
use exec_support::PrepareObservation;
use reverie_elf_loader::ExecRequest;
use reverie_elf_loader::host::HostPolicyRefusal;
use reverie_elf_loader::host::HostQualification;
use reverie_elf_loader::host::MountEvidence;

static SERIAL: Mutex<()> = Mutex::new(());
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const TRACE_MODE: &str = "REVERIE_LB_TRACE_MODE";
const TRACE_TARGET: &str = "REVERIE_LB_TRACE_TARGET";
const TRACE_RESULT: &str = "REVERIE_LB_TRACE_RESULT";

unsafe extern "C" {
    #[link_name = "open"]
    fn control_open(path: *const libc::c_char, flags: libc::c_int, ...) -> libc::c_int;
    #[link_name = "open64"]
    fn control_open64(path: *const libc::c_char, flags: libc::c_int, ...) -> libc::c_int;
    #[link_name = "openat"]
    fn control_openat(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        flags: libc::c_int,
        ...
    ) -> libc::c_int;
    #[link_name = "openat64"]
    fn control_openat64(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        flags: libc::c_int,
        ...
    ) -> libc::c_int;
}

#[test]
fn lb_prepare_child() {
    exec_support::prepare_child_entry();
}

fn directory() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target");
    loop {
        let ordinal = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = root.join(format!("lb-open-{}-{ordinal}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => return directory,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create open-order fixture directory: {error}"),
        }
    }
}

fn cstring(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn request(path: &Path) -> ExecRequest {
    ExecRequest::execve(path, vec![CString::new("guest-argv0").unwrap()], Vec::new()).unwrap()
}

fn with_interpreter(directory: &Path, name: &str, interpreter: &Path) -> PathBuf {
    let mut bytes = fs::read("/bin/true").unwrap();
    let phoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
    let phentsize = usize::from(u16::from_le_bytes(bytes[54..56].try_into().unwrap()));
    let phnum = usize::from(u16::from_le_bytes(bytes[56..58].try_into().unwrap()));
    let header = (0..phnum)
        .map(|index| phoff + index * phentsize)
        .find(|offset| u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap()) == 3)
        .expect("native true has PT_INTERP");
    let offset = bytes.len() as u64;
    let interpreter = cstring(interpreter);
    let length = interpreter.as_bytes_with_nul().len() as u64;
    bytes.extend_from_slice(interpreter.as_bytes_with_nul());
    bytes[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&length.to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&length.to_le_bytes());
    let path = directory.join(name);
    executable(&path, &bytes);
    path
}

#[derive(Debug)]
struct Event {
    operation: String,
    dirfd: i32,
    flags: i32,
    result: i64,
    errno: i32,
    path: Vec<u8>,
}

impl Event {
    fn is_open(&self) -> bool {
        matches!(
            self.operation.as_str(),
            "open" | "open64" | "openat" | "openat64"
        )
    }

    fn is_read_open(&self) -> bool {
        self.is_open()
            && self.flags & libc::O_PATH == 0
            && self.flags & libc::O_ACCMODE != libc::O_WRONLY
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TraceError {
    Io(io::ErrorKind),
    CapacityExceeded,
    MalformedRecord,
    MissingFooter,
    NonterminalFooter,
    MissingConstructor,
    Saturated {
        attempted: u64,
        logged: u64,
        failures: u64,
    },
    LostEvents {
        attempted: u64,
        logged: u64,
        failures: u64,
    },
    CountMismatch {
        attempted: u64,
        logged: u64,
        parsed: usize,
    },
    IncompleteMarker,
}

type TraceEvidence = Result<Vec<Event>, TraceError>;

fn events(path: &Path) -> TraceEvidence {
    let bytes = fs::read(path).map_err(|error| TraceError::Io(error.kind()))?;
    if bytes.len() > 4096 * 8192 + 256 {
        return Err(TraceError::CapacityExceeded);
    }
    if !bytes.ends_with(b"\n") {
        return Err(TraceError::MissingFooter);
    }
    let lines: Vec<_> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    let mut parsed = Vec::new();
    let mut footer = None;
    for (index, line) in lines.iter().enumerate() {
        let fields: Vec<_> = line.splitn(6, |byte| *byte == b'\t').collect();
        if fields[0] == b"footer" {
            if fields.len() != 5 {
                return Err(TraceError::MalformedRecord);
            }
            if index + 1 != lines.len() || footer.is_some() {
                return Err(TraceError::NonterminalFooter);
            }
            let number = |index: usize| {
                std::str::from_utf8(fields[index])
                    .ok()
                    .and_then(|field| field.parse::<u64>().ok())
                    .ok_or(TraceError::MalformedRecord)
            };
            footer = Some((number(1)?, number(2)?, number(3)?, number(4)?));
            continue;
        }
        if fields.len() != 6 {
            return Err(TraceError::MalformedRecord);
        }
        let number = |index: usize| {
            std::str::from_utf8(fields[index])
                .ok()
                .and_then(|field| field.parse::<i64>().ok())
                .ok_or(TraceError::MalformedRecord)
        };
        parsed.push(Event {
            operation: std::str::from_utf8(fields[0])
                .map_err(|_| TraceError::MalformedRecord)?
                .to_owned(),
            dirfd: i32::try_from(number(1)?).map_err(|_| TraceError::MalformedRecord)?,
            flags: u32::try_from(number(2)?).map_err(|_| TraceError::MalformedRecord)? as i32,
            result: number(3)?,
            errno: i32::try_from(number(4)?).map_err(|_| TraceError::MalformedRecord)?,
            path: fields[5].to_vec(),
        });
    }
    let (attempted, logged, failures, complete) = footer.ok_or(TraceError::MissingFooter)?;
    // Equality is saturation too: capped evidence cannot prove absence of a
    // later forbidden event. Every admitted trace has strictly spare capacity.
    if attempted >= 4096 || parsed.len() >= 4096 {
        return Err(TraceError::Saturated {
            attempted,
            logged,
            failures,
        });
    }
    if failures != 0 {
        return Err(TraceError::LostEvents {
            attempted,
            logged,
            failures,
        });
    }
    if attempted != logged || logged != parsed.len() as u64 {
        return Err(TraceError::CountMismatch {
            attempted,
            logged,
            parsed: parsed.len(),
        });
    }
    if complete != 1 {
        return Err(TraceError::IncompleteMarker);
    }
    if parsed
        .iter()
        .filter(|event| event.operation == "initialized")
        .count()
        != 1
    {
        return Err(TraceError::MissingConstructor);
    }
    Ok(parsed)
}

fn procfd(path: &[u8]) -> Option<i64> {
    let descriptor = path.strip_prefix(b"/proc/self/fd/")?;
    std::str::from_utf8(descriptor).ok()?.parse().ok()
}

fn no_nonregular_read_open(trace: &TraceEvidence, target: &Path) -> bool {
    let Ok(events) = trace else {
        return false;
    };
    let target = target.as_os_str().as_bytes();
    let mut pinned = BTreeSet::new();
    for event in events {
        let same_object =
            event.path == target || procfd(&event.path).is_some_and(|fd| pinned.contains(&fd));
        if same_object && event.is_read_open() {
            return false;
        }
        if same_object && event.is_open() && event.flags & libc::O_PATH != 0 && event.result >= 0 {
            pinned.insert(event.result);
        }
    }
    true
}

fn no_target_traversal(trace: &TraceEvidence, target: &Path) -> bool {
    let Ok(events) = trace else {
        return false;
    };
    events.iter().all(|event| {
        event.path != target.as_os_str().as_bytes()
            || !(event.is_open() || event.operation == "statx")
    })
}

fn preparation_trace(directory: &Path, path: &Path, expected: PrepareObservation) -> TraceEvidence {
    let request = request(path);
    assert_eq!(
        exec_support::run_native(&request, &ChildSetup::default(), directory),
        NativeObservation::Errno(libc::EACCES)
    );
    let setup = ChildSetup {
        trace_library: Some(PathBuf::from(env!("ELF_LOADER_OPEN_TRACE"))),
        ..ChildSetup::default()
    };
    let (prepared, trace) = exec_support::run_prepare_with_trace(&request, &setup, directory);
    assert_eq!(prepared, expected);
    let evidence = events(&trace);
    assert!(
        evidence.is_ok(),
        "incomplete preparation trace: {evidence:?}"
    );
    evidence
}

fn native_eacces(original_check: bool) -> PrepareObservation {
    PrepareObservation::NativeErrno {
        errno: libc::EACCES,
        original_check,
        e2big: None,
    }
}

fn mutation_child(directory: &Path, mode: &str, target: &Path) -> TraceEvidence {
    let trace = directory.join(format!("{mode}.opens"));
    let result = directory.join(format!("{mode}.result"));
    let output = directory.join(format!("{mode}.stdout"));
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lb_trace_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(TRACE_MODE, mode)
        .env(TRACE_TARGET, target)
        .env(TRACE_RESULT, &result)
        .env("LD_PRELOAD", env!("ELF_LOADER_OPEN_TRACE"))
        .env("REVERIE_LB_OPEN_TRACE", &trace)
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(output).unwrap()))
        .stderr(Stdio::from(
            File::create(directory.join(format!("{mode}.stderr"))).unwrap(),
        ))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!("instrumentation child exceeded two seconds: {mode}, {status}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        status.success(),
        "instrumentation child failed: {mode}, {status}"
    );
    assert_eq!(fs::read(result).unwrap(), b"PASS\n");
    let evidence = events(&trace);
    if mode != "trace-saturation" {
        assert!(
            evidence.is_ok(),
            "incomplete {mode} mutation trace: {evidence:?}"
        );
    }
    evidence
}

fn close_opened(fd: i32) {
    assert!(
        fd >= 0,
        "mutation fixture open: {}",
        io::Error::last_os_error()
    );
    assert_eq!(unsafe { libc::close(fd) }, 0);
}

fn stat_target(path: &CString) {
    let mut metadata = std::mem::MaybeUninit::<libc::statx>::zeroed();
    assert_eq!(
        unsafe {
            libc::statx(
                libc::AT_FDCWD,
                path.as_ptr(),
                0,
                libc::STATX_TYPE | libc::STATX_INO,
                metadata.as_mut_ptr(),
            )
        },
        0
    );
}

fn descriptor_count() -> usize {
    (0..1024)
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .count()
}

#[test]
fn lb_trace_child() {
    let Some(mode) = std::env::var_os(TRACE_MODE) else {
        return;
    };
    let target = PathBuf::from(std::env::var_os(TRACE_TARGET).unwrap());
    let name = cstring(&target);
    let before = descriptor_count();
    let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC;
    match mode.to_str().unwrap() {
        "trace-complete-control" => {
            close_opened(unsafe { control_open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) });
        }
        "trace-saturation" => {
            // A forbidden readable open follows the cap. Its successful
            // syscall must not become accepted absence-of-open evidence.
            for _ in 0..4096 {
                close_opened(unsafe {
                    control_open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC)
                });
            }
            close_opened(unsafe { control_open(name.as_ptr(), flags) });
        }
        "fifo-read-open" | "device-read-open" => {
            close_opened(unsafe { control_open(name.as_ptr(), flags) });
        }
        "pinned-fifo-read-open" => {
            let fd = unsafe { control_open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            assert!(fd >= 0);
            let endpoint = CString::new(format!("/proc/self/fd/{fd}")).unwrap();
            close_opened(unsafe { control_open(endpoint.as_ptr(), flags) });
            close_opened(fd);
        }
        "hook-controls" => {
            close_opened(unsafe { control_open(name.as_ptr(), flags) });
            close_opened(unsafe { control_open64(name.as_ptr(), flags) });
            close_opened(unsafe { control_openat(libc::AT_FDCWD, name.as_ptr(), flags) });
            close_opened(unsafe { control_openat64(libc::AT_FDCWD, name.as_ptr(), flags) });
            stat_target(&name);
            let missing = cstring(&target.with_extension("missing"));
            assert_eq!(
                unsafe { control_open(missing.as_ptr(), libc::O_RDONLY) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOENT)
            );
            let created = cstring(&target.with_extension("created"));
            let old_mask = unsafe { libc::umask(0) };
            let fd = unsafe {
                control_open64(
                    created.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC,
                    0o640 as libc::mode_t,
                )
            };
            assert!(fd >= 0);
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
            assert_eq!(unsafe { libc::fstat(fd, metadata.as_mut_ptr()) }, 0);
            assert_eq!(unsafe { metadata.assume_init() }.st_mode & 0o777, 0o640);
            close_opened(fd);
            unsafe { libc::umask(old_mask) };
        }
        "unsafe-host" | "statx-before-unsafe-host" => {
            // MODEL ONLY: actual namespace and native executable stay ordinary.
            // An unsafe evidence row must reject qualification before any
            // requested target lookup; the mutation performs that lookup first.
            let host = exec_support::modeled_host(None);
            let mut evidence = host.evidence().clone();
            evidence.mounts =
                MountEvidence::parse_complete(b"900 899 0:9 / / rw - fuse model rw\n", Vec::new())
                    .unwrap();
            if mode == "statx-before-unsafe-host" {
                stat_target(&name);
            }
            let refusal = unsafe {
                HostQualification::from_frozen_evidence(evidence, host.attestation().clone())
            }
            .unwrap_err();
            assert!(
                matches!(refusal, HostPolicyRefusal::UnsafeLookupMount { filesystem, .. }
                if filesystem == "fuse")
            );
        }
        mode => panic!("unknown instrumentation child: {mode}"),
    }
    assert_eq!(descriptor_count(), before, "tracer retained a descriptor");
    fs::write(std::env::var_os(TRACE_RESULT).unwrap(), b"PASS\n").unwrap();
}

#[test]
fn lb2_main_and_both_interpreter_kinds_classify_before_read_open() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory();
    let fifo = directory.join("fifo");
    let name = cstring(&fifo);
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o755) }, 0);
    let main_events = preparation_trace(&directory, &fifo, native_eacces(true));
    assert!(no_nonregular_read_open(&main_events, &fifo));
    assert!(
        no_target_traversal(&main_events, &fifo),
        "original CHECK rejects before a userspace pin"
    );

    let interpreter_main = with_interpreter(&directory, "elf", &fifo);
    let interpreter_events = preparation_trace(&directory, &interpreter_main, native_eacces(false));
    assert!(no_nonregular_read_open(&interpreter_events, &fifo));
    assert!(
        interpreter_events
            .as_ref()
            .unwrap()
            .iter()
            .any(|event| event.path == fifo.as_os_str().as_bytes()
                && event.is_open()
                && event.flags & libc::O_PATH != 0
                && event.result >= 0)
    );

    let script = directory.join("script");
    let contents = format!("#!{}\n", fifo.display());
    assert!(contents.len() <= 256, "fixture shebang is not truncated");
    executable(&script, contents.as_bytes());
    let script_events = preparation_trace(&directory, &script, native_eacces(false));
    assert!(no_nonregular_read_open(&script_events, &fifo));
    assert!(
        script_events
            .as_ref()
            .unwrap()
            .iter()
            .any(|event| event.path == fifo.as_os_str().as_bytes()
                && event.is_open()
                && event.flags & libc::O_PATH != 0
                && event.result >= 0)
    );

    for mode in ["fifo-read-open", "pinned-fifo-read-open"] {
        let mutation = mutation_child(&directory, mode, &fifo);
        assert!(
            !no_nonregular_read_open(&mutation, &fifo),
            "{mode} mutation must be rejected"
        );
    }
    fs::write(
        directory.join("FIFO-mutations.result"),
        "REJECTED: direct and pinned-FD FIFO readable-open mutations\n",
    )
    .unwrap();
}

#[test]
fn lb2_device_main_and_interpreter_paths_never_device_open() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory();
    let device = Path::new("/dev/null");
    let main = preparation_trace(&directory, device, native_eacces(true));
    assert!(no_target_traversal(&main, device));
    let interpreter_main = with_interpreter(&directory, "device-elf", device);
    let interpreter = preparation_trace(
        &directory,
        &interpreter_main,
        PrepareObservation::Refusal("LookupMountUnverified".into()),
    );
    assert!(no_target_traversal(&interpreter, device));
    let script = directory.join("device-script");
    executable(&script, b"#!/dev/null\n");
    let script = preparation_trace(
        &directory,
        &script,
        PrepareObservation::Refusal("LookupMountUnverified".into()),
    );
    assert!(no_target_traversal(&script, device));
    let mutation = mutation_child(&directory, "device-read-open", device);
    assert!(!no_target_traversal(&mutation, device));
    assert!(!no_nonregular_read_open(&mutation, device));
    fs::write(
        directory.join("device-mutation.result"),
        "REJECTED: ordinary readable /dev/null open mutation\n",
    )
    .unwrap();
}

#[test]
fn lb2_all_tracer_symbols_modes_errno_and_descriptor_neutrality() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory();
    let fifo = directory.join("controls");
    assert_eq!(unsafe { libc::mkfifo(cstring(&fifo).as_ptr(), 0o755) }, 0);
    let controls = mutation_child(&directory, "hook-controls", &fifo);
    let controls = controls.as_ref().unwrap();
    for operation in ["open", "open64", "openat", "openat64", "statx"] {
        assert!(
            controls.iter().any(|event| event.operation == operation
                && event.dirfd == libc::AT_FDCWD
                && event.path == fifo.as_os_str().as_bytes()
                && event.result >= 0
                && event.errno == 0),
            "live {operation} witness missing"
        );
    }
    assert!(
        controls
            .iter()
            .any(|event| event.is_open() && event.result == -1 && event.errno == libc::ENOENT)
    );
    let provenance = fs::read_to_string(env!("ELF_LOADER_PROVENANCE")).unwrap();
    assert!(
        provenance.contains("source sha256:") && provenance.contains("tests/fixtures/open_trace.c")
    );
    assert!(
        provenance.contains("open-trace.so")
            && provenance.contains("-shared")
            && provenance.contains("sha256:")
            && provenance
                .lines()
                .next()
                .is_some_and(|line| !line.is_empty())
    );
}

#[test]
fn lb2_incomplete_trace_saturation_and_footer_mutations_are_rejected() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory();
    let fifo = directory.join("completeness");
    assert_eq!(unsafe { libc::mkfifo(cstring(&fifo).as_ptr(), 0o755) }, 0);
    let control = mutation_child(&directory, "trace-complete-control", &fifo);
    assert!(no_nonregular_read_open(&control, &fifo));
    let control_bytes = fs::read(directory.join("trace-complete-control.opens")).unwrap();
    let footer_start = control_bytes[..control_bytes.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .unwrap()
        + 1;
    let prefix = &control_bytes[..footer_start];

    let missing = directory.join("missing-footer.opens");
    fs::write(&missing, prefix).unwrap();
    let missing = events(&missing);
    assert!(matches!(missing, Err(TraceError::MissingFooter)));
    assert!(!no_nonregular_read_open(&missing, &fifo));

    let count = control.as_ref().unwrap().len();
    let mismatched = directory.join("mismatched-footer.opens");
    let mut mismatched_bytes = prefix.to_vec();
    mismatched_bytes
        .extend_from_slice(format!("footer\t{count}\t{}\t0\t1\n", count + 1).as_bytes());
    fs::write(&mismatched, mismatched_bytes).unwrap();
    let mismatched = events(&mismatched);
    assert!(matches!(mismatched, Err(TraceError::CountMismatch { .. })));
    assert!(!no_nonregular_read_open(&mismatched, &fifo));

    let lost = directory.join("lost-event-footer.opens");
    let mut lost_bytes = prefix.to_vec();
    lost_bytes.extend_from_slice(format!("footer\t{}\t{count}\t1\t0\n", count + 1).as_bytes());
    fs::write(&lost, lost_bytes).unwrap();
    let lost = events(&lost);
    assert!(matches!(
        lost,
        Err(TraceError::LostEvents { failures: 1, .. })
    ));
    assert!(!no_nonregular_read_open(&lost, &fifo));

    let saturated = mutation_child(&directory, "trace-saturation", &fifo);
    assert!(
        matches!(saturated, Err(TraceError::Saturated { attempted, logged: 4096, failures })
        if attempted > 4096 && failures > 0)
    );
    assert!(
        !no_nonregular_read_open(&saturated, &fifo),
        "a dropped post-cap readable open cannot prove safe preparation"
    );
    fs::write(directory.join("incomplete-evidence-mutations.result"),
        format!("REJECTED: missing footer, count mismatch, loss and real saturation\nsaturation={saturated:?}\n"))
        .unwrap();
}

#[test]
fn lb5_modeled_unsafe_host_precedes_target_statx_mutation() {
    let _serial = SERIAL.lock().unwrap();
    let directory = directory();
    let target = directory.join("target");
    executable(&target, &fs::read("/bin/true").unwrap());
    assert_eq!(
        exec_support::run_native(&request(&target), &ChildSetup::default(), &directory),
        NativeObservation::Exited(0)
    );
    let qualified = mutation_child(&directory, "unsafe-host", &target);
    assert!(no_target_traversal(&qualified, &target));
    let mutated = mutation_child(&directory, "statx-before-unsafe-host", &target);
    assert!(
        !no_target_traversal(&mutated, &target),
        "statx-before-qualification mutation must fail"
    );
    assert!(
        mutated
            .as_ref()
            .unwrap()
            .iter()
            .any(|event| event.operation == "statx"
                && event.path == target.as_os_str().as_bytes()
                && event.result == 0)
    );
    fs::write(
        directory.join("unsafe-host-mutation.result"),
        "MODEL ONLY: UnsafeLookupMount before target traversal; statx-first mutation REJECTED\n",
    )
    .unwrap();
}
