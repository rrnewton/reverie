/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */
use std::process::Command;
use std::time::Duration;

#[path = "common/supervisor.rs"]
mod supervised;

fn hardware_method_result(
    method: &str,
    report: &supervised::Report,
    native_rows: usize,
    mediated_rows: usize,
) -> String {
    use std::collections::BTreeMap;

    let expected_modes: &[&str] = match method {
        "supervisor_counter_fallback_on_alt_stack"
        | "supervisor_counter_fallback_without_alt_stack" => &["rcb-acquisition-fallback"],
        "supervisor_counter_installed_on_alt_stack"
        | "supervisor_counter_installed_without_alt_stack" => &["rcb-acquisition-installed"],
        "private_counter_scm_rights_transport_is_refused_before_alias" => &[
            "rcb-private-scm-rights",
            "rcb-private-scm-rights-child",
        ],
        "private_counter_pidfd_getfd_transport_is_refused_before_alias" =>
            &["rcb-private-pidfd-getfd"],
        "private_counter_io_uring_transport_and_inherited_sqpoll_are_refused" =>
            &["rcb-private-io-uring"],
        _ => panic!("unselected hardware method {method}"),
    };
    assert!(
        report.counter_unavailable.is_empty(),
        "{method}: unavailable counter observations: {:?}",
        report.counter_unavailable
    );
    assert!(
        !report.events.is_empty(),
        "{method}: zero authenticated acquisitions"
    );
    assert_eq!(
        report.events.len(),
        report.counter_results.len(),
        "{method}: every acquisition needs its own actual counter result"
    );
    let mut events = BTreeMap::new();
    for event in &report.events {
        assert_eq!(event[0], 1, "{method}: acquisition version");
        assert_ne!(event[5], 0, "{method}: event identity");
        assert_eq!(event[6], 4, "{method}: PERF_TYPE_RAW profile");
        assert!(
            matches!(event[7], 0x5101c4 | 0x5100d1),
            "{method}: retired-conditional-branch profile"
        );
        assert!(event[8] <= i32::MAX as u64, "{method}: target CPU");
        assert_eq!(event[9], 0, "{method}: event was acquired disabled");
        assert!(
            events.insert(event[5], *event).is_none(),
            "{method}: duplicate event identity"
        );
    }
    let mut results = BTreeMap::new();
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
    assert!(!events.is_empty(), "{method}: zero authenticated acquisitions");
    assert_eq!(events.len(), results.len(), "{method}: acquisition/result join");
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
        let result = results.get(event_id).expect("counter result for event");
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
    format!(
        "liteinst hardware method: phase=run-counter binary=rcb_acquisition method={method} events={} results={} event-ids={} result-event-ids={} owners={} result-owners={} event-types={} configs={} cpus={} result-cpus={} initially-disabled={} fds={} counters={} modes={} native-rows={native_rows} mediated-rows={mediated_rows}",
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
    )
}

