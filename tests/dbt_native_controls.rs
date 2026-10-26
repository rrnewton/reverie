// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

//! Real caller-owned Tool controls; missing clients are errors, never skips.

#[path = "tool_fixtures/native_controls.rs"]
mod tools;

use std::sync::atomic::Ordering;

use reverie::ExitStatus;
use reverie::Rdtsc;
use reverie_test_support::dbt::DbtTestTool;
use reverie_test_support::dbt::test_cmd;
use reverie_test_support::dbt::test_cmd_with_config;
use tools::ControlGlobal;
use tools::ControlTool;
use tools::InstructionErrorMode;
use tools::PROBE_LEAF;
use tools::PROBE_REPLY;
use tools::PROBE_SUBLEAF;
use tools::Request;
use tools::ThreadState;

impl DbtTestTool for ControlTool<true> {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_SUBSCRIBED_CONTROL_CLIENT";
}
impl DbtTestTool for ControlTool<false> {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_UNSUBSCRIBED_CONTROL_CLIENT";
}

fn guest() -> &'static str {
    env!("CARGO_BIN_EXE_dbt-native-control-guest")
}

fn lifecycle(global: &ControlGlobal, status: i32) -> (ThreadState, Vec<Request>) {
    let events = global.events.lock().unwrap().clone();
    assert_eq!(
        global.requests.load(Ordering::SeqCst) as usize,
        events.len()
    );
    assert!(
        events.len() >= 4,
        "both startup and both exit hooks must report"
    );
    let (
        root,
        Request::ThreadStart {
            state: initial,
            pid,
        },
    ) = &events[0]
    else {
        panic!("first request was not the actual thread-start callback");
    };
    assert_eq!(root, pid);
    assert!(*root > 0);
    assert_eq!(initial.initialized_tid, *root);
    assert_eq!(initial.injected_start_pid, i64::from(*root));
    assert_eq!(initial.thread_starts, 1);
    assert_eq!(initial.post_execs, 0);
    assert_eq!(initial.cookie, 0x42);
    let Request::PostExec { state: post } = &events[1].1 else {
        panic!("second request was not post-exec after thread-start");
    };
    assert_eq!(post.initialized_tid, *root);
    assert_eq!(post.thread_starts, 1);
    assert_eq!(post.post_execs, 1);
    assert_eq!(post.cookie, 0x52);
    assert!(events.iter().all(|(from, _)| from == root));
    let Request::ThreadExit {
        status: thread_status,
        state,
    } = &events[events.len() - 2].1
    else {
        panic!("thread exit must be immediately before process exit");
    };
    assert_eq!(*thread_status, status);
    assert_eq!(events.last().unwrap().1, Request::ProcessExit { status });
    assert_eq!(state.initialized_tid, *root);
    assert_eq!(state.injected_start_pid, i64::from(*root));
    assert_eq!(state.thread_starts, 1);
    assert_eq!(state.post_execs, 1);
    assert!(state.syscalls > 0);
    assert_eq!(
        state.cookie,
        0x52 + state.syscalls + state.cpuid_callbacks + state.rdtsc_callbacks
    );
    assert_eq!(
        state.ordinary_getpids as usize,
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::Getpid { .. }))
            .count()
    );
    assert_eq!(
        state.cpuid_callbacks as usize,
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::Cpuid { .. }))
            .count()
    );
    assert_eq!(
        state.rdtsc_callbacks as usize,
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::Rdtsc { .. }))
            .count()
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::ThreadStart { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::PostExec { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::ThreadExit { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, r)| matches!(r, Request::ProcessExit { .. }))
            .count(),
        1
    );
    let final_state = state.clone();
    (final_state, events.into_iter().map(|(_, r)| r).collect())
}

