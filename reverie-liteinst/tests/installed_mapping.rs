use std::process::Command;
use std::time::Duration;

#[path = "../../reverie-rpc-transport/tests/fixtures/owned_lifecycle.rs"]
mod owned_lifecycle;

fn installed(mode: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("LITEINST_INSTALLED_MAPPING_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    let directory = root.join(mode);
    std::fs::create_dir(&directory).unwrap();
    let mut command = Command::new(env!(
        "CARGO_BIN_EXE_reverie-liteinst-installed-mapping-fixture"
    ));
    command.arg("host").arg(&directory).arg(mode);
    let (output, _) = owned_lifecycle::run(
        command,
        Duration::from_secs(if mode == "io-uring" { 10 } else { 40 }),
    )
    .unwrap();
    std::fs::write(directory.join("host.stdout"), &output.stdout).unwrap();
    std::fs::write(directory.join("host.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{output:?}; {}",
        directory.display()
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    let count = if mode.ends_with("kernel-error") { 1 } else { 2 };
    assert_eq!(
        output.stdout,
        format!(
            "installed mapping complete: {mode}; processes={count}; statistics={}\n",
            mode != "statistics-off"
        )
        .as_bytes()
    );
}

#[test]
fn installed_raw_fork_owns_both_captures_and_clients() {
    installed("raw");
}
#[test]
fn installed_clone3_uses_the_real_fork_result() {
    installed("clone3");
}
#[test]
fn installed_kernel_failure_restores_both_parent_clients() {
    installed("kernel-error");
}
#[test]
fn installed_statistics_off_does_not_create_a_statistics_client() {
    installed("statistics-off");
}
#[test]
fn installed_nonzero_root_retains_failure_after_complete_capture() {
    installed("root-nonzero");
}

#[test]
fn installed_signal_root_cannot_claim_finish_or_graceful_rpc_close() {
    installed("root-signal");
}

#[test]
fn installed_private_destination_failure_preserves_public_and_native_fork() {
    installed("private-failure");
}
#[test]
fn installed_public_destination_failure_preserves_private_and_native_fork() {
    installed("public-failure");
}
#[test]
fn installed_failed_capture_does_not_replace_actual_kernel_error() {
    installed("private-failure-kernel-error");
}
#[test]
fn installed_second_capture_capacity_preserves_first_reservation_and_fork() {
    installed("private-capacity");
}
#[test]
fn installed_second_capture_capacity_preserves_first_cancelled_reservation() {
    installed("private-capacity-kernel-error");
}

#[test]
fn installed_io_uring_preserves_native_send_update_and_original_pointer_effects() {
    installed("io-uring");
}

fn setup(mode: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("LITEINST_INSTALLED_MAPPING_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    let directory = root.join(format!("setup-{mode}"));
    std::fs::create_dir(&directory).unwrap();
    let mut command = Command::new(env!(
        "CARGO_BIN_EXE_reverie-liteinst-installed-mapping-fixture"
    ));
    command.arg("setup-host").arg(&directory).arg(mode);
    let (output, _) = owned_lifecycle::run(command, Duration::from_secs(5)).unwrap();
    std::fs::write(directory.join("host.stdout"), &output.stdout).unwrap();
    std::fs::write(directory.join("host.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{output:?}; {}",
        directory.display()
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("installed setup complete: {mode}\n").as_bytes()
    );
}
#[test]
fn installed_setup_complete_configuration_closes_both_fresh_clients() {
    setup("complete");
}
#[test]
fn installed_setup_silent_main_uses_original_deadline() {
    setup("main-silent");
}
#[test]
fn installed_setup_silent_statistics_uses_original_deadline() {
    setup("statistics-silent");
}
#[test]
fn installed_setup_partial_header_uses_original_deadline() {
    setup("header-stall");
}
#[test]
fn installed_setup_partial_body_uses_original_deadline() {
    setup("body-stall");
}
#[test]
fn installed_setup_partial_header_eof_is_truncated() {
    setup("header-eof");
}
#[test]
fn installed_setup_partial_body_eof_is_truncated() {
    setup("body-eof");
}

#[test]
fn installed_setup_clean_eof_stays_distinct_from_truncation() {
    setup("clean-eof");
}
#[test]
fn installed_setup_decode_cannot_silently_exceed_deadline() {
    setup("slow-decode");
}
#[test]
fn installed_setup_clone_cannot_silently_exceed_deadline() {
    setup("slow-clone");
}

#[test]
fn installed_setup_failure_marks_both_captures_before_caller_exit() {
    setup("main-silent-alive");
}

#[test]
fn installed_setup_decode_panic_preserves_payload_and_retires_copied_producers() {
    setup("panic-decode-alive");
}

#[test]
fn installed_setup_clone_panic_preserves_payload_and_retires_copied_producers() {
    setup("panic-clone-alive");
}
