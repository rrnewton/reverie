/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The pass-through recorder (`reverie-examples/recorder.rs`) gives a program
//! the syscall results native Linux gives it, on ptrace, for programs whose
//! record does not depend on the environment. The full corpus, with its
//! expected-mismatch ledger, runs in the environment the ledger was measured
//! in (`reverie-examples/e2e/`).

use std::path::Path;
use std::process::Command;

use reverie::GlobalTool;

/// Every run gets the sanitizer settings reverie-ptrace adds to its guest's
/// environment (tracer.rs), so that ptrace's guest has the native one: the
/// number of variables decides when `sh -c` grows its heap.
fn record(backend: &str, program: &[&str], output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recorder"))
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .env("LSAN_OPTIONS", "detect_leaks=0")
        .args(["--backend", backend, "--output"])
        .arg(output)
        .args(program)
        .output()
        .expect("run the recorder")
}

#[test]
fn ptrace_records_what_native_linux_returns() {
    let directory = tempfile::tempdir().unwrap();
    for program in [
        &["/bin/true"][..],
        &["/bin/false"],
        &["/bin/echo", "hello"],
        &["/bin/sh", "-c", "echo a; exit 3"],
    ] {
        let native = directory.path().join("native");
        let ptrace = directory.path().join("ptrace");
        for (backend, path) in [("native", &native), ("ptrace", &ptrace)] {
            let output = record(backend, program, path);
            assert!(
                output.status.success(),
                "{backend} {program:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let native = std::fs::read_to_string(&native).unwrap();
        let ptrace = std::fs::read_to_string(&ptrace).unwrap();
        assert!(
            native.starts_with("exit ") && native.contains("\nthread 0\nexecve=ok\n"),
            "{program:?}: {native}"
        );
        assert_eq!(ptrace, native, "{program:?}");
    }
}

/// The in-guest backends host the recorder in the example preload; a program
/// whose calls all come after the loader records as natively.
#[test]
fn in_guest_backends_host_the_recorder_in_the_example_preload() {
    let directory = tempfile::tempdir().unwrap();
    for program in [&["/bin/true"][..], &["/bin/false"]] {
        let native = directory.path().join("native");
        let output = record("native", program, &native);
        assert!(output.status.success(), "native {program:?}");
        let native = std::fs::read_to_string(&native).unwrap();
        for backend in ["liteinst", "in-guest-trap"] {
            let path = directory.path().join(backend);
            let output = record(backend, program, &path);
            assert!(
                output.status.success(),
                "{backend} {program:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let got = std::fs::read_to_string(&path).unwrap();
            // The loader's calls come before the preload's constructor, so
            // the backend's record is the end of the native one.
            let (exit, calls) = got.split_once("\nthread 0\n").unwrap();
            assert_eq!(
                exit,
                native.lines().next().unwrap(),
                "{backend} {program:?}"
            );
            assert!(
                native.ends_with(calls) && !calls.is_empty(),
                "{backend} {program:?}:\n{got}\nnative:\n{native}"
            );
        }
    }
}

#[test]
fn sabre_without_its_loader_refuses_by_name() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("record");
    let output = Command::new(env!("CARGO_BIN_EXE_recorder"))
        .env("SABRE_BINARY", directory.path().join("no-such-loader"))
        .args(["--backend", "sabre", "--output"])
        .arg(&output_path)
        .arg("/bin/true")
        .output()
        .expect("run the recorder");
    assert!(!output.status.success(), "sabre ran");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("REFUSED: the SaBRe loader was not found"),
        "sabre: {stderr}"
    );
    assert!(!output_path.exists(), "sabre wrote a record");
}

/// SaBRe hosts the recorder in experimental/reverie-sabre-strace's plugin, its
/// GlobalState served from the recorder: the tool sees the program's calls
/// from when SaBRe's loader has loaded it, and the exit code. (Which calls,
/// and their results, are the cross-backend check's ledger rows,
/// reverie-examples/e2e/.) It needs reverie-sabre-strace built beside the
/// recorder (`cargo build -p reverie-sabre-strace`), for the plugin and for
/// the loader it names (`--print-sabre`).
#[test]
#[ignore = "requires `cargo build -p reverie-sabre-strace`; run with --ignored"]
fn sabre_hosts_the_recorder_in_its_example_tool_plugin() {
    let binaries = Path::new(env!("CARGO_BIN_EXE_recorder")).parent().unwrap();
    let loader = Command::new(binaries.join("reverie-sabre-strace"))
        .arg("--print-sabre")
        .output()
        .expect("run reverie-sabre-strace --print-sabre");
    assert!(loader.status.success(), "{loader:?}");
    let loader = String::from_utf8(loader.stdout).unwrap();
    let directory = tempfile::tempdir().unwrap();
    for program in [&["/bin/true"][..], &["/bin/false"]] {
        let native = directory.path().join("native");
        let output = record("native", program, &native);
        assert!(output.status.success(), "native {program:?}");
        let native = std::fs::read_to_string(&native).unwrap();
        let path = directory.path().join("sabre");
        let output = Command::new(env!("CARGO_BIN_EXE_recorder"))
            .env("SABRE_BINARY", loader.trim_end())
            .args(["--backend", "sabre", "--output"])
            .arg(&path)
            .args(program)
            .output()
            .expect("run the recorder");
        assert!(
            output.status.success(),
            "sabre {program:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let got = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            got.lines().next(),
            native.lines().next(),
            "sabre {program:?}"
        );
        assert!(
            got.ends_with("\nexit_group=?\n") && got.lines().count() > 3,
            "sabre {program:?}:\n{got}"
        );
    }
}

/// DBT hosts the recorder in reverie-dbt's prototype runtime, its GlobalState
/// served from the recorder over DBT's coordinator socket: the tool sees every
/// call native Linux makes after DynamoRIO takes over, in order, and the exit
/// code. (Some results differ, a DynamoRIO descriptor and rseq among them;
/// those are the cross-backend check's ledger rows, reverie-examples/e2e/.) It
/// needs the DBT client, built by reverie-dbt/scripts/build-client.sh (found
/// beside the recorder, or through REVERIE_DBT_CLIENT).
#[test]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
fn dbt_hosts_the_recorder_in_its_prototype_runtime() {
    fn names(record: &str) -> Vec<&str> {
        record
            .lines()
            .filter(|line| !line.starts_with("exit ") && !line.starts_with("thread "))
            .map(|line| line.split_once('=').map_or(line, |(name, _)| name))
            .collect()
    }
    let directory = tempfile::tempdir().unwrap();
    for program in [&["/bin/true"][..], &["/bin/false"]] {
        let native = directory.path().join("native");
        let output = record("native", program, &native);
        assert!(output.status.success(), "native {program:?}");
        let native = std::fs::read_to_string(&native).unwrap();
        let path = directory.path().join("dbt");
        let output = record("dbt", program, &path);
        assert!(
            output.status.success(),
            "dbt {program:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let got = std::fs::read_to_string(&path).unwrap();
        assert_eq!(got.lines().next(), native.lines().next(), "dbt {program:?}");
        // The execve that starts the program runs before DynamoRIO takes over.
        let mut native_names = names(&native);
        assert_eq!(native_names.remove(0), "execve", "{native}");
        assert_eq!(names(&got), native_names, "dbt {program:?}:\n{got}");
    }
}

// These use the real recorder/GlobalState route, not scripted RPC responses.
// A static raw-entry fixture makes the first application syscall observable.
fn compile_descriptor_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recorder_descriptor_lifecycle.c");
    let guest = directory.path().join("recorder-descriptor-lifecycle");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args([
            "-std=c11",
            "-O2",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-ffreestanding",
            "-fno-builtin",
            "-fno-stack-protector",
            "-fno-pie",
            "-no-pie",
            "-nostdlib",
            "-static",
            "-Wl,-e,_start",
            "-Wl,--build-id=none",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&guest)
        .output()
        .expect("compile raw-entry descriptor fixture");
    assert!(
        output.status.success(),
        "{}: {}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!(
        "descriptor fixture: compiler={:?} source={} guest={} image={:02x?}",
        std::env::var_os("CC").unwrap_or_else(|| "cc".into()),
        source.display(),
        guest.display(),
        std::fs::read(&guest).expect("retain compiled descriptor image")
    );
    (directory, guest)
}

#[derive(Debug)]
struct DescriptorRecord {
    words: Vec<i64>,
    threads: Vec<Vec<String>>,
    text: String,
}

fn descriptor_frontend(backend: &str, output_path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recorder"));
    command
        .args(["--backend", backend, "--output"])
        .arg(output_path);
    // The backend adapter selects recorder itself. Ambient test tools must
    // not replace it, including the named selector that precedes older flags.
    for selector in [
        "REVERIE_DBT_TOOL",
        "HERMIT_DBT_TEST_REWRITE_EXIT",
        "HERMIT_DBT_TEST_INJECT_INVALID_CLONE",
        "HERMIT_DBT_TEST_SET_REG",
        "HERMIT_DBT_TEST_COMPARE_EXCHANGE",
        "HERMIT_DBT_TEST_PPID",
        "HERMIT_DBT_TEST_BACKTRACE",
        "HERMIT_DBT_STRACE",
        "HERMIT_DBT_SYSCALL_HISTOGRAM",
        "HERMIT_DBT_COUNTER1",
        "HERMIT_DBT_COUNTER2",
        "HERMIT_DBT_CHUNKY_PRINT",
        "HERMIT_DBT_CHROME_TRACE",
        "HERMIT_DBT_CHAOS",
        "HERMIT_DBT_NOOP",
        "HERMIT_DBT_COUNTER1_EXACT",
        "HERMIT_DBT_COUNTER2_EXACT",
    ] {
        command.env_remove(selector);
    }
    command
}

fn descriptor_command(backend: &str, guest: &Path, mode: &str, output_path: &Path) -> Command {
    let mut command = descriptor_frontend(backend, output_path);
    command.arg(guest).arg(mode);
    command
}

fn record_descriptors(
    backend: &str,
    guest: &Path,
    mode: &str,
    output_path: &Path,
) -> DescriptorRecord {
    let output = descriptor_command(backend, guest, mode, output_path)
        .output()
        .expect("run raw-entry recorder fixture");
    parse_descriptor_output(backend, mode, output_path, output)
}

fn retain_recorder_output(label: &str, output: &std::process::Output, path: &Path) {
    eprintln!(
        "recorder bytes {label}: status={:?} stdout={:02x?} stderr={:02x?} record={:02x?}",
        output.status.code(),
        output.stdout,
        output.stderr,
        std::fs::read(path)
    );
}

fn parse_descriptor_output(
    backend: &str,
    mode: &str,
    output_path: &Path,
    output: std::process::Output,
) -> DescriptorRecord {
    retain_recorder_output(&format!("{backend}/{mode}"), &output, output_path);
    let text = std::fs::read_to_string(output_path);
    // Keep successful controls and failed guest output in --nocapture receipts
    // before the fixture/record TempDir is dropped.
    eprintln!(
        "descriptor {backend}/{mode}: status={} stdout={:02x?} stderr={}\nrecord:\n{}",
        output.status,
        output.stdout,
        String::from_utf8_lossy(&output.stderr),
        text.as_deref().unwrap_or("<no UTF-8 recorder output>")
    );
    assert!(output.status.success(), "{backend} {mode}: {output:?}");
    let text = text.expect("read complete recorder output");
    let mut lines = text.lines();
    // The recorder's own exit code alone does not establish the guest's exit.
    assert_eq!(lines.next(), Some("exit 0"), "{backend} {mode}:\n{text}");
    let mut threads: Vec<Vec<String>> = Vec::new();
    for line in lines {
        if let Some(number) = line.strip_prefix("thread ") {
            assert_eq!(number.parse::<usize>().unwrap(), threads.len(), "{text}");
            threads.push(Vec::new());
        } else {
            assert!(line.contains('='), "{text}");
            threads
                .last_mut()
                .expect("event after thread header")
                .push(line.into());
        }
    }
    assert!(!threads.is_empty(), "{text}");
    if backend == "native" {
        // This launch exec precedes _start; remove nothing from DBT's record.
        assert_eq!(threads[0].remove(0), "execve=ok", "{text}");
    }
    assert_eq!(output.stdout.len() % 8, 0, "{backend} {mode}: {output:?}");
    let words = output
        .stdout
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| i64::from_le_bytes(*bytes))
        .collect();
    DescriptorRecord {
        words,
        threads,
        text,
    }
}

fn check_first_descriptor(mode: &str) {
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, mode, &directory.path().join("native"));
    let dbt = record_descriptors("dbt", &guest, mode, &directory.path().join("dbt"));
    assert_descriptor_records(mode, native, dbt);
}

