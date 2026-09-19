use std::process::Command;
use std::process::Output;
use std::time::Duration;

use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;

const INSTRUCTION_CONTROL_UNAVAILABLE_STATUS: i32 = 77;
const TEST_STRADDLER_STALENESS_TICKS: &str = "20000";

#[test]
fn fallback_uses_owned_frames_after_guest_stack_revocation() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let mut reports = Vec::new();
    for on_alt_stack in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut command = Command::new(binary);
        command
            .arg("owned-frame")
            .arg(&socket)
            .env_remove(STRADDLER_STALENESS_TICKS_ENV);
        reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
        let report = owned_frame_report(command);
        let output = &report.output;

        assert!(output.stderr.is_empty(), "{output:?}");
        if output.status.code() == Some(77) {
            assert_eq!(output.stdout, b"owned frame: OSPKE unavailable\n");
            eprintln!("owned frame PKRU controls unmeasured: OSPKE unavailable");
            continue;
        }
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("owned frame observation: "))
                .count(),
            8
        );
        let bytes = stdout.lines().last().unwrap()
            .strip_prefix("owned frame: rseq=unregistered native=4 Tool=4 registers=equal xstate-bytes=")
            .and_then(|line| line.strip_suffix(" callback-stack=owned guest-stack-revoked=preserved negative-pkru=preserved hooks=0 traps=4"))
            .expect("complete owned-frame observations").parse::<usize>().unwrap();
        assert!(bytes >= 576);
        println!("alt_stack={on_alt_stack}\n{stdout}");
        reports.push(report);
    }
    hardware_method_result(
        "fallback_uses_owned_frames_after_guest_stack_revocation",
        &reports,
        0,
        0,
    );
}

fn owned_frame_report(command: Command) -> supervised::Report {
    supervised_report(command, Duration::from_secs(5), Some(1024 * 1024))
}

#[test]
fn ordinary_tool_memory_and_scratch_work_with_protected_guest_state() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let mut reports = Vec::new();
    for on_alt_stack in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut command = Command::new(binary);
        command.arg("memory-access").arg(&socket);
        reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
        let report = report_with_timeout(command, Duration::from_secs(20));
        let output = &report.output;

        assert!(output.stderr.is_empty(), "{output:?}");
        if output.status.code() == Some(77) {
            assert_eq!(output.stdout, b"memory access: OSPKE unavailable\n");
            eprintln!("ordinary memory protected-state control unmeasured: OSPKE unavailable");
            continue;
        }
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        let bytes = stdout.strip_prefix("memory access: rseq=unregistered native=2 Tool=2 pkru=0,1 xstate-bytes=")
            .and_then(|line| line.strip_suffix(" scratch=complete readlink=complete inspection=complete faults=EFAULT rpc=2 hooks=0 traps=2\n"))
            .expect("complete memory-access evidence").parse::<usize>().unwrap();
        assert!(bytes >= 576);
        println!("alt_stack={on_alt_stack} {stdout}");
        reports.push(report);
    }
    hardware_method_result(
        "ordinary_tool_memory_and_scratch_work_with_protected_guest_state",
        &reports,
        0,
        0,
    );
}

#[test]
fn unpatchable_syscall_dispatches_tool_after_signal_return() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut command = Command::new(binary);
    command.arg("syscall-fallback").arg(&socket);
    let report = report_with_timeout(command, Duration::from_secs(20));
    let output = &report.output;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"fallback: calls=6 rpc=7 hooks=0 bytes=unchanged abi=preserved\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    hardware_method_result(
        "unpatchable_syscall_dispatches_tool_after_signal_return",
        &[report],
        0,
        0,
    );
}