fn run(installed: bool, use_alt_stack: bool, method: &str) -> String {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let kind = if installed { "installed" } else { "fallback" };
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
    command
        .arg(format!("rcb-acquisition-{kind}"))
        .arg(&socket)
        .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
    reverie_liteinst::set_guest_alt_stack(&mut command, use_alt_stack);
    let report = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
    assert!(report.output.status.success(), "{:?}", report.output);
    assert!(report.output.stderr.is_empty(), "{:?}", report.output);
    // The observer precedes EVENT/ACK; the completed setup/service report,
    // successful required clock reads and post-acquisition RPC also matter.
    assert_eq!(report.events.len(), 1, "one actual supervisor acquisition");
    let event = report.events[0];
    assert_eq!(event[0], 1);
    assert_eq!(event[1], 1);
    assert_eq!(event[2], 0);
    assert!(event[3] > 0 && event[4] > 0);
    assert_ne!(event[5], 0);
    assert_eq!(event[9], 0, "event was acquired physically disabled");
    let output = String::from_utf8(report.output.stdout.clone()).unwrap();
    let count = |prefix: &str| {
        output
            .lines()
            .filter(|line| line.starts_with(prefix))
            .count()
    };
    assert_eq!(count("rcb native "), 8, "{output}");
    assert_eq!(count("rcb mediated "), 8, "{output}");
    assert_eq!(count("rcb vector "), 2, "{output}");
    assert_eq!(count("rcb restorer: "), 1, "{output}");
    assert_eq!(count("rcb warmup: "), 1, "{output}");
    assert_eq!(count("rcb callback-table "), 1, "{output}");
    assert!(count("rcb callback ") > 0, "{output}");
    assert_eq!(count("rcb callback-boundary "), 1, "{output}");
    assert_eq!(count("rcb trampoline "), 1, "{output}");
    for variant in 0..2 {
        assert!(
            output.lines().any(|line| line == format!(
                "rcb vector variant={variant} private=[0, 0, 4096, 4096] ordinary=[0, 4096, 0, 4096]"
            )),
            "{output}"
        );
    }
    let summary = output.lines().last().expect("complete matrix summary");
    assert_eq!(count("rcb matrix: "), 1, "{output}");
    let fields: Vec<_> = summary.split_whitespace().collect();
    assert_eq!(fields.len(), 11, "{summary}");
    assert_eq!(&fields[..2], &["rcb", "matrix:"]);
    assert_eq!(fields[2], format!("path={kind}"));
    assert_eq!(fields[3], "native=8");
    assert_eq!(fields[4], "mediated=8");
    assert_eq!(fields[5], format!("actual-pid={}", event[4]));
    let ordinary_id: u64 = fields[6]
        .strip_prefix("ordinary-id=")
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(ordinary_id, 0);
    assert_ne!(
        ordinary_id, event[5],
        "ordinary and runtime events must differ"
    );
    assert_eq!(fields[7], if installed { "hooks=21" } else { "hooks=0" });
    assert_eq!(fields[8], if installed { "traps=1" } else { "traps=17" });
    assert_eq!(fields[9], "callback-rpc=1");
    let cpu = fields[10]
        .strip_prefix("cpu=")
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert_eq!(event[8], u64::from(cpu), "supervisor used the target's bound CPU");
    println!("alt_stack={use_alt_stack} acquisition={event:?}\n{output}");
    hardware_method_result(method, &report, 8, 8)
}