fn assert_descriptor_records(mode: &str, native: DescriptorRecord, dbt: DescriptorRecord) {
    let mut expected = vec!["openat", "openat", "write", "close", "close", "exit_group"];
    if mode == "warm" {
        expected.insert(0, "getpid");
    }
    for record in [&native, &dbt] {
        assert_eq!(record.threads.len(), 1, "{record:?}");
        let names: Vec<_> = record.threads[0]
            .iter()
            .map(|event| event.split_once('=').unwrap().0)
            .collect();
        // Check the first-call premise in the real record, not just the source.
        assert_eq!(names, expected, "{record:?}");
    }
    assert_eq!(
        native.words,
        [3, 4],
        "native lowest-free controls: {native:?}"
    );
    assert_eq!(
        dbt.words, native.words,
        "first two available descriptors held together: {dbt:?}"
    );
    // Retain all results too: the recorder must report the actual guest fd.
    assert_eq!(dbt.threads, native.threads, "{dbt:?}\n{native:?}");
}

#[test]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
fn dbt_recorder_cold_open_keeps_the_first_linux_descriptor() {
    check_first_descriptor("cold");
}

#[test]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
fn dbt_recorder_prior_rpc_keeps_the_first_linux_descriptor() {
    check_first_descriptor("warm");
}

