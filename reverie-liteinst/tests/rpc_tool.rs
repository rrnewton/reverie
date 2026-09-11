use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;

const INSTRUCTION_CONTROL_UNAVAILABLE_STATUS: i32 = 77;
const TEST_STRADDLER_STALENESS_TICKS: &str = "20000";

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_syscalls_unarmed_qualify_native_result_and_once_only_pipe_read() {
    let output = sud_guest("sud-owned-cpuid-syscall-unarmed");
    check_owned_syscalls(output, false);
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_syscalls_armed_cancel_and_replace_with_exact_timer_positions() {
    check_owned_syscalls(sud_guest("sud-owned-cpuid-syscall-unarmed"), false);
    for work in [0, 20000] {
        for repeat in 0..3 {
            println!("owned-syscall work={work} repeat={repeat}");
            check_owned_syscalls(
                sud_guest(&format!("sud-owned-cpuid-syscall-armed-{work}")),
                true,
            );
        }
    }
}

#[cfg(feature = "test-owned-cpuid")]
fn check_owned_syscalls(output: Output, armed: bool) {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    print!("{text}");
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("syscall-result:"))
            .count(),
        3
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("syscall-timer-position:"))
            .count(),
        if armed { 2 } else { 0 }
    );
    if armed {
        assert!(text.contains("event=1 generation=1 sequence=8"), "{text}");
        assert!(text.contains("event=4 generation=3 sequence=1"), "{text}");
        assert!(
            text.contains(
                "callbacks=5 rpc=5 injections=3 timers=2 completed=10 clocks=[0, 3, 3, 3, 3, 6]"
            ),
            "{text}"
        );
    } else {
        assert!(
            text.contains(
                "callbacks=3 rpc=3 injections=3 timers=0 completed=0 clocks=[0, 3, 3, 6]"
            ),
            "{text}"
        );
    }
    assert!(
        text.contains("pipe=first-and-second-preserved state=preserved patches=0"),
        "{text}"
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_precise_timers_deliver_crossing_suffix_and_cancel_before_cpuid() {
    for work in [0, 20000] {
        for repeat in 0..3 {
            let output = sud_guest(&format!("sud-owned-cpuid-timer-{work}"));
            assert!(
                output.status.success(),
                "work={work} repeat={repeat} {output:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            print!("work={work} repeat={repeat} {text}");
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with("timer-position:"))
                    .count(),
                3
            );
            assert!(text.contains("event=1 generation=1 sequence=7"), "{text}");
            assert!(text.contains("event=2 generation=2 sequence=2"), "{text}");
            assert!(text.contains("event=4 generation=5 sequence=1"), "{text}");
            assert!(
                text.contains(
                    "callbacks=6 rpc=6 timers=3 completed=11 clocks=[0, 3, 3, 3, 3, 3, 6]"
                ),
                "{text}"
            );
            assert!(text.contains("state=preserved patches=0"), "{text}");
        }
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_nop_steps_preserve_state_without_timer_delivery() {
    let output = sud_guest("sud-owned-cpuid-step");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let inventory = match std::fs::metadata("/proc/self/timers") {
        Ok(_) => "Empty",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "Unavailable",
        Err(error) => panic!("cannot inspect POSIX timer inventory: {error}"),
    };
    assert_eq!(output.stdout, format!("owned-step: callbacks=2 rpc=2 completed=2 clocks=[0,0,3] state=preserved patches=0 timers=unsupported posix_inventory={inventory} fp_profile=seeded-hi16-zmm\n").as_bytes());
}

#[test]
fn sud_public_cpuid_install_refuses_before_activation() {
    let output = sud_guest("sud-cpuid-public");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"sud-cpuid-public: refused before activation mask=preserved\n"
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_blocked_sigsegv_refuses_before_activation() {
    let output = sud_guest("sud-owned-cpuid-blocked");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"owned-cpuid-blocked: refused before activation mask=preserved callbacks=0\n"
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_clocked_installer_failure_exits_without_entering_probe() {
    let output = sud_guest("sud-owned-cpuid-clock-install-failure");
    assert_eq!(output.status.code(), Some(127), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        output.stderr,
        b"owned-clock: installer refused Unsupported; probe not entered\n"
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_clocked_instructions_preserve_full_guest_rcb_trajectory() {
    let inventory = match std::fs::metadata("/proc/self/timers") {
        Ok(_) => "Empty",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "Unavailable",
        Err(error) => panic!("cannot inspect POSIX timer inventory availability: {error}"),
    };
    for work in [0, 20_000] {
        for repeat in 0..3 {
            let output = sud_guest(&format!("sud-owned-cpuid-clock-{work}"));
            assert!(
                output.status.success(),
                "work={work} repeat={repeat}: {output:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_eq!(output.stdout, format!("owned-clock: samples=[7, 8, 9, 10, 17, 18, 19, 19, 21, 24] deltas=[1, 1, 1, 7, 1, 1, 0, 2, 3] callbacks=9 rpc=9 work={work} state=preserved patches=0 timers=unsupported posix_inventory={inventory} fp_profile=seeded-hi16-zmm\n").as_bytes());
        }
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_mixed_instructions_use_consecutive_tool_rpc_callbacks_without_patching() {
    let inventory = match std::fs::metadata("/proc/self/timers") {
        Ok(_) => "Empty",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "Unavailable",
        Err(error) => panic!("cannot inspect POSIX timer inventory availability: {error}"),
    };
    let output = sud_guest("sud-owned-cpuid-mixed");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(output.stdout, format!("owned-instructions: cpuid=3 rdtsc=3 rdtscp=3 consecutive=9 rpc=9 state=preserved bytes=unchanged patches=0 posix_inventory={inventory} fp_profile=seeded-hi16-zmm\n").as_bytes());
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_zero_hi16_xsave_baseline() {
    check_xsave_control("sud-owned-cpuid-xsave-zero-baseline");
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_zero_hi16_ordinary_signal_return() {
    check_xsave_control("sud-owned-cpuid-xsave-zero-signal");
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_unseeded_xsave_baseline() {
    check_xsave_control("sud-owned-cpuid-xsave-baseline");
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_unseeded_ordinary_signal_return() {
    check_xsave_control("sud-owned-cpuid-xsave-signal");
}

#[cfg(feature = "test-owned-cpuid")]
fn check_xsave_control(mode: &str) {
    let output = sud_guest(mode);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        output
            .stdout
            .ends_with(b"xsave-control: full_bytes=preserved callbacks=0\n"),
        "{output:?}"
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_unseeded_uses_consecutive_tool_rpc_callbacks_without_patching() {
    check_owned_cpuid_profile("sud-owned-cpuid", "unseeded");
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_seeded_hi16_zmm_uses_consecutive_tool_rpc_callbacks_without_patching() {
    check_owned_cpuid_profile("sud-owned-cpuid-seeded", "seeded-hi16-zmm");
}

#[cfg(feature = "test-owned-cpuid")]
fn check_owned_cpuid_profile(mode: &str, profile: &str) {
    let inventory = match std::fs::metadata("/proc/self/timers") {
        Ok(_) => "Empty",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "Unavailable",
        Err(error) => panic!("cannot inspect POSIX timer inventory availability: {error}"),
    };
    let output = sud_guest(mode);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("owned-cpuid: consecutive=6 rpc=6 state=preserved bytes=unchanged patches=0 posix_inventory={inventory} fp_profile={profile}\n").as_bytes()
    );
}

#[cfg(feature = "test-owned-cpuid")]
#[test]
fn sud_owned_cpuid_refuses_unadmitted_sources_controls_and_timers() {
    for mode in ["sud-owned-cpuid-source", "sud-owned-cpuid-control"] {
        let output = sud_guest(mode);
        assert_eq!(output.status.code(), Some(126), "{mode}: {output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{mode}: {output:?}"
        );
    }
    let output = sud_guest("sud-owned-cpuid-timer");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"owned-cpuid-timer: refused before callbacks\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn sud_only_typed_mask_queries_errors_and_tail_restoration() {
    let output = sud_guest("sud-masks");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"sud-masks: inject/tail/query/error/restore callbacks=7 patches=0\n"
    );
}

#[test]
fn sud_runtime_source_admission_preserves_masks_and_refuses_unknown_sources() {
    for mode in [
        "sud-policy-blocked",
        "sud-policy-ignored",
        "sud-policy-ordinary",
    ] {
        let output = sud_guest(mode);
        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(output.stderr.is_empty(), "{mode}: {output:?}");
        assert_eq!(
            output.stdout,
            format!("{mode}: refused, original mask/disposition preserved\n").as_bytes()
        );
    }
    let output = sud_guest("sud-policy-unknown");
    assert_eq!(output.status.code(), Some(126), "{output:?}");
    assert!(
        output.stderr.is_empty() && output.stdout.is_empty(),
        "{output:?}"
    );
}

#[test]
fn sud_only_shared_tool_without_guest_patching() {
    let output = sud_guest("sud-only");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(
        text.starts_with("sud-only: guest_probes=15 rpc=16 bytes=unchanged abi=preserved "),
        "{text}"
    );
    println!("{text}");
}

#[test]
fn bootstrap_installers_preserve_environment_and_forward_modes() {
    let output = sud_guest("sud-bootstrap-only");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(
        text.starts_with(
            "sud-bootstrap-only: guest_probes=15 rpc=16 bytes=unchanged abi=preserved "
        ),
        "{text}"
    );

    let output = sud_guest("bootstrap-guest");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = std::str::from_utf8(&output.stdout).unwrap();
    assert!(
        text.starts_with("calls=32 traps=1 hooks=32 rpc_delta=34 "),
        "{text}"
    );
}

#[test]
fn sud_only_abi_signal_and_subscription_refusals() {
    use std::os::unix::process::ExitStatusExt;
    for mode in ["sud-x32", "sud-compat"] {
        let output = sud_guest(mode);
        assert_eq!(output.status.code(), Some(126), "{mode}: {output:?}");
    }
    for mode in ["sud-handler", "sud-instructions", "sud-vdso", "sud-clock"] {
        let output = sud_guest(mode);
        assert!(output.status.success(), "{mode}: {output:?}");
        assert_eq!(output.stdout, format!("{mode}: refused\n").as_bytes());
        assert!(output.stderr.is_empty(), "{mode}: {output:?}");
    }
    for mode in [
        "bootstrap-sud-handler",
        "bootstrap-sud-instructions",
        "bootstrap-sud-vdso",
        "bootstrap-sud-clock",
    ] {
        let output = sud_guest(mode);
        assert!(output.status.success(), "{mode}: {output:?}");
        assert_eq!(output.stdout, format!("{mode}: refused\n").as_bytes());
        assert!(output.stderr.is_empty(), "{mode}: {output:?}");
    }
    let output = sud_guest("sud-ptrace-control");
    assert_eq!(output.status.signal(), Some(libc::SIGSYS), "{output:?}");
}

fn sud_guest(mode: &str) -> Output {
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
    let mut command = Command::new(binary);
    command.arg(mode).arg(&socket);
    let output = socket.exists().then(|| {
        if mode.starts_with("sud-owned-cpuid-syscall-") {
            owned_syscall_output(command)
        } else {
            output_with_timeout(command, Duration::from_secs(20))
        }
    });
    let _ = coordinator.kill();
    let _ = coordinator.wait();
    let output = output.expect("coordinator socket was not created");
    println!("{mode}: {output:?}");
    output
}

fn owned_syscall_output(mut command: Command) -> Output {
    let directory = tempfile::Builder::new()
        .prefix("sud")
        .tempdir()
        .unwrap()
        .keep();
    let stdout = directory.join("stdout");
    let stderr = directory.join("stderr");
    command.env("REVERIE_OWNED_SYSCALL_EVIDENCE", directory.join("frames"));
    command.stdout(std::fs::File::create(&stdout).unwrap());
    command.stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(10));
    };
    println!("owned syscall raw streams/evidence: {directory:?} status={status}");
    Output {
        status,
        stdout: std::fs::read(stdout).unwrap(),
        stderr: std::fs::read(stderr).unwrap(),
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
