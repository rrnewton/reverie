use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;

const INSTRUCTION_CONTROL_UNAVAILABLE_STATUS: i32 = 77;
const TEST_STRADDLER_STALENESS_TICKS: &str = "20000";

#[test]
fn logged_output_drains_both_pipes_with_one_blocking_worker() {
    use std::os::unix::process::CommandExt;

    // Each invocation owns this process group. Keep cleanup active even if a
    // deadline or assertion fails, including the host's actual guest child.
    struct Host(Option<std::process::Child>);
    impl Drop for Host {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
            }
        }
    }

    let temporary = tempfile::tempdir().unwrap();
    let directory = std::env::var_os("LITEINST_LOG_LAUNCH_TEST_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&directory).unwrap();
    let mut host = Host(Some(
        Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"))
            .arg("diagnostic-log-host")
            .arg(&directory)
            .arg("one-blocking-worker")
            .process_group(0)
            .stdout(std::fs::File::create(directory.join("host.stdout")).unwrap())
            .stderr(std::fs::File::create(directory.join("host.stderr")).unwrap())
            .spawn()
            .unwrap(),
    ));
    let host_pid = host.0.as_ref().unwrap().id();
    std::fs::write(directory.join("host.pid"), host_pid.to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        // WNOWAIT keeps the exited host PID reserved until its whole group
        // is stopped. A panic or failed host must not leave its guest alive,
        // and cleanup must not target a PID that wait() has already released.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    host_pid,
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        if unsafe { info.si_pid() } != 0 {
            let mut child = host.0.take().unwrap();
            unsafe { libc::kill(-(host_pid as i32), libc::SIGKILL) };
            break child.wait().unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "logged host exceeded 20 seconds; retained evidence: {}",
            directory.display()
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "logged host status {status}; {}",
        directory.display()
    );
    assert_eq!(std::fs::read(directory.join("host.stderr")).unwrap(), b"");
    let capacities = std::fs::read_to_string(directory.join("pipe-capacities.txt")).unwrap();
    let capacities: Vec<usize> = capacities
        .trim_end()
        .split(' ')
        .map(|part| part.split_once('=').unwrap().1.parse().unwrap())
        .collect();
    assert_eq!(capacities.len(), 3);
    assert_eq!(capacities[2], 1024 * 1024);
    assert!(capacities[0] > 0 && capacities[0] < capacities[2]);
    assert!(capacities[1] > 0 && capacities[1] < capacities[2]);
    assert_eq!(
        std::fs::read(directory.join("stdout.bin")).unwrap(),
        [b"injected stdout\n".as_slice(), &vec![b'O'; capacities[2]]].concat()
    );
    assert_eq!(
        std::fs::read(directory.join("stderr.bin")).unwrap(),
        [
            b"tail-injected stderr\n".as_slice(),
            &vec![b'E'; capacities[2]]
        ]
        .concat()
    );
    assert_eq!(
        std::fs::read(directory.join("log.bin")).unwrap(),
        b"bootstrap ready\nTool syscall\nTool syscall\nTool cleanup\n"
    );
    assert_eq!(std::fs::read(directory.join("error.txt")).unwrap(), b"");
    assert_eq!(
        std::fs::read(directory.join("host.stdout")).unwrap(),
        b"diagnostic one-blocking-worker: rpc=2 stdout=1048592 stderr=1048597 log=55 missing-finish=false\n"
    );
    println!(
        "one blocking worker: capacities={capacities:?}; exact stdout/stderr/log and RPC preserved"
    );
}

#[test]
fn protected_diagnostic_log_preserves_native_state_rpc_and_output() {
    use std::os::unix::process::CommandExt;

    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    for (case, on_alt_stack) in [
        ("descriptors", true),
        ("missing-finish", true),
        ("fork", true),
        ("pkey", true),
        ("pkey", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut command = Command::new(binary);
        command
            .arg("diagnostic-log-host")
            .arg(directory.path())
            .arg(case);
        reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
        // The fixture host owns a child. Bound both processes if the transport
        // regresses, including a collector waiting for a missing completion.
        command
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                assert_eq!(
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) },
                    0
                );
                let output = child.wait_with_output().unwrap();
                panic!("diagnostic {case} exceeded 20 seconds: {output:?}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.stderr.is_empty(), "{output:?}");
        if case == "pkey" && output.status.code() == Some(77) {
            assert_eq!(output.stdout, b"diagnostic pkey: OSPKE unavailable\n");
            eprintln!("diagnostic pkey control unmeasured: OSPKE unavailable");
            continue;
        }
        assert!(output.status.success(), "{output:?}");
        let stdout = std::fs::read(directory.path().join("stdout.bin")).unwrap();
        let stderr = std::fs::read(directory.path().join("stderr.bin")).unwrap();
        let log = std::fs::read(directory.path().join("log.bin")).unwrap();
        let error = std::fs::read_to_string(directory.path().join("error.txt")).unwrap();
        let (calls, cleanups) = match case {
            "descriptors" | "missing-finish" => {
                assert_eq!(stdout, b"injected stdout\n");
                assert_eq!(stderr, b"tail-injected stderr\n");
                (4, usize::from(case != "missing-finish"))
            }
            "fork" => {
                assert_eq!(stdout, b"fallback fork child: hooks=0 traps=1 fallback=1 syscall=1\nfallback fork parent: hooks=0 traps=1 fallback=1 syscall=1\n");
                assert!(stderr.is_empty());
                (1, 2)
            }
            "pkey" => {
                assert!(stderr.is_empty());
                let text = std::str::from_utf8(&stdout).unwrap();
                let lines: Vec<_> = text.lines().collect();
                assert_eq!(lines.len(), 6, "{text}");
                let key: u32 = lines[1]
                    .strip_prefix("native pkey=")
                    .unwrap()
                    .strip_suffix(" pkru=0: state=preserved")
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(key > 0);
                let bytes: usize = lines[5]
                    .strip_prefix("fallback pkeys: zero=preserved key0-denied=preserved bytes=")
                    .unwrap()
                    .strip_suffix(" tool=424242 rpc=2")
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(bytes >= 576);
                assert_eq!(
                    text,
                    format!(
                        "pkey fixture: rseq=unregistered before native and Tool controls\nnative pkey={key} pkru=0: state=preserved\nnative pkey={key} pkru=1: state=preserved\nTool pkey={key} pkru=1: state=preserved\nTool pkey={key} pkru=0: state=preserved\nfallback pkeys: zero=preserved key0-denied=preserved bytes={bytes} tool=424242 rpc=2\n"
                    )
                );
                (2, 1)
            }
            _ => unreachable!(),
        };
        assert_eq!(
            log,
            [
                b"bootstrap ready\n".to_vec(),
                b"Tool syscall\n".repeat(calls),
                b"Tool cleanup\n".repeat(cleanups)
            ]
            .concat()
        );
        if case == "missing-finish" {
            assert_eq!(error, "guest log missing process completion");
        } else {
            assert!(error.is_empty(), "{error}");
        }
        assert_eq!(
            output.stdout,
            format!(
                "diagnostic {case}: rpc={calls} stdout={} stderr={} log={} missing-finish={}\n",
                stdout.len(),
                stderr.len(),
                log.len(),
                case == "missing-finish"
            )
            .as_bytes()
        );
        println!(
            "alt_stack={on_alt_stack} {}",
            String::from_utf8(output.stdout).unwrap()
        );
        print!("{}", String::from_utf8(stdout).unwrap());
    }
}

#[test]
fn ordinary_tool_memory_and_scratch_work_with_protected_guest_state() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    for on_alt_stack in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut coordinator = Command::new(binary)
            .arg("coordinator")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let ready = socket.exists();
        let mut command = Command::new(binary);
        command.arg("memory-access").arg(&socket);
        reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
        let output = ready.then(|| output_with_timeout(command, Duration::from_secs(20)));
        let _ = coordinator.kill();
        let _ = coordinator.wait();
        let output = output.expect("coordinator socket was not created");
        assert!(output.stderr.is_empty(), "{output:?}");
        if output.status.code() == Some(77) {
            assert_eq!(output.stdout, b"memory access: OSPKE unavailable\n");
            eprintln!("ordinary memory protected-state control unmeasured: OSPKE unavailable");
            continue;
        }
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let bytes = stdout.strip_prefix("memory access: rseq=unregistered native=2 Tool=2 pkru=0,1 xstate-bytes=")
            .and_then(|line| line.strip_suffix(" scratch=complete readlink=complete inspection=complete faults=EFAULT rpc=2 hooks=0 traps=2\n"))
            .expect("complete memory-access evidence").parse::<usize>().unwrap();
        assert!(bytes >= 576);
        println!("alt_stack={on_alt_stack} {stdout}");
    }
}