// Separate processes preserve the original acquisition vectors and warmup.
// Zero, 4096 and fork retain their original absolute first-callback controls.
// The additive signal process queues one handler with exactly 64 branches;
// the reconstructed child's fresh first callback remains 4097. Root
// thread-start itself remains exact zero in every process.
fn run_initial(installed: bool, use_alt_stack: bool) {
    let kind = if installed { "installed" } else { "fallback" };
    for probe in ["zero", "4096", "fork", "signal"] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
        command
            .arg(format!("rcb-initial-{probe}-{kind}"))
            .arg(&socket)
            .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
        reverie_liteinst::set_guest_alt_stack(&mut command, use_alt_stack);
        let report = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
        assert!(report.output.status.success(), "{:?}", report.output);
        assert!(report.output.stderr.is_empty(), "{:?}", report.output);
        let fork = probe == "fork";
        assert_eq!(report.events.len(), if fork { 2 } else { 1 });
        let root = report.events[0];
        assert_eq!(&root[..3], &[1, 1, 0]);
        assert!(root[3] > 0 && root[4] > 0);
        assert_ne!(root[5], 0);
        assert_eq!(root[9], 0, "root event was acquired physically disabled");
        let child_pid = if fork {
            let child = report.events[1];
            assert_eq!(&child[..3], &[1, 2, 1], "actual child acquisition");
            assert!(child[3] > 0 && child[4] > 0);
            assert_ne!(child[3], root[3], "distinct host process");
            assert_ne!(child[4], root[4], "distinct process-local PID");
            assert_ne!(child[5], 0);
            assert_ne!(child[5], root[5], "a fresh supervisor event for the child");
            assert_eq!(&child[6..8], &root[6..8]);
            assert_eq!(child[8], root[8], "fork inherited the bound CPU");
            assert_eq!(child[9], 0, "child event was acquired physically disabled");
            child[4]
        } else {
            0
        };
        let output = String::from_utf8(report.output.stdout).unwrap();
        let count = |prefix: &str| output.lines().filter(|line| line.starts_with(prefix)).count();
        assert_eq!(count("rcb initial start: "), if fork { 2 } else { 1 }, "{output}");
        assert_eq!(count("rcb initial post-exec: "), 1, "{output}");
        assert_eq!(count("rcb initial callback: "), if fork { 2 } else { 1 }, "{output}");
        assert_eq!(count("rcb initial: "), 1, "{output}");
        assert_eq!(output.lines().count(), if fork { 12 } else { 10 }, "{output}");
        let require = |line: String| {
            assert_eq!(output.lines().filter(|actual| *actual == line).count(), 1, "{line}\n{output}");
        };
        let pid = root[4];
        for name in [
            "root-activation",
            "root-constructor",
            "root-signal-handler",
            "entry-window-signal-handler",
            "instruction-signal-entry",
            "root-signal-restorer",
        ] {
            let prefix = format!("rcb {name} ");
            assert_eq!(count(&prefix), 1, "{output}");
            let line = output.lines().find(|line| line.starts_with(&prefix)).unwrap();
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 6, "{line}");
            assert_eq!(fields[2], format!("pid={pid}"));
            let address = u64::from_str_radix(fields[3].strip_prefix("address=0x").unwrap(), 16).unwrap();
            assert_ne!(address, 0);
            let bytes: usize = fields[4].strip_prefix("bytes=").unwrap().parse().unwrap();
            assert!((1..=16384).contains(&bytes));
            let code = fields[5].strip_prefix("code=").unwrap();
            assert_eq!(code.len(), 2 * bytes, "complete loaded symbol bytes");
            assert!(code.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        require(format!(
            "rcb initial start: role=root pid={pid} clock=0 post-setup=4096 owned-drop=4096 timer-request-rcbs=8192 rpc=1 senders=1"
        ));
        require(format!("rcb initial post-exec: role=root pid={pid} clock=0"));
        let first = match probe {
            "4096" => 4096,
            "signal" => 64,
            "zero" | "fork" => 0,
            _ => unreachable!(),
        };
        let root_syscall = if fork { 57 } else { 39 };
        let root_hooks = u64::from(installed);
        require(format!(
            "rcb initial callback: role=root pid={pid} ordinal=0 clock={first} syscall={root_syscall} timer-request-rcbs=8192 hooks={root_hooks} traps=1"
        ));
        if fork {
            require(format!(
                "rcb initial start: role=child pid={child_pid} clock=0 post-setup=4096 owned-drop=4096 timer-request-rcbs=8192 rpc=2 senders=2 inherited-perf-map=absent mincore=ENOMEM access=EFAULT maps=absent"
            ));
            let (hooks, traps) = if installed { (2, 0) } else { (0, 2) };
            require(format!(
                "rcb initial callback: role=child pid={child_pid} ordinal=0 clock=4097 syscall=39 timer-request-rcbs=8192 hooks={hooks} traps={traps}"
            ));
        }
        let summary = output.lines().last().unwrap();
        let fields: Vec<_> = summary.split_whitespace().collect();
        assert_eq!(fields.len(), 22, "{summary}");
        assert_eq!(&fields[..2], &["rcb", "initial:"]);
        assert_eq!(fields[2], format!("path={kind}"));
        assert_eq!(fields[3], format!("probe={probe}"));
        assert_eq!(fields[4], format!("actual-pid={pid}"));
        assert_eq!(fields[5], format!("first={first}"));
        assert_eq!(fields[6], "setup=4096");
        assert_eq!(fields[7], "drop=4096");
        assert_eq!(fields[8], "unselected-mask=exact");
        assert_eq!(
            fields[9],
            if probe == "signal" {
                "root-entry-signal=1"
            } else {
                "root-entry-signal=0"
            }
        );
        assert_eq!(fields[10], if probe == "signal" { "signal=64" } else { "signal=0" });
        assert_eq!(
            fields[11],
            if probe == "signal" {
                "signal-mode=1"
            } else {
                "signal-mode=unobserved"
            }
        );
        assert_eq!(
            fields[12],
            if probe == "signal" {
                "signal-held=0"
            } else {
                "signal-held=unobserved"
            }
        );
        assert_eq!(
            fields[13],
            if probe == "signal" {
                if use_alt_stack {
                    "signal-stack=alt"
                } else {
                    "signal-stack=ordinary"
                }
            } else {
                "signal-stack=unobserved"
            },
        );
        if probe == "signal" {
            let stack_pointer = u64::from_str_radix(
                fields[14].strip_prefix("signal-sp=0x").unwrap(),
                16,
            )
            .unwrap();
            assert_ne!(stack_pointer, 0);
            if use_alt_stack {
                let range = fields[15].strip_prefix("signal-alt=0x").unwrap();
                let (start, end) = range.split_once("-0x").unwrap();
                let start = u64::from_str_radix(start, 16).unwrap();
                let end = u64::from_str_radix(end, 16).unwrap();
                assert!(start <= stack_pointer && stack_pointer < end);
            } else {
                assert_eq!(fields[15], "signal-alt=disabled");
            }
        } else {
            assert_eq!(fields[14], "signal-sp=unobserved");
            assert_eq!(fields[15], "signal-alt=unobserved");
        }
        assert_eq!(
            fields[16],
            if probe == "signal" {
                "callback-edge-signals=2"
            } else {
                "callback-edge-signals=0"
            }
        );
        assert_eq!(
            fields[17],
            if probe == "signal" {
                "pre-mask-signal=held-delivered-paused"
            } else {
                "pre-mask-signal=unobserved"
            }
        );
        assert_eq!(
            fields[18],
            if fork {
                "fork-parent-event=continuous"
            } else {
                "fork-parent-event=unobserved"
            }
        );
        assert_eq!(fields[19], "callbacks=1");
        let cpu = fields[20]
            .strip_prefix("cpu=")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(root[8], u64::from(cpu), "root acquisition CPU attestation");
        assert_eq!(fields[21], format!("child-pid={child_pid}"));
        println!("alt_stack={use_alt_stack} initial acquisitions={:?}\n{output}", report.events);
    }
}

