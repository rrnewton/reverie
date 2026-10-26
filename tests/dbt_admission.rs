// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

#[path = "tool_fixtures/basics.rs"]
#[allow(dead_code)] // Noop is shared unchanged; Counter is selected by dbt-basics.
mod basics;
#[path = "tool_fixtures/dbt_admission.rs"]
mod tools;
use reverie::ExitStatus;
use reverie_test_support::dbt::DbtTestTool;
use reverie_test_support::dbt::test_cmd;
use reverie_test_support::dbt::test_cmd_with_config;
use tools::AdmissionTool;
use tools::Mode;
impl DbtTestTool for basics::NoopTool {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_NOOP_TEST_CLIENT";
    const ZERO_APPLICATION_RPC: bool = true;
}
impl DbtTestTool for AdmissionTool {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_ADMISSION_TEST_CLIENT";
}
const GUEST: &str = env!("CARGO_BIN_EXE_dbt-admission-guest");
const TRANSITIONS: [(&str, i64); 6] = [
    ("fork", libc::SYS_fork),
    ("vfork", libc::SYS_vfork),
    ("clone", libc::SYS_clone),
    ("clone3", libc::SYS_clone3),
    ("execve", libc::SYS_execve),
    ("execveat", libc::SYS_execveat),
];

fn refused(output: &reverie::process::Output, reason: &str) {
    assert_ne!(output.status, ExitStatus::Exited(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!(
        "expected refusal {reason:?}: status={:?}, stderr={stderr}",
        output.status
    );
    assert!(
        stderr.contains(reason),
        "missing refusal {reason:?}; status={:?}, stderr={stderr}",
        output.status
    );
}
#[test]
fn original_birth_and_exec_are_named_refusals() {
    for (name, _) in TRANSITIONS {
        let (output, _) = test_cmd::<basics::NoopTool>(GUEST, &[name]).unwrap();
        refused(
            &output,
            &format!("initial-root Tool domain refuses physical {name}"),
        );
    }
}
#[test]
fn injected_birth_and_exec_remain_fatal_when_errno_is_swallowed() {
    for (name, number) in TRANSITIONS {
        let (output, _) =
            test_cmd_with_config::<AdmissionTool>(GUEST, &["getpid"], Mode::Inject(number))
                .unwrap();
        refused(
            &output,
            &format!("initial-root Tool domain refuses physical {name}"),
        );
    }
}
#[test]
fn deferred_birth_and_exec_are_named_refusals() {
    for (name, number) in TRANSITIONS {
        let (output, _) =
            test_cmd_with_config::<AdmissionTool>(GUEST, &["getpid"], Mode::Tail(number)).unwrap();
        refused(
            &output,
            &format!("initial-root Tool domain refuses physical {name}"),
        );
    }
}
#[test]
fn delivered_sigusr1_is_a_named_caller_mode_refusal() {
    let (output, _) = test_cmd::<basics::NoopTool>(GUEST, &["sigusr1"]).unwrap();
    refused(&output, "initial-root Tool domain refuses delivered signal");
}
#[test]
fn unretired_sigkill_is_a_launcher_failure() {
    let error = test_cmd::<basics::NoopTool>(GUEST, &["sigkill"])
        .expect_err("SIGKILL must not return ordinary output");
    eprintln!("expected unretired failure: {error}");
    assert!(
        error.to_string().contains("caller Tool root not retired"),
        "{error}"
    );
}
#[test]
fn real_restart_reenters_the_tool_and_preserves_its_state() {
    let (output, global) =
        test_cmd_with_config::<AdmissionTool>(GUEST, &["getpid"], Mode::Restart).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap()
            > 0
    );
    assert_eq!(*global.0.lock().unwrap(), [1, 2]);
}
#[test]
fn non_errno_tool_error_is_named_fatal() {
    let (output, _) =
        test_cmd_with_config::<AdmissionTool>(GUEST, &["getpid"], Mode::Error).unwrap();
    refused(&output, "fixture non-errno Tool error");
}
#[test]
fn failed_startup_cannot_declare_initial_root_ready() {
    for mode in [Mode::FailStart, Mode::FailPostExec] {
        let error = test_cmd_with_config::<AdmissionTool>(GUEST, &["getpid"], mode)
            .err()
            .expect("failed startup must not be accepted");
        eprintln!("expected {mode:?} failure: {error}");
        assert!(error.to_string().contains("initial-root"), "{error}");
    }
}

#[test]
fn missing_coordinator_refuses_the_actual_caller_runtime() {
    let client = std::env::var_os("REVERIE_DBT_NOOP_TEST_CLIENT").unwrap();
    let runner = reverie_dbt::DbtRunner::new(reverie_dbt::bundled_drrun_path(), client).unwrap();
    let output = runner
        .output(&std::process::Command::new("/bin/true"))
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("expected standalone refusal: {stderr}");
    assert!(
        stderr.contains("caller Tool requires coordinated native startup"),
        "{stderr}"
    );
}

fn primary_survives(deferred: bool, operation: &str) {
    let (output, global) =
        test_cmd_with_config::<AdmissionTool>(GUEST, &[operation], Mode::ProtectFds { deferred })
            .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"primary-survived\n");
    let expected = if deferred && operation != "protect-dup2" {
        vec![1, 3, 2]
    } else {
        vec![1, 2]
    };
    assert_eq!(
        *global.0.lock().unwrap(),
        expected,
        "primary RPC failed, or a protected dup2 reached the Tool"
    );
}

#[test]
fn primary_rpc_survives_original_close_range() {
    primary_survives(false, "protect-close-range");
}

#[test]
fn primary_rpc_survives_deferred_close_range() {
    primary_survives(true, "protect-close-range");
}

#[test]
fn primary_rpc_survives_original_dup2() {
    primary_survives(false, "protect-dup2");
}

#[test]
fn primary_rpc_survives_deferred_dup2_without_tool_delivery() {
    primary_survives(true, "protect-dup2");
}

// These isolate the primary descriptor. They do not replace the full-range
// controls above, which also exercise unrelated native/SDK descriptors.
#[test]
fn primary_rpc_survives_original_single_fd_range() {
    primary_survives(false, "protect-primary-range");
}

#[test]
fn primary_rpc_survives_deferred_single_fd_range() {
    primary_survives(true, "protect-primary-range");
}