#[test]
fn unpatchable_syscall_dispatches_tool_after_signal_return() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut coordinator = Command::new(binary)
        .arg("coordinator")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let ready = socket.exists();
    let mut command = Command::new(binary);
    command.arg("syscall-fallback").arg(&socket);
    let output = ready.then(|| output_with_timeout(command, Duration::from_secs(20)));
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let output = output.expect("coordinator socket was not created");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"fallback: calls=6 rpc=7 hooks=0 bytes=unchanged abi=preserved\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn unpatchable_syscall_preserves_xstate_with_native_controls() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut coordinator = Command::new(binary)
        .arg("coordinator")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let ready = socket.exists();
    let mut command = Command::new(binary);
    command.arg("syscall-fallback-xstate").arg(&socket);
    let output = ready.then(|| output_with_timeout(command, Duration::from_secs(20)));
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let output = output.expect("coordinator socket was not created");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("fallback xstate: mask=0x"), "{stdout}");
    assert!(
        stdout.ends_with(" native=preserved clobber=detected tool=preserved\n"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    print!("{stdout}");
}

#[test]
fn unpatchable_syscall_preserves_protection_keys() {
    protection_keys_with_signal_stack(true);
}

#[test]
fn unpatchable_syscall_preserves_protection_keys_without_alt_stack() {
    protection_keys_with_signal_stack(false);
}

fn protection_keys_with_signal_stack(on_alt_stack: bool) {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut coordinator = Command::new(binary)
        .arg("coordinator")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let ready = socket.exists();
    let mut command = Command::new(binary);
    command.arg("syscall-fallback-pkey").arg(&socket);
    reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
    let output = ready.then(|| output_with_timeout(command, Duration::from_secs(20)));
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let output = output.expect("coordinator socket was not created");
    if output.status.code() == Some(77) {
        assert_eq!(output.stdout, b"fallback pkeys: OSPKE unavailable\n");
        assert!(output.stderr.is_empty(), "{output:?}");
        eprintln!("pkey control unavailable: OSPKE is not enabled");
        return;
    }
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(" pkru=0: state=preserved\n"), "{stdout}");
    assert!(stdout.contains(" pkru=1: state=preserved\n"), "{stdout}");
    assert!(
        stdout.contains("fallback pkeys: zero=preserved key0-denied=preserved bytes="),
        "{stdout}"
    );
    assert!(stdout.ends_with(" tool=424242 rpc=2\n"), "{stdout}");
    assert_eq!(stdout.lines().count(), 6, "{stdout}");
    assert!(
        stdout.starts_with("pkey fixture: rseq=unregistered before native and Tool controls\n"),
        "{stdout}"
    );
    println!("alt_stack={on_alt_stack}");
    print!("{stdout}");
}