#[test]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
fn dbt_descriptor_reservation_failure_is_a_runtime_refusal() {
    use std::os::unix::process::CommandExt;

    let (directory, guest) = compile_descriptor_fixture();
    for backend in ["native", "dbt"] {
        let path = directory.path().join(backend);
        let mut command = descriptor_command(backend, &guest, "cold", &path);
        // Native guest allocation still has room, but the runtime cannot
        // reserve a descriptor at or above 1024. Apply the same real kernel
        // limit before either recorder starts; no guest warmup or error hook.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 1024,
                    rlim_max: 1024,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().expect("run limited descriptor fixture");
        let record = std::fs::read_to_string(&path);
        eprintln!(
            "descriptor reservation {backend}: status={} stdout={:02x?} stderr={}\nrecord:\n{}",
            output.status,
            output.stdout,
            String::from_utf8_lossy(&output.stderr),
            record.as_deref().unwrap_or("<no UTF-8 recorder output>")
        );
        assert!(output.status.success(), "{backend}: {output:?}");
        let record = record.expect("read the recorder's guest outcome");
        if backend == "native" {
            let expected: Vec<_> = [3_i64, 4].into_iter().flat_map(i64::to_le_bytes).collect();
            assert_eq!(output.stdout, expected, "{record}");
            assert!(record.starts_with("exit 0\n"), "{record}");
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("reverie-dbt: application RPC preparation refused:"),
                "{output:?}"
            );
            assert!(record.starts_with("exit 101\n"), "{record}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(!record.contains("openat="), "{record}");
        }
    }
}

