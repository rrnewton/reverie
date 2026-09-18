#![deny(warnings)]
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

fn run(mode: &str) {
    let child = Command::new(env!("CARGO_BIN_EXE_liteinst2-arena-fork-fixture"))
        .arg(mode)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let timed_out = loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        assert_eq!(rc, 0);
        if unsafe { info.si_pid() } != 0 {
            break false;
        }
        if Instant::now() >= deadline {
            break true;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // Keep the helper PID reserved until its entire owned group is stopped.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let output = child.wait_with_output().unwrap();
    if let Some(root) = std::env::var_os("LITEINST_ARENA_TEST_EVIDENCE") {
        let directory = std::path::PathBuf::from(root).join(mode);
        std::fs::create_dir_all(&directory).unwrap();
        for (name, bytes) in [
            ("stdout", output.stdout.as_slice()),
            ("stderr", output.stderr.as_slice()),
        ] {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(name))
                .unwrap();
            file.write_all(bytes).unwrap();
        }
        std::fs::write(
            directory.join("status"),
            format!("{}; timeout={timed_out}\n", output.status),
        )
        .unwrap();
    }
    assert!(!timed_out, "arena {mode} exceeded 20 seconds: {output:?}");
    assert!(output.status.success(), "arena {mode}: {output:?}");
    assert!(output.stderr.is_empty(), "arena {mode}: {output:?}");
    print!("{}", String::from_utf8(output.stdout).unwrap());
}

#[test]
fn parent_first_fork_reservations_preserve_both_executable_images() {
    run("parent-first");
}
#[test]
fn child_first_fork_reservations_preserve_both_executable_images() {
    run("child-first");
}
#[test]
fn concurrent_fork_reservations_share_the_exact_finite_capacity() {
    run("concurrent");
}
#[test]
fn arena_capacity_and_reachability_remain_checked() {
    run("capacity");
}
#[test]
fn metadata_mmap_failure_releases_pending_code_aliases_and_descriptor() {
    run("mmap-error");
}
#[test]
fn late_close_failure_preserves_reused_descriptor_and_releases_mappings() {
    run("close-error");
}