#[test]
fn unpatchable_syscall_preserves_xstate_with_native_controls() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut command = Command::new(binary);
    command.arg("syscall-fallback-xstate").arg(&socket);
    let report = report_with_timeout(command, Duration::from_secs(20));
    let output = &report.output;

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(stdout.starts_with("fallback xstate: mask=0x"), "{stdout}");
    assert!(
        stdout.ends_with(" native=preserved clobber=detected tool=preserved\n"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    print!("{stdout}");
    hardware_method_result(
        "unpatchable_syscall_preserves_xstate_with_native_controls",
        &[report],
        0,
        0,
    );
}

#[test]
fn unpatchable_syscall_preserves_protection_keys() {
    protection_keys_with_signal_stack(
        true,
        "unpatchable_syscall_preserves_protection_keys",
    );
}

#[test]
fn unpatchable_syscall_preserves_protection_keys_without_alt_stack() {
    protection_keys_with_signal_stack(
        false,
        "unpatchable_syscall_preserves_protection_keys_without_alt_stack",
    );
}

fn protection_keys_with_signal_stack(on_alt_stack: bool, method: &str) {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut command = Command::new(binary);
    command.arg("syscall-fallback-pkey").arg(&socket);
    reverie_liteinst::set_guest_alt_stack(&mut command, on_alt_stack);
    let report = report_with_timeout(command, Duration::from_secs(20));
    let output = &report.output;

    if output.status.code() == Some(77) {
        assert_eq!(output.stdout, b"fallback pkeys: OSPKE unavailable\n");
        assert!(output.stderr.is_empty(), "{output:?}");
        eprintln!("pkey control unavailable: OSPKE is not enabled");
        #[cfg(feature = "rcb-qualification")]
        panic!("{method}: selected hardware method has no counter result");
        #[cfg(not(feature = "rcb-qualification"))]
        return;
    }
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
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
    hardware_method_result(method, &[report], 0, 0);
}

#[test]
fn fork_child_accounts_for_fallback_and_installed_dispatch() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let reports = {
        ["installed", "fallback"].map(|kind| {
            let mut command = Command::new(binary);
            command.arg(format!("syscall-{kind}-fork")).arg(&socket);
            report_with_timeout(command, Duration::from_secs(20))
        })
    };

    let [installed_report, fallback_report] = &reports;
    let installed = &installed_report.output;
    let fallback = &fallback_report.output;
    assert!(installed.status.success(), "{installed:?}");
    assert_eq!(installed.stdout, b"installed fork child: hooks=1 traps=0 fallback=0 syscall=0\ninstalled fork parent: hooks=2 traps=1 fallback=0 syscall=0\n");
    assert!(installed.stderr.is_empty(), "{installed:?}");
    assert!(fallback.status.success(), "{fallback:?}");
    assert_eq!(fallback.stdout, b"fallback fork child: hooks=0 traps=1 fallback=1 syscall=1\nfallback fork parent: hooks=0 traps=1 fallback=1 syscall=1\n");
    assert!(fallback.stderr.is_empty(), "{fallback:?}");
    print!(
        "{}{}",
        String::from_utf8(installed.stdout.clone()).unwrap(),
        String::from_utf8(fallback.stdout.clone()).unwrap()
    );
    hardware_method_result(
        "fork_child_accounts_for_fallback_and_installed_dispatch",
        &reports,
        0,
        0,
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

fn output_with_timeout(command: Command, timeout: Duration) -> Output {
    supervised_output(command, timeout, None)
}

fn report_with_timeout(command: Command, timeout: Duration) -> supervised::Report {
    supervised_report(command, timeout, None)
}

#[path = "common/supervisor.rs"]
mod supervised;

fn supervised_output(command: Command, timeout: Duration, output_limit: Option<u64>) -> Output {
    supervised_report(command, timeout, output_limit).output
}

fn supervised_report(
    command: Command,
    timeout: Duration,
    output_limit: Option<u64>,
) -> supervised::Report {
    supervised::run(command, timeout, output_limit)
}

#[cfg(feature = "rcb-qualification")]
fn hardware_method_result(
    method: &str,
    reports: &[supervised::Report],
    native_rows: usize,
    mediated_rows: usize,
) {
    use std::collections::BTreeMap;

    let expected_modes: &[&str] = match method {
        "fallback_uses_owned_frames_after_guest_stack_revocation" =>
            &["owned-frame", "owned-frame"],
        "ordinary_tool_memory_and_scratch_work_with_protected_guest_state" =>
            &["memory-access", "memory-access"],
        "unpatchable_syscall_dispatches_tool_after_signal_return" => &["syscall-fallback"],
        "unpatchable_syscall_preserves_xstate_with_native_controls" =>
            &["syscall-fallback-xstate"],
        "unpatchable_syscall_preserves_protection_keys"
        | "unpatchable_syscall_preserves_protection_keys_without_alt_stack" =>
            &["syscall-fallback-pkey"],
        "fork_child_accounts_for_fallback_and_installed_dispatch" => &[
            "syscall-fallback-fork",
            "syscall-fallback-fork-child",
            "syscall-installed-fork",
            "syscall-installed-fork-child",
        ],
        "installed_hook_reentry_bypasses_tool_with_shared_coordinator_rpc" =>
            &["clock-and-vdso-guest"],
        "initial_and_child_config_callbacks_keep_private_sockets_protected" => &[
            "bootstrap-admission",
            "bootstrap-admission",
            "bootstrap-admission-child",
            "bootstrap-admission-child",
        ],
        _ => panic!("unselected hardware method {method}"),
    };
    let mut events = BTreeMap::new();
    let mut results = BTreeMap::new();
    for report in reports {
        assert!(
            report.counter_unavailable.is_empty(),
            "{method}: unavailable counter observations: {:?}",
            report.counter_unavailable
        );
        assert!(
            !report.events.is_empty(),
            "{method}: every supervised process needs an authenticated acquisition"
        );
        assert_eq!(
            report.events.len(),
            report.counter_results.len(),
            "{method}: every supervised process needs its own counter results"
        );
        for event in &report.events {
            assert_eq!(event[0], 1, "{method}: acquisition version");
            assert_ne!(event[5], 0, "{method}: event identity");
            assert_eq!(event[6], 4, "{method}: PERF_TYPE_RAW profile");
            assert!(
                matches!(event[7], 0x5101c4 | 0x5100d1),
                "{method}: exact retired-conditional-branch profile"
            );
            assert!(event[8] <= i32::MAX as u64, "{method}: target CPU");
            assert_eq!(event[9], 0, "{method}: event was acquired disabled");
            assert!(
                events.insert(event[5], *event).is_none(),
                "{method}: duplicate acquisition event"
            );
        }
        for result in &report.counter_results {
            assert_ne!(result.event_id, 0, "{method}: counter result event identity");
            assert!(result.fd <= i32::MAX as u64, "{method}: counter descriptor");
            assert_ne!(result.owner, 0, "{method}: counter owner");
            assert!(result.cpu <= i32::MAX as u64, "{method}: counter CPU");
            assert!(result.clock > 0, "{method}: actual counter result");
            assert!(
                results.insert(result.event_id, result).is_none(),
                "{method}: duplicate counter result"
            );
        }
    }
    assert!(!events.is_empty(), "{method}: zero authenticated acquisitions");
    assert_eq!(
        results.len(),
        events.len(),
        "{method}: every acquisition needs its own actual counter result"
    );
    let mut owners = Vec::new();
    let mut result_owners = Vec::new();
    let mut event_types = Vec::new();
    let mut configs = Vec::new();
    let mut cpus = Vec::new();
    let mut result_cpus = Vec::new();
    let mut disabled = Vec::new();
    let mut fds = Vec::new();
    let mut counters = Vec::new();
    let mut modes = Vec::new();
    for (event_id, event) in &events {
        let result = results
            .get(event_id)
            .unwrap_or_else(|| panic!("{method}: missing result for event {event_id}"));
        assert_eq!(result.owner, event[4], "{method}: target owner join");
        assert_eq!(result.cpu, event[8], "{method}: target CPU join");
        owners.push(event[4]);
        result_owners.push(result.owner);
        event_types.push(event[6]);
        configs.push(event[7]);
        cpus.push(event[8]);
        result_cpus.push(result.cpu);
        disabled.push(event[9]);
        fds.push(result.fd);
        counters.push(result.clock);
        modes.push(result.mode.as_str());
    }
    let mut observed_modes = modes.clone();
    observed_modes.sort_unstable();
    let mut expected_modes = expected_modes.to_vec();
    expected_modes.sort_unstable();
    assert_eq!(
        observed_modes, expected_modes,
        "{method}: counter results came from another guest mode"
    );
    let list = |values: &[u64]| {
        values
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let event_ids = events.keys().copied().collect::<Vec<_>>();
    let result_event_ids = results.keys().copied().collect::<Vec<_>>();
    println!(
        "liteinst hardware method: phase=run-liteinst-existing binary=rpc_tool method={method} events={} results={} event-ids={} result-event-ids={} owners={} result-owners={} event-types={} configs={} cpus={} result-cpus={} initially-disabled={} fds={} counters={} modes={} native-rows={native_rows} mediated-rows={mediated_rows}",
        events.len(),
        results.len(),
        list(&event_ids),
        list(&result_event_ids),
        list(&owners),
        list(&result_owners),
        list(&event_types),
        list(&configs),
        list(&cpus),
        list(&result_cpus),
        list(&disabled),
        list(&fds),
        list(&counters),
        modes.join(","),
    );
}

#[cfg(not(feature = "rcb-qualification"))]
fn hardware_method_result(
    _method: &str,
    _reports: &[supervised::Report],
    _native_rows: usize,
    _mediated_rows: usize,
) {
}

#[test]
fn installed_hook_reentry_bypasses_tool_with_shared_coordinator_rpc() {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest");
    let directory = std::path::Path::new("/tmp").join(format!("li-rpc-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let socket = directory.join("coordinator.sock");

    let rejected_handler = {
        let mut command = Command::new(binary);
        command.arg("preinstalled-handler").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
    assert!(rejected_handler.status.success(), "{rejected_handler:?}");
    assert_eq!(rejected_handler.stdout, b"preinstalled-handler-reset\n");

    let pending_sigsys = {
        let mut command = Command::new(binary);
        command.arg("pending-sigsys").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
    assert_eq!(
        pending_sigsys.status.code(),
        Some(126),
        "{pending_sigsys:?}"
    );

    let preblocked_sigsys = {
        let mut command = Command::new(binary);
        command.arg("preblocked-sigsys").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
    assert!(preblocked_sigsys.status.success(), "{preblocked_sigsys:?}");
    assert_eq!(preblocked_sigsys.stdout, b"inherited-sigsys-unblocked\n");

    let spoofed_sigsys = {
        let mut command = Command::new(binary);
        command.arg("spoof-sigsys").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
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
        let nested_instruction_fork = {
            let mut command = Command::new(binary);
            command
                .arg("nested-instruction-fork")
                .arg(&socket)
                .env(reverie_liteinst::IN_GUEST_STAGE_STREAM_ENV, "1");
            output_with_timeout(command, Duration::from_secs(20))
        };
        assert!(
            nested_instruction_fork.status.success(),
            "{nested_instruction_fork:?}"
        );
        assert_eq!(
            nested_instruction_fork.stdout,
            b"child-acquisition-cpuid=0 child-guest-cpuid-callbacks=1 virtual-cpuid-eax=0x11111111\nnested-cpuid=native nested-rdtsc=native nested-rdtscp=native guest-cpuid=tool guest-rdtsc=tool guest-rdtscp=tool child-getpid=complete child-exit=0\n"
        );
        let nested_stderr = String::from_utf8(nested_instruction_fork.stderr).unwrap();
        let stage_count = |stage: &str| {
            nested_stderr
                .lines()
                .filter(|line| line.ends_with(stage))
                .count()
        };
        for stage in [
            "stage=fork-child-clock-acquire-begin",
            "stage=fork-child-clock-acquire-complete",
            "stage=fork-child-thread-start-begin",
            "stage=fork-child-thread-start-complete",
        ] {
            assert_eq!(
                stage_count(stage),
                1,
                "stage marker {stage:?} must appear exactly once: {nested_stderr}"
            );
        }
        let lines: Vec<_> = nested_stderr.lines().collect();
        let clock_begin = lines
            .iter()
            .position(|line| line.ends_with("stage=fork-child-clock-acquire-begin"))
            .unwrap();
        let clock_complete = lines
            .iter()
            .position(|line| line.ends_with("stage=fork-child-clock-acquire-complete"))
            .unwrap();
        assert!(clock_begin < clock_complete, "{nested_stderr}");
        assert!(
            lines[clock_begin + 1..clock_complete]
                .iter()
                .all(|line| !line.contains("cpuid")),
            "fork-child profile import executed CPUID: {nested_stderr}"
        );
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

    let clock_and_vdso_report = {
        let mut command = Command::new(binary);
        command.arg("clock-and-vdso-guest").arg(&socket);
        report_with_timeout(command, Duration::from_secs(20))
    };
    let clock_and_vdso_guest = &clock_and_vdso_report.output;
    assert!(
        clock_and_vdso_guest.status.success(),
        "{clock_and_vdso_guest:?}"
    );
    let clock_and_vdso_stdout =
        String::from_utf8(clock_and_vdso_guest.stdout.clone()).unwrap();
    eprintln!("clock/vDSO evidence: {}", clock_and_vdso_stdout.trim_end());
    assert!(
        clock_and_vdso_stdout.starts_with("rcb=measured "),
        "{clock_and_vdso_stdout}"
    );
    assert!(
        clock_and_vdso_stdout.ends_with("vdso-calls=1\n"),
        "{clock_and_vdso_stdout}"
    );
    hardware_method_result(
        "installed_hook_reentry_bypasses_tool_with_shared_coordinator_rpc",
        &[clock_and_vdso_report],
        0,
        0,
    );

    let unsubscribed_lifecycle = {
        let mut command = Command::new(binary);
        command.arg("unsubscribed-lifecycle").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
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

    let injected_exit = {
        let mut command = Command::new(binary);
        command.arg("injected-exit").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
    assert_eq!(injected_exit.status.code(), Some(0x34), "{injected_exit:?}");
    assert!(injected_exit.stdout.is_empty(), "{injected_exit:?}");
    assert_eq!(
        injected_exit.stderr,
        b"injected-thread=Exited(52)\ninjected-process=Exited(52)\n"
    );

    let fork_guest = {
        let mut command = Command::new(binary);
        command.arg("fork-guest").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
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
    let raw_fork_guest = {
        let mut command = Command::new(binary);
        command.arg("raw-fork-guest").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };
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
        let output = {
            let mut command = Command::new(binary);
            command.arg(mode).arg(&socket);
            output_with_timeout(command, Duration::from_secs(20))
        };
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
        let output = {
            let mut command = Command::new(binary);
            command.arg(mode).arg(&socket);
            output_with_timeout(command, Duration::from_secs(20))
        };
        assert!(output.status.success(), "{mode}: {output:?}");
        assert_eq!(output.stdout, expected, "{mode}: {output:?}");
    }

    let guest = {
        let mut command = Command::new(binary);
        command.arg("guest").arg(&socket);
        output_with_timeout(command, Duration::from_secs(20))
    };

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

#[test]
fn initial_and_child_config_callbacks_keep_private_sockets_protected() {
    let mut reports = Vec::new();
    for use_alt_stack in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
        command.arg("bootstrap-admission").arg(&socket);
        reverie_liteinst::set_guest_alt_stack(&mut command, use_alt_stack);
        let report = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
        assert!(report.output.status.success(), "{:?}", report.output);
        assert!(report.output.stderr.is_empty(), "{:?}", report.output);
        let output = String::from_utf8(report.output.stdout.clone()).unwrap();
        assert!(
            output.contains(
                "phases=15 rpc=3 mask=preserved native-messages=ok active-messages=ENOTSUP trusted-setup-rpc=ok bootstrap-signals="
            ),
            "{output}"
        );
        assert!(output.contains("admission child pid="), "{output}");
        assert!(output.contains("phases=9 probes=2 rpc=2"), "{output}");
        assert_eq!(report.events.len(), 2, "actual parent/child acquisitions");
        assert_eq!(report.events[0][1], 1);
        assert_eq!(report.events[1][2], 1);
        assert_ne!(report.events[0][3], report.events[1][3]);
        assert_ne!(report.events[0][5], 0);
        assert_ne!(report.events[1][5], 0);
        assert_ne!(report.events[0][5], report.events[1][5]);
        println!(
            "alt_stack={use_alt_stack} actual_events={:?}\n{output}",
            report.events
        );
        reports.push(report);
    }
    hardware_method_result(
        "initial_and_child_config_callbacks_keep_private_sockets_protected",
        &reports,
        0,
        0,
    );
}