// Keep CounterLocal's real RPC-before-tail-inject path. The coordinator below
// observes real requests; it does not implement another guest Tool or protocol.
#[path = "../counter1_tool.rs"]
mod descriptor_counter1;

type ExactDescriptorCounter = <descriptor_counter1::CounterLocal as reverie::Tool>::GlobalState;

#[derive(Debug, Default)]
struct DescriptorCounterObserver {
    counter: ExactDescriptorCounter,
    events: std::sync::Mutex<Vec<(i32, reverie::syscalls::Sysno)>>,
}

#[reverie::global_tool]
impl GlobalTool for DescriptorCounterObserver {
    type Request = <ExactDescriptorCounter as reverie::GlobalTool>::Request;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, from: reverie::Pid, request: descriptor_counter1::IncrMsg) {
        self.events.lock().unwrap().push((from.as_raw(), request.0));
        reverie::GlobalTool::receive_rpc(&self.counter, from, request).await;
    }
}

fn assert_descriptor_lifetime_words(words: &[i64], label: &str) {
    assert_eq!(words.len(), 11, "{label}: {words:?}");
    assert!(
        words[0] > 0 && words[1] > 0 && words[0] != words[1],
        "{label}: {words:?}"
    );
    assert_eq!(
        words[2], words[1],
        "child PID equals waited PID: {label}: {words:?}"
    );
    assert_eq!(
        words[3], words[0],
        "child's actual parent: {label}: {words:?}"
    );
    assert_eq!(
        words[4], words[0],
        "parent's subsequent syscall: {label}: {words:?}"
    );
    assert_eq!(
        &words[5..9],
        &[37 << 8, 1, b'A' as i64, b'P' as i64],
        "{label}: {words:?}"
    );
    assert!(
        words[9] >= 3 && words[10] >= 3 && words[9] != words[10],
        "{label}: {words:?}"
    );
}