#[test]
fn fork_child_accounts_for_fallback_and_installed_dispatch() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut coordinator = Command::new(binary)
        .arg("coordinator")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let ready = socket.exists();
    let outputs = ready.then(|| {
        ["installed", "fallback"].map(|kind| {
            let mut command = Command::new(binary);
            command.arg(format!("syscall-{kind}-fork")).arg(&socket);
            output_with_timeout(command, Duration::from_secs(20))
        })
    });
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let [installed, fallback] = outputs.expect("coordinator socket was not created");
    assert!(installed.status.success(), "{installed:?}");
    assert_eq!(installed.stdout, b"installed fork child: hooks=1 traps=0 fallback=0 syscall=0\ninstalled fork parent: hooks=2 traps=1 fallback=0 syscall=0\n");
    assert!(installed.stderr.is_empty(), "{installed:?}");
    assert!(fallback.status.success(), "{fallback:?}");
    assert_eq!(fallback.stdout, b"fallback fork child: hooks=0 traps=1 fallback=1 syscall=1\nfallback fork parent: hooks=0 traps=1 fallback=1 syscall=1\n");
    assert!(fallback.stderr.is_empty(), "{fallback:?}");
    print!(
        "{}{}",
        String::from_utf8(installed.stdout).unwrap(),
        String::from_utf8(fallback.stdout).unwrap()
    );
}