fn run_root_profile_refusals() {
    for (fault, expected_events) in [("unsupported", 0), ("wrong-event", 1)] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
        command
            .arg("rcb-profile-refusal")
            .arg(&socket)
            .env("REVERIE_LITEINST_TEST_RCB_PROFILE_FAULT", fault)
            .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
        let report =
            supervised::run_profile_refusal(command, Duration::from_secs(20), Some(1024 * 1024));
        assert_eq!(report.output.status.code(), Some(126), "{:?}", report.output);
        assert_eq!(report.output.stdout, b"root-profile-refusal=exact\n");
        let stderr = String::from_utf8(report.output.stderr).unwrap();
        assert!(
            stderr.contains("reverie-liteinst: root activation failed"),
            "{stderr}"
        );
        assert_eq!(report.events.len(), expected_events, "fault={fault}");
        if fault == "wrong-event" {
            let event = report.events[0];
            assert_ne!(event[5], 0, "real event ID before corrupt offer");
        }
    }
}

#[test]
fn supervisor_counter_fallback_on_alt_stack() {
    let hardware = run(
        false,
        true,
        "supervisor_counter_fallback_on_alt_stack",
    );
    run_initial(false, true);
    // One existing selected test owns both root-side negative controls; the
    // selected/required test population remains unchanged.
    run_root_profile_refusals();
    println!("{hardware}");
}

#[test]
fn supervisor_counter_fallback_without_alt_stack() {
    let hardware = run(
        false,
        false,
        "supervisor_counter_fallback_without_alt_stack",
    );
    run_initial(false, false);
    println!("{hardware}");
}

#[test]
fn supervisor_counter_installed_on_alt_stack() {
    let hardware = run(
        true,
        true,
        "supervisor_counter_installed_on_alt_stack",
    );
    run_initial(true, true);
    println!("{hardware}");
}

#[test]
fn supervisor_counter_installed_without_alt_stack() {
    let hardware = run(
        true,
        false,
        "supervisor_counter_installed_without_alt_stack",
    );
    run_initial(true, false);
    println!("{hardware}");
}

fn marker_value(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(key))
        .unwrap_or_else(|| panic!("missing {key} in {line}"))
        .parse()
        .unwrap()
}