fn descriptor_parent_requests(mode: &str) -> Vec<reverie::syscalls::Sysno> {
    use reverie::syscalls::Sysno;
    vec![
        Sysno::getpid,
        Sysno::pipe2,
        Sysno::write,
        if mode == "fork" {
            Sysno::fork
        } else {
            Sysno::vfork
        },
        Sysno::wait4,
        Sysno::getpid,
        Sysno::fcntl,
        Sysno::read,
        Sysno::read,
        Sysno::write,
        Sysno::read,
        Sysno::close,
        Sysno::read,
        Sysno::close,
        Sysno::write,
        Sysno::exit_group,
    ]
}

fn descriptor_child_requests() -> Vec<reverie::syscalls::Sysno> {
    use reverie::syscalls::Sysno;
    vec![
        Sysno::getpid,
        Sysno::getppid,
        Sysno::close,
        Sysno::dup3,
        Sysno::write,
        Sysno::close,
        Sysno::close,
        Sysno::exit,
    ]
}

async fn check_counter1_descriptor_lifetime(mode: &str) {
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, mode, &directory.path().join("native"));
    assert_descriptor_lifetime_words(&native.words, "native");
    let expected_native_parent = [
        "getpid=ok",
        "pipe2=ok",
        "write=1",
        if mode == "fork" {
            "fork=ok"
        } else {
            "vfork=ok"
        },
        "wait4=ok",
        "getpid=ok",
        "fcntl=1",
        "read=1",
        "read=16",
        "write=1",
        "read=1",
        "close=ok",
        "read=0",
        "close=ok",
        "write=88",
        "exit_group=?",
    ];
    assert_eq!(native.threads.len(), 2, "{}", native.text);
    assert_eq!(native.threads[0], expected_native_parent, "{}", native.text);
    assert_eq!(
        native.threads[1],
        [
            "getpid=ok".to_owned(),
            "getppid=ok".to_owned(),
            "close=ok".to_owned(),
            format!("dup3={}", native.words[9]),
            "write=16".to_owned(),
            "close=ok".to_owned(),
            "close=ok".to_owned(),
            "exit=?".to_owned(),
        ],
        "{}",
        native.text
    );

    let runner = reverie_dbt::DbtRunner::from_env()
        .expect("DynamoRIO and the prototype native client must be configured");
    let mut command = Command::new(&guest);
    command.arg(mode);
    for selector in [
        "REVERIE_DBT_TOOL",
        "HERMIT_DBT_TEST_REWRITE_EXIT",
        "HERMIT_DBT_TEST_INJECT_INVALID_CLONE",
        "HERMIT_DBT_TEST_SET_REG",
        "HERMIT_DBT_TEST_COMPARE_EXCHANGE",
        "HERMIT_DBT_TEST_PPID",
        "HERMIT_DBT_TEST_BACKTRACE",
        "HERMIT_DBT_STRACE",
        "HERMIT_DBT_SYSCALL_HISTOGRAM",
        "HERMIT_DBT_COUNTER1",
        "HERMIT_DBT_COUNTER2",
        "HERMIT_DBT_CHUNKY_PRINT",
        "HERMIT_DBT_CHROME_TRACE",
        "HERMIT_DBT_CHAOS",
        "HERMIT_DBT_NOOP",
        "HERMIT_DBT_COUNTER1_EXACT",
        "HERMIT_DBT_COUNTER2_EXACT",
    ] {
        command.env_remove(selector);
    }
    command.env(reverie_dbt::TOOL_ENV, "counter1");
    let (output, observer) = runner
        .output_with_global::<DescriptorCounterObserver>(&command, ())
        .await
        .expect("run real CounterLocal with the observing coordinator");
    let events = observer.events.into_inner().unwrap();
    eprintln!(
        "descriptor dbt/counter1/{mode}: status={} stdout={:02x?} stderr={}\nrequests={events:?}",
        output.status,
        output.stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "dbt/counter1/{mode}: {output:?}");
    assert_eq!(output.stdout.len(), 88, "dbt/counter1/{mode}: {output:?}");
    let words: Vec<_> = output
        .stdout
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| i64::from_le_bytes(*bytes))
        .collect();
    assert_descriptor_lifetime_words(&words, &format!("dbt/counter1/{mode}"));
    assert_eq!(
        observer.counter.total() as usize,
        events.len(),
        "{events:?}"
    );

    let mut by_sender = std::collections::BTreeMap::<i32, Vec<_>>::new();
    for &(sender, sysno) in &events {
        by_sender.entry(sender).or_default().push(sysno);
    }
    let parent = i32::try_from(words[0]).unwrap();
    let child = i32::try_from(words[1]).unwrap();
    assert_eq!(
        by_sender.remove(&parent),
        Some(descriptor_parent_requests(mode)),
        "{events:?}"
    );
    if mode == "fork" {
        // This proves requests from the actual waited child, not just a larger
        // global total. Its first RPC exercises the copied-PID reconnect.
        assert_eq!(
            by_sender.remove(&child),
            Some(descriptor_child_requests()),
            "{events:?}"
        );
    } else {
        // Copied vfork operations do not enter CounterLocal. The complete
        // Linux witness and later parent requests prove usable parent state,
        // not a child reconnect or identity of the cached socket.
        assert!(!by_sender.contains_key(&child), "{events:?}");
    }
    assert!(by_sender.is_empty(), "unexpected RPC sender: {events:?}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
async fn dbt_counter1_fork_reconnect_preserves_parent_descriptors() {
    check_counter1_descriptor_lifetime("fork").await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires the DBT client built by reverie-dbt/scripts/build-client.sh; run with --ignored"]
async fn dbt_counter1_copied_vfork_preserves_parent_descriptors() {
    check_counter1_descriptor_lifetime("vfork").await;
}

fn check_ptrace_first_descriptor(mode: &str) {
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, mode, &directory.path().join("native"));
    let mut ptrace = record_descriptors("ptrace", &guest, mode, &directory.path().join("ptrace"));
    // Require the complete native report before making a post-launch view.
    assert_eq!(ptrace.text, native.text, "{ptrace:?}\n{native:?}");
    assert_eq!(ptrace.threads[0].remove(0), "execve=ok", "{ptrace:?}");
    assert_descriptor_records(mode, native, ptrace);
}

#[test]
fn ptrace_recorder_cold_open_keeps_the_first_linux_descriptor() {
    check_ptrace_first_descriptor("cold");
}

#[test]
fn ptrace_recorder_prior_rpc_keeps_the_first_linux_descriptor() {
    check_ptrace_first_descriptor("warm");
}

fn recorder_caller_artifact() -> std::path::PathBuf {
    Path::new(env!("CARGO_BIN_EXE_recorder"))
        .parent()
        .unwrap()
        .join("reverie-dbt-callers/recorder/manifest.json")
}

#[test]
fn dbt_recorder_caller_cold_open_keeps_the_first_linux_descriptor() {
    let artifact = recorder_caller_artifact();
    let _validated = reverie_dbt::CallerArtifact::load(&artifact)
        .expect("build the current Recorder caller with build-client.sh --caller-manifest");
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, "cold", &directory.path().join("native"));
    let path = directory.path().join("caller");
    let output = descriptor_frontend("dbt", &path)
        .arg("--dbt-artifact")
        .arg(&artifact)
        .arg(&guest)
        .arg("cold")
        .output()
        .expect("run Recorder with its explicit caller artifact");
    let dbt = parse_descriptor_output("dbt", "cold", &path, output);
    assert_descriptor_records("cold", native, dbt);
}