#[test]
fn fallback_refusal_is_counted_separately_from_tool_errors() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
    command.arg("syscall-fallback-refusal").arg("unused");
    let output = output_with_timeout(command, Duration::from_secs(20));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"fallback refusal: result=-95 attempts=1 refused=1 syscall=1 hooks=0 bytes=unchanged\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.is_empty());
    for line in stderr.lines() {
        let number = line
            .strip_prefix("reverie-liteinst: tool=compat syscall=")
            .expect("unexpected runtime diagnostic");
        number
            .parse::<i64>()
            .expect("invalid compatibility syscall record");
    }
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("child exceeded {timeout:?}: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn installed_hook_reentry_bypasses_tool_with_shared_coordinator_rpc() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = std::path::Path::new("/tmp").join(format!("li-rpc-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let socket = directory.join("coordinator.sock");

    let mut coordinator = Command::new(binary)
        .arg("coordinator")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "coordinator socket was not created");

    let rejected_handler = Command::new(binary)
        .arg("preinstalled-handler")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(rejected_handler.status.success(), "{rejected_handler:?}");
    assert_eq!(rejected_handler.stdout, b"preinstalled-handler-reset\n");

    let pending_sigsys = Command::new(binary)
        .arg("pending-sigsys")
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(
        pending_sigsys.status.code(),
        Some(126),
        "{pending_sigsys:?}"
    );

    let preblocked_sigsys = Command::new(binary)
        .arg("preblocked-sigsys")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(preblocked_sigsys.status.success(), "{preblocked_sigsys:?}");
    assert_eq!(preblocked_sigsys.stdout, b"inherited-sigsys-unblocked\n");

    let spoofed_sigsys = Command::new(binary)
        .arg("spoof-sigsys")
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(
        spoofed_sigsys.status.code(),
        Some(126),
        "{spoofed_sigsys:?}"
    );

    let mut instruction_positives = 0;
    let mut instruction_refusals = 0;
    for (mode, concurrent) in [
        ("instruction-guest", true),
        ("instruction-guest-quiescent", false),
    ] {
        let mut instruction_command = Command::new(binary);
        instruction_command.arg(mode).arg(&socket);
        if concurrent {
            instruction_command.env(
                STRADDLER_STALENESS_TICKS_ENV,
                TEST_STRADDLER_STALENESS_TICKS,
            );
        } else {
            instruction_command.env_remove(STRADDLER_STALENESS_TICKS_ENV);
        }
        let instruction_guest = output_with_timeout(instruction_command, Duration::from_secs(10));
        if instruction_guest.status.code() == Some(INSTRUCTION_CONTROL_UNAVAILABLE_STATUS) {
            assert!(
                instruction_guest.stdout.is_empty(),
                "{mode}: {instruction_guest:?}"
            );
            assert_eq!(
                instruction_guest.stderr, b"instruction-control-unavailable\n",
                "{mode}: the refusal path must emit exactly one diagnostic"
            );
            instruction_refusals += 1;
            eprintln!("instruction publication evidence: mode={mode} positive=0 refusal=1");
        } else {
            assert!(
                instruction_guest.status.success(),
                "{mode}: {instruction_guest:?}"
            );
            assert_eq!(
                instruction_guest.stdout,
                b"cpuid=tool rdtsc=tool rdtscp=tool rdrand=masked rdseed=masked instruction-handler-rpc=1 patched-native=1 first-use-native=1 nested-syscall-native=1 tool-callbacks=9\n",
                "{mode}"
            );
            assert!(
                instruction_guest.stderr.is_empty(),
                "{mode}: the successful instruction path emitted a refusal diagnostic: {instruction_guest:?}"
            );
            instruction_positives += 1;
            eprintln!("instruction publication evidence: mode={mode} positive=1 refusal=0");
        }
    }
    assert!(
        matches!(
            (instruction_positives, instruction_refusals),
            (2, 0) | (0, 2)
        ),
        "Concurrent and Quiescent instruction controls must agree: positive={instruction_positives} refusal={instruction_refusals}"
    );
    eprintln!(
        "instruction publication total: positive={instruction_positives} refusal={instruction_refusals}"
    );
    let instruction_control_available = instruction_positives == 2;

    if instruction_control_available {
        let nested_instruction_fork = Command::new(binary)
            .arg("nested-instruction-fork")
            .arg(&socket)
            .env(reverie_liteinst::IN_GUEST_STAGE_STREAM_ENV, "1")
            .output()
            .unwrap();
        assert!(
            nested_instruction_fork.status.success(),
            "{nested_instruction_fork:?}"
        );
        assert_eq!(
            nested_instruction_fork.stdout,
            b"nested-cpuid=native nested-rdtsc=native nested-rdtscp=native guest-cpuid=tool guest-rdtsc=tool guest-rdtscp=tool child-getpid=complete child-exit=0\n"
        );
        let nested_stderr = String::from_utf8(nested_instruction_fork.stderr).unwrap();
        let stage_count = |stage: &str| {
            nested_stderr
                .lines()
                .filter(|line| line.ends_with(stage))
                .count()
        };
        for stage in [
            "stage=fork-child-thread-start-begin",
            "stage=fork-child-thread-start-complete",
        ] {
            assert_eq!(
                stage_count(stage),
                1,
                "stage marker {stage:?} must appear exactly once: {nested_stderr}"
            );
        }
        assert!(
            stage_count("stage=nested-instruction-native-cpuid") >= 1,
            "at least the planted nested CPUID must take the native path: {nested_stderr}"
        );
        assert!(
            stage_count("stage=nested-instruction-fault-native-cpuid") >= 1,
            "unpatched Tool-internal CPUID must bypass publication: {nested_stderr}"
        );
        for stage in [
            "stage=nested-instruction-native-rdtsc",
            "stage=nested-instruction-native-rdtscp",
        ] {
            assert_eq!(
                stage_count(stage),
                1,
                "the planted nested instruction must take the native path once: {nested_stderr}"
            );
        }
    }

    let clock_and_vdso_guest = Command::new(binary)
        .arg("clock-and-vdso-guest")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(
        clock_and_vdso_guest.status.success(),
        "{clock_and_vdso_guest:?}"
    );
    let clock_and_vdso_stdout = String::from_utf8(clock_and_vdso_guest.stdout).unwrap();
    eprintln!("clock/vDSO evidence: {}", clock_and_vdso_stdout.trim_end());
    assert!(
        clock_and_vdso_stdout == "rcb=unmeasured vdso-calls=1\n"
            || clock_and_vdso_stdout.starts_with("rcb=measured "),
        "{clock_and_vdso_stdout}"
    );
    assert!(
        clock_and_vdso_stdout.ends_with("vdso-calls=1\n"),
        "{clock_and_vdso_stdout}"
    );

    let unsubscribed_lifecycle = Command::new(binary)
        .arg("unsubscribed-lifecycle")
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(
        unsubscribed_lifecycle.status.code(),
        Some(0x34),
        "{unsubscribed_lifecycle:?}"
    );
    assert_eq!(
        unsubscribed_lifecycle.stdout,
        b"unsubscribed-clone-rejected\n"
    );
    assert_eq!(
        unsubscribed_lifecycle.stderr,
        b"unsubscribed-thread=Exited(52)\nunsubscribed-process=Exited(52)\n"
    );

    let injected_exit = Command::new(binary)
        .arg("injected-exit")
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(injected_exit.status.code(), Some(0x34), "{injected_exit:?}");
    assert!(injected_exit.stdout.is_empty(), "{injected_exit:?}");
    assert_eq!(
        injected_exit.stderr,
        b"injected-thread=Exited(52)\ninjected-process=Exited(52)\n"
    );

    let fork_guest = Command::new(binary)
        .arg("fork-guest")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(fork_guest.status.success(), "{fork_guest:?}");
    let fork_stdout = String::from_utf8(fork_guest.stdout).unwrap();
    let mut fields = fork_stdout.split_whitespace();
    let fork_total: u64 = fields
        .next()
        .and_then(|field| field.strip_prefix("fork-rpc-total="))
        .expect("fork guest must print the shared RPC total")
        .parse()
        .unwrap();
    let sender_delta: u64 = fields
        .next()
        .and_then(|field| field.strip_prefix("fork-rpc-sender-delta="))
        .expect("fork guest must print the shared RPC sender delta")
        .parse()
        .unwrap();
    assert_eq!(fields.next(), None, "{fork_stdout}");
    assert!(fork_total >= 5, "{fork_stdout}");
    assert_eq!(sender_delta, 1, "{fork_stdout}");

    // Same contract, but the child is created by a bare `SYS_fork` instruction
    // that never enters libc — the shape Go's runtime and hand-written
    // `syscall(2)` sites produce. A `pthread_atfork`-based detector cannot see
    // this fork, so the child would silently keep sending on the parent's
    // inherited connection and the sender delta would be 0.
    let raw_fork_guest = Command::new(binary)
        .arg("raw-fork-guest")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(raw_fork_guest.status.success(), "{raw_fork_guest:?}");
    let raw_fork_stdout = String::from_utf8(raw_fork_guest.stdout).unwrap();
    let mut raw_fields = raw_fork_stdout.split_whitespace();
    let raw_fork_total: u64 = raw_fields
        .next()
        .and_then(|field| field.strip_prefix("raw-fork-rpc-total="))
        .expect("raw fork guest must print the shared RPC total")
        .parse()
        .unwrap();
    let raw_sender_delta: u64 = raw_fields
        .next()
        .and_then(|field| field.strip_prefix("raw-fork-rpc-sender-delta="))
        .expect("raw fork guest must print the shared RPC sender delta")
        .parse()
        .unwrap();
    assert_eq!(raw_fields.next(), None, "{raw_fork_stdout}");
    assert!(raw_fork_total >= 5, "{raw_fork_stdout}");
    assert_eq!(
        raw_sender_delta, 1,
        "a raw SYS_fork child must reconnect under its own identity: {raw_fork_stdout}"
    );

    for (mode, expected) in [
        (
            "clone3-guest",
            b"clone3=child-reconstructed sender-delta=1\n".as_slice(),
        ),
        (
            "vfork-guest",
            b"vfork=translated-cow-child sender-delta=1\n".as_slice(),
        ),
    ] {
        let output = Command::new(binary)
            .arg(mode)
            .arg(&socket)
            .output()
            .unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        eprintln!(
            "process evidence: {}",
            String::from_utf8_lossy(&output.stdout).trim_end()
        );
        assert_eq!(output.stdout, expected, "{mode}: {output:?}");
    }

    for (mode, expected) in [
        (
            "unsubscribed-fork",
            b"unsubscribed-fork-reconstructed\n".as_slice(),
        ),
        ("tail-fork", b"tail-fork-reconstructed\n".as_slice()),
    ] {
        let output = Command::new(binary)
            .arg(mode)
            .arg(&socket)
            .output()
            .unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        assert_eq!(output.stdout, expected, "{mode}: {output:?}");
    }

    let guest = Command::new(binary)
        .arg("guest")
        .arg(&socket)
        .output()
        .unwrap();
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let _ = std::fs::remove_dir_all(&directory);

    assert!(
        guest.status.success(),
        "status={} stdout={} stderr={}",
        guest.status,
        String::from_utf8_lossy(&guest.stdout),
        String::from_utf8_lossy(&guest.stderr)
    );
    let stdout = String::from_utf8(guest.stdout).unwrap();
    assert!(
        stdout.starts_with(
            "calls=32 traps=1 hooks=32 rpc_delta=34 nested_traps=1 nested_hooks=33 mask_traps=1 mask_hooks=33 mask_result=-1 first_use_exec_result=-95 first_use_signal_result=-1 "
        ),
        "{stdout}"
    );
}