#[test]
fn ordinary_getpid_injection_returns_the_actual_guest_pid() {
    let (output, global) = test_cmd::<ControlTool<false>>(guest(), &["getpid"]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(output.stderr.is_empty());
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let pid: i64 = stdout
        .strip_prefix("pid=")
        .unwrap()
        .strip_suffix('\n')
        .unwrap()
        .parse()
        .unwrap();
    assert!(pid > 0);
    let (state, events) = lifecycle(&global, 0);
    assert_eq!(state.injected_start_pid, pid);
    assert!(state.ordinary_getpids > 0);
    let observed: Vec<_> = events
        .iter()
        .filter_map(|request| match request {
            Request::Getpid { injected, pid } => Some((*injected, i64::from(*pid))),
            _ => None,
        })
        .collect();
    assert!(!observed.is_empty());
    assert!(observed.iter().all(|pair| *pair == (pid, pid)));
}

#[test]
fn native_exit42_preserves_state_rpc_and_thread_then_process_order() {
    let (output, global) = test_cmd::<ControlTool<false>>(guest(), &["exit42"]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(42));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let (state, _) = lifecycle(&global, 42);
    assert!(
        state.ordinary_getpids > 0,
        "real callback state must survive to the consuming exit hook"
    );
    assert_eq!(state.cpuid_callbacks, 0);
    assert_eq!(state.rdtsc_callbacks, 0);
}

fn physical_cpuid() -> [u32; 4] {
    // Leaf zero is the physical max-leaf/vendor answer; do not assume a
    // particular answer for the intentionally out-of-range custom probe.
    let value = std::arch::x86_64::__cpuid_count(0, 0);
    [value.eax, value.ebx, value.ecx, value.edx]
}

fn physical_tsc() -> u64 {
    // SAFETY: this x86_64 fixture uses the architecture's SSE2 and TSC reads.
    unsafe {
        std::arch::x86_64::_mm_lfence();
        let value = std::arch::x86_64::_rdtsc();
        std::arch::x86_64::_mm_lfence();
        value
    }
}

fn instruction_output(stdout: &[u8]) -> ([u32; 4], [u32; 4], [u64; 2]) {
    let text = std::str::from_utf8(stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(text.ends_with('\n'));
    let physical: Vec<u32> = lines[0]
        .strip_prefix("physical=")
        .unwrap()
        .split(',')
        .map(|word| u32::from_str_radix(word, 16).unwrap())
        .collect();
    let cpuid: Vec<u32> = lines[1]
        .strip_prefix("cpuid=")
        .unwrap()
        .split(',')
        .map(|word| u32::from_str_radix(word, 16).unwrap())
        .collect();
    let tsc: Vec<u64> = lines[2]
        .strip_prefix("tsc=")
        .unwrap()
        .split(',')
        .map(|word| word.parse().unwrap())
        .collect();
    (
        physical.try_into().unwrap(),
        cpuid.try_into().unwrap(),
        tsc.try_into().unwrap(),
    )
}

fn physical_bracket(before: u64, observed: [u64; 2], after: u64) {
    // Independent controller reads establish a broad physical-time interval,
    // not a predicted value, maximum duration or determinism comparator.
    assert!(
        before < after,
        "host TSC did not advance across the command"
    );
    assert!(
        before <= observed[0],
        "guest TSC precedes the physical admission sample"
    );
    assert!(
        observed[0] < observed[1],
        "ordered physical reads must advance"
    );
    assert!(
        observed[1] <= after,
        "guest TSC exceeds the physical completion sample"
    );
}

#[test]
fn subscribed_instructions_reach_the_tool_and_return_its_recorded_results() {
    let expected_physical = physical_cpuid();
    let before = physical_tsc();
    let (output, global) = test_cmd::<ControlTool<true>>(guest(), &["instructions"]).unwrap();
    let after = physical_tsc();
    assert_eq!(
        physical_cpuid(),
        expected_physical,
        "physical leaf zero changed during the control"
    );
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(output.stderr.is_empty());
    let (physical, cpuid, tsc) = instruction_output(&output.stdout);
    assert_eq!(physical, expected_physical);
    assert_eq!(cpuid, PROBE_REPLY);
    physical_bracket(before, tsc, after);
    let (state, events) = lifecycle(&global, 0);
    let probe: Vec<_> = events
        .iter()
        .filter_map(|request| match request {
            Request::Cpuid {
                leaf: PROBE_LEAF,
                subleaf: PROBE_SUBLEAF,
                value,
            } => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(probe, [PROBE_REPLY]);
    let returned_tsc: Vec<_> = events
        .iter()
        .filter_map(|request| match request {
            Request::Rdtsc {
                request: Rdtsc::Tsc,
                value,
            } => {
                assert_eq!(value.aux, None);
                Some(value.tsc)
            }
            _ => None,
        })
        .collect();
    assert!(
        returned_tsc.windows(2).any(|values| values == tsc),
        "guest register results did not match two consecutive actual Tool replies"
    );
    assert!(state.cpuid_callbacks > 0);
    assert!(state.rdtsc_callbacks >= 2);
}

#[test]
fn unsubscribed_instructions_keep_physical_results_without_tool_callbacks() {
    let expected_cpuid = physical_cpuid();
    let before = physical_tsc();
    let (output, global) = test_cmd::<ControlTool<false>>(guest(), &["instructions"]).unwrap();
    let after = physical_tsc();
    assert_eq!(
        physical_cpuid(),
        expected_cpuid,
        "physical probe leaf changed during the control"
    );
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(output.stderr.is_empty());
    let (physical, cpuid, tsc) = instruction_output(&output.stdout);
    assert_eq!(physical, expected_cpuid);
    assert_ne!(cpuid, PROBE_REPLY);
    physical_bracket(before, tsc, after);
    let (state, events) = lifecycle(&global, 0);
    assert_eq!(state.cpuid_callbacks, 0);
    assert_eq!(state.rdtsc_callbacks, 0);
    assert!(
        events
            .iter()
            .all(|request| !matches!(request, Request::Cpuid { .. } | Request::Rdtsc { .. }))
    );
}

fn instruction_hook_error(mode: InstructionErrorMode, argument: &str, errno: reverie::Errno) {
    let (output, global) =
        test_cmd_with_config::<ControlTool<true>>(guest(), &[argument], mode).unwrap();
    assert!(
        matches!(output.status, ExitStatus::Exited(code) if code != 0),
        "instruction error must take the native fatal path, not signal/panic/timeout: {:?}",
        output.status
    );
    // Exact output requires proof that main reached the selected probe, and
    // rejects continuing past that instruction even if a later failure occurs.
    assert_eq!(output.stdout, format!("before-{argument}\n").as_bytes());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = format!("reverie-dbt: caller Tool refused: {errno}");
    assert!(stderr.contains(&reason), "missing {reason:?}: {stderr}");
    assert!(!stderr.contains("panicked at"), "{stderr}");
    eprintln!(
        "expected subscribed {mode:?} hook error: status={:?}, stderr={stderr}",
        output.status
    );

    let events = global.events.lock().unwrap();
    assert_eq!(
        global.requests.load(Ordering::SeqCst) as usize,
        events.len()
    );
    let Some((
        root,
        Request::ThreadStart {
            state: initial,
            pid,
        },
    )) = events.first()
    else {
        panic!("hook failure did not follow the real thread-start callback");
    };
    assert_eq!(root, pid);
    assert!(*root > 0);
    assert_eq!(initial.initialized_tid, *root);
    assert_eq!(initial.injected_start_pid, i64::from(*root));
    assert_eq!(initial.thread_starts, 1);
    assert_eq!(initial.post_execs, 0);
    let Some((from, Request::PostExec { state: post })) = events.get(1) else {
        panic!("hook failure did not follow the real post-exec callback");
    };
    assert_eq!(from, root);
    assert_eq!(post.thread_starts, 1);
    assert_eq!(post.post_execs, 1);
    assert!(events.iter().all(|(from, _)| from == root));
    let errors: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, (_, request))| match request {
            Request::InstructionError { mode, errno, state } => Some((index, *mode, *errno, state)),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one actual failing hook RPC"
    );
    let (error_index, actual_mode, actual_errno, state) = errors[0];
    assert!(error_index > 1);
    assert_eq!(actual_mode, mode);
    assert_eq!(actual_errno, errno.into_raw());
    assert_eq!(state.initialized_tid, *root);
    assert_eq!(state.thread_starts, 1);
    assert_eq!(state.post_execs, 1);
    assert_eq!(
        state.cookie,
        0x52 + state.syscalls + state.cpuid_callbacks + state.rdtsc_callbacks
    );
    if mode == InstructionErrorMode::Cpuid {
        assert!(state.cpuid_callbacks > 0);
        assert!(!state.rdtsc_error_armed);
    } else {
        assert!(state.rdtsc_error_armed);
        assert!(state.rdtsc_callbacks > 0);
        let probes: Vec<_> = events[..error_index]
            .iter()
            .filter_map(|(_, request)| match request {
                Request::Cpuid {
                    leaf: PROBE_LEAF,
                    subleaf: PROBE_SUBLEAF,
                    value,
                } => Some(*value),
                _ => None,
            })
            .collect();
        assert_eq!(
            probes,
            [PROBE_REPLY],
            "RDTSC error was not armed by the guest probe"
        );
    }
}

#[test]
fn subscribed_cpuid_error_refuses_before_the_guest_continues() {
    instruction_hook_error(
        InstructionErrorMode::Cpuid,
        "cpuid-error",
        reverie::Errno::EINVAL,
    );
}

#[test]
fn subscribed_rdtsc_error_refuses_before_the_guest_continues() {
    instruction_hook_error(
        InstructionErrorMode::Rdtsc,
        "rdtsc-error",
        reverie::Errno::ERANGE,
    );
}