#[test]
fn recorder_default_dbt_lifecycle_consumers_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    for (label, program, expected) in [
        (
            "sh-fork",
            &["/bin/sh", "-c", "/bin/true; /bin/echo b"][..],
            &b"b\n"[..],
        ),
        (
            "sh-pipe",
            &["/bin/sh", "-c", "/bin/seq 1 100 | /bin/wc -l"],
            &b"100\n"[..],
        ),
        (
            "env-python",
            &["/bin/env", "PYTHONHASHSEED=0", "/bin/python3", "-c", "pass"],
            &b""[..],
        ),
    ] {
        let path = directory.path().join(label);
        let output = record("dbt", program, &path);
        retain_recorder_output(label, &output, &path);
        assert!(output.status.success(), "{label}: {output:?}");
        let report = std::fs::read_to_string(&path).expect("read complete legacy Recorder report");
        assert_eq!(report.lines().next(), Some("exit 0"), "{label}: {report}");
        assert_eq!(output.stdout, expected, "{label}: {output:?}");
    }
}

#[test]
fn dbt_recorder_caller_prior_rpc_keeps_the_first_linux_descriptor() {
    let artifact = recorder_caller_artifact();
    let _validated = reverie_dbt::CallerArtifact::load(&artifact)
        .expect("build the current Recorder caller with build-client.sh --caller-manifest");
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, "warm", &directory.path().join("native"));
    let path = directory.path().join("caller");
    let output = descriptor_frontend("dbt", &path)
        .arg("--dbt-artifact")
        .arg(&artifact)
        .arg(&guest)
        .arg("warm")
        .output()
        .expect("run Recorder with its explicit caller artifact");
    let dbt = parse_descriptor_output("dbt", "warm", &path, output);
    assert_descriptor_records("warm", native, dbt);
}