fn run_private_transport(
    mode: &str,
    sacrificial_mode: &str,
    transport: &str,
    expected_events: usize,
    method: &str,
) -> String {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("sacrificial-coordinator.sock");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
    command
        .arg(sacrificial_mode)
        .arg(&socket)
        .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
    let sacrificial = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
    assert!(sacrificial.output.status.success(), "{:?}", sacrificial.output);
    assert!(sacrificial.output.stderr.is_empty(), "{:?}", sacrificial.output);
    assert_eq!(sacrificial.events.len(), 1, "one isolated sacrificial event");
    let sacrificial_line = String::from_utf8(sacrificial.output.stdout).unwrap();
    let sacrificial_line = sacrificial_line.trim_end();
    assert!(
        sacrificial_line.starts_with(&format!("private sacrificial={transport}")),
        "{sacrificial_line}"
    );
    assert!(sacrificial_line.ends_with("destructive-effect=confirmed"));
    assert_eq!(
        marker_value(sacrificial_line, "event-id="),
        sacrificial.events[0][5],
        "sacrificial marker joins the actual supervisor event"
    );
    assert_eq!(
        marker_value(sacrificial_line, "owner="),
        sacrificial.events[0][4],
        "sacrificial owner joins the authenticated event target"
    );

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("coordinator.sock");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
    command
        .arg(mode)
        .arg(&socket)
        .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
    let report = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
    assert!(report.output.status.success(), "{:?}", report.output);
    assert!(report.output.stderr.is_empty(), "{:?}", report.output);
    assert_eq!(report.events.len(), expected_events, "actual supervisor events");
    for (index, event) in report.events.iter().enumerate() {
        assert_eq!(event[0], 1);
        assert_eq!(event[1], index as u64 + 1);
        assert_eq!(event[2], index as u64);
        assert!(event[3] > 0 && event[4] > 0);
        assert_ne!(event[5], 0, "actual supervisor perf event id");
        assert!(event[8] <= i32::MAX as u64, "explicit target CPU");
        assert_eq!(event[9], 0, "event was acquired physically disabled");
    }
    if expected_events == 2 {
        assert_ne!(report.events[0][3], report.events[1][3]);
        assert_ne!(report.events[0][4], report.events[1][4]);
        assert_ne!(report.events[0][5], report.events[1][5]);
    }
    let output = String::from_utf8(report.output.stdout.clone()).unwrap();
    let marker = output.lines().next().unwrap();
    assert!(
        marker.starts_with(&format!("private transport={transport}")),
        "{marker}"
    );
    assert!(marker.contains("native-control=ok guarded=ENOTSUP"), "{marker}");
    assert_eq!(marker_value(marker, "event-id="), report.events[0][5]);
    assert_eq!(marker_value(marker, "owner="), report.events[0][4]);
    assert_eq!(marker_value(marker, "clock-delta="), 4096);
    let before = marker_value(marker, "clock-before=");
    let after = marker_value(marker, "clock-after=");
    assert!(after >= before.checked_add(4096).unwrap());
    if transport == "scm-rights" {
        assert!(marker.contains("prepared-generation=2"), "{marker}");
        assert_eq!(report.events[0][1], 1, "parent generation");
        assert_eq!(report.events[0][2], 0, "parent has no parent generation");
        assert_eq!(report.events[1][1], 2, "exactly one child generation");
        assert_eq!(report.events[1][2], 1, "child belongs to parent generation");
    }
    assert_eq!(output.lines().count(), 1, "{output}");
    hardware_method_result(method, &report, 0, 0)
}

#[test]
fn private_counter_scm_rights_transport_is_refused_before_alias() {
    let hardware = run_private_transport(
        "rcb-private-scm-rights",
        "rcb-private-scm-rights-sacrificial",
        "scm-rights",
        2,
        "private_counter_scm_rights_transport_is_refused_before_alias",
    );
    println!("{hardware}");
}

#[test]
fn private_counter_pidfd_getfd_transport_is_refused_before_alias() {
    let hardware = run_private_transport(
        "rcb-private-pidfd-getfd",
        "rcb-private-pidfd-getfd-sacrificial",
        "pidfd_getfd",
        1,
        "private_counter_pidfd_getfd_transport_is_refused_before_alias",
    );
    println!("{hardware}");
}

#[test]
fn private_counter_io_uring_transport_and_inherited_sqpoll_are_refused() {
    let hardware = run_private_transport(
        "rcb-private-io-uring",
        "rcb-private-io-uring-sacrificial",
        "io_uring",
        1,
        "private_counter_io_uring_transport_and_inherited_sqpoll_are_refused",
    );

    // This second, isolated native process creates a real SQPOLL ring before
    // installation. The root inventory must observe it before connecting to a
    // supervisor or importing an event, retain ENOTSUP, and fail the root
    // activation instead of returning to guest code.
    let directory = tempfile::tempdir().unwrap();
    let absent_socket = directory.path().join("must-not-connect.sock");
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-rpc-tool-guest"));
    command
        .arg("rcb-inherited-sqpoll")
        .arg(&absent_socket)
        .env_remove(reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV);
    let report = supervised::run(command, Duration::from_secs(20), Some(1024 * 1024));
    assert_eq!(report.output.status.code(), Some(126), "{:?}", report.output);
    assert!(report.output.stdout.is_empty(), "{:?}", report.output);
    assert_eq!(
        report.output.stderr,
        b"reverie-liteinst: root activation failed\n",
        "the pre-activation refusal must reach the fail-closed root boundary"
    );
    assert!(report.events.is_empty(), "no supervisor event may precede inventory");
    println!("{hardware}");
}