#[test]
fn dbt_recorder_caller_noop_fails_the_descriptor_oracle() {
    let noop = Path::new(env!("CARGO_BIN_EXE_recorder"))
        .parent()
        .unwrap()
        .join("reverie-dbt-callers/basics-noop/manifest.json");
    reverie_dbt::CallerArtifact::load(&noop).expect("build the real basics-Noop caller");
    let (directory, guest) = compile_descriptor_fixture();
    let native = record_descriptors("native", &guest, "cold", &directory.path().join("native"));
    let path = directory.path().join("noop");
    let output = descriptor_frontend("dbt", &path)
        .arg("--dbt-artifact")
        .arg(&noop)
        .arg(&guest)
        .arg("cold")
        .output()
        .expect("run the descriptor guest with the actual Noop caller");
    retain_recorder_output("noop/cold", &output, &path);
    assert!(output.status.success(), "{output:?}");
    let expected: Vec<_> = [3_i64, 4].into_iter().flat_map(i64::to_le_bytes).collect();
    assert_eq!(output.stdout, expected, "{output:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "exit 0\n");
    // The real guest succeeded with the right descriptors. The unchanged
    // positive oracle must still reject its missing recorded thread/events.
    assert!(
        std::panic::catch_unwind(|| {
            let dbt = parse_descriptor_output("dbt", "cold", &path, output);
            assert_descriptor_records("cold", native, dbt);
        })
        .is_err()
    );
}

#[test]
fn dbt_recorder_caller_missing_or_malformed_artifact_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let malformed = directory.path().join("malformed.json");
    std::fs::write(&malformed, b"{}").unwrap();
    for artifact in [directory.path().join("missing.json"), malformed] {
        for existing_report in [false, true] {
            let path = directory.path().join("report");
            if existing_report {
                std::fs::write(&path, b"existing report\n").unwrap();
            }
            let output = descriptor_frontend("dbt", &path)
                .arg("--dbt-artifact")
                .arg(&artifact)
                .args(["/bin/echo", "must-not-run"])
                .output()
                .expect("run Recorder with an invalid supplied artifact");
            retain_recorder_output("invalid-artifact", &output, &path);
            assert!(!output.status.success(), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(artifact.to_str().unwrap()), "{stderr}");
            assert!(
                stderr.contains("cannot load Recorder caller artifact"),
                "{stderr}"
            );
            assert!(
                stderr.contains("build-client.sh --caller-manifest"),
                "{stderr}"
            );
            if existing_report {
                assert_eq!(std::fs::read(&path).unwrap(), b"existing report\n");
                std::fs::remove_file(&path).unwrap();
            } else {
                assert!(!path.exists(), "refused run created a report");
            }
        }
    }
}

#[test]
fn recorder_caller_artifact_requires_dbt_backend() {
    let artifact = recorder_caller_artifact();
    reverie_dbt::CallerArtifact::load(&artifact).expect("build the current Recorder caller");
    let directory = tempfile::tempdir().unwrap();
    for backend in [
        "native",
        "ptrace",
        "kvm",
        "liteinst",
        "in-guest-trap",
        "e9patch",
        "sabre",
    ] {
        let path = directory.path().join(backend);
        let output = descriptor_frontend(backend, &path)
            .arg("--dbt-artifact")
            .arg(&artifact)
            .args(["/bin/echo", "must-not-run"])
            .output()
            .expect("run Recorder with an incompatible backend option");
        retain_recorder_output(backend, &output, &path);
        assert!(!output.status.success(), "{backend}: {output:?}");
        assert!(output.stdout.is_empty(), "{backend}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--dbt-artifact requires --backend dbt"),
            "{backend}: {output:?}"
        );
        assert!(!path.exists(), "refused run created a report");
    }
}

#[test]
fn dbt_recorder_caller_preserves_nonzero_guest_exit() {
    let artifact = recorder_caller_artifact();
    reverie_dbt::CallerArtifact::load(&artifact).expect("build the current Recorder caller");
    let (directory, guest) = compile_descriptor_fixture();
    let path = directory.path().join("nonzero");
    let output = descriptor_frontend("dbt", &path)
        .arg("--dbt-artifact")
        .arg(&artifact)
        .arg(&guest)
        .arg("invalid")
        .output()
        .expect("run the existing fixture's nonzero mode");
    retain_recorder_output("caller/nonzero", &output, &path);
    // Recorder reports the completed guest's status without adopting it as
    // the frontend's own exit status.
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "exit 3\nthread 0\nexit_group=?\n"
    );
}

#[test]
fn recorder_caller_inherited_stdin_is_explicit() {
    use std::io::Write as _;

    let artifact = recorder_caller_artifact();
    reverie_dbt::CallerArtifact::load(&artifact).expect("build the current Recorder caller");
    let directory = tempfile::tempdir().unwrap();
    for (label, backend, use_caller, expected) in [
        ("ptrace", "ptrace", false, &b"hello\n"[..]),
        ("caller", "dbt", true, &b"hello\n"[..]),
        ("legacy", "dbt", false, &b""[..]),
    ] {
        let path = directory.path().join(label);
        // std::io::pipe creates close-on-exec descriptors. A new pipe for
        // each run supplies exactly six bytes and EOF before the spawn.
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"hello\n").unwrap();
        drop(writer);
        let mut command = descriptor_frontend(backend, &path);
        if use_caller {
            command.arg("--dbt-artifact").arg(&artifact);
        }
        let output = command
            .stdin(reader)
            .arg("/bin/cat")
            .output()
            .expect("run Recorder with fresh inherited input");
        retain_recorder_output(label, &output, &path);
        assert!(output.status.success(), "{label}: {output:?}");
        let report = std::fs::read_to_string(&path).unwrap();
        assert_eq!(report.lines().next(), Some("exit 0"), "{label}: {report}");
        assert_eq!(output.stdout, expected, "{label}: {output:?}");
        assert!(output.stderr.is_empty(), "{label}: {output:?}");
    }
}
