#![deny(warnings)]
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[path = "support/trampoline_tail_fixture.rs"]
mod fixture;

use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

const CHILD_ENV: &str = "LITEINST_TAIL_CHILD";
const DIRECTORY_ENV: &str = "LITEINST_TAIL_DIRECTORY";
const OUTPUT_LIMIT: u64 = 64 * 1024;

fn run(name: &str, kind: &str, placement: &str, argument: u64) {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        let directory = PathBuf::from(std::env::var_os(DIRECTORY_ENV).unwrap());
        fixture::run(kind, placement, argument, &directory);
        return;
    }
    let retained = std::env::var_os("LITEINST_TAIL_TEST_EVIDENCE");
    let root = retained.clone().map_or_else(
        || std::env::temp_dir().join(format!("liteinst-tail-{}", std::process::id())),
        PathBuf::from,
    );
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let output = |name| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))
            .unwrap()
    };
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, name)
        .env(DIRECTORY_ENV, &directory)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(output("stdout"))
        .stderr(output("stderr"));
    // SAFETY: the child only changes resource limits before exec; no allocation
    // or lock-taking is performed in this post-fork closure.
    unsafe {
        command.pre_exec(|| {
            for (resource, value) in [(libc::RLIMIT_CORE, 0), (libc::RLIMIT_FSIZE, OUTPUT_LIMIT)] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let start = Instant::now();
    let mut child = command.spawn().unwrap();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(5) {
            timed_out = true;
            // SAFETY: this live child's process group is owned by this test.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = std::fs::read(directory.join("stdout")).unwrap();
    let stderr = std::fs::read(directory.join("stderr")).unwrap();
    std::fs::write(directory.join("status"), format!(
        "status={status}; raw_status={}; signal={:?}; timeout={timed_out}; elapsed_seconds={}\n",
        status.into_raw(), status.signal(), start.elapsed().as_secs_f64(),
    )).unwrap();
    assert!(
        !timed_out && status.success(),
        "{name}: {status}; timeout={timed_out}; evidence={}\nstdout: {}\nstderr: {}",
        directory.display(),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    if retained.is_none() {
        std::fs::remove_dir_all(directory).unwrap();
    }
}

macro_rules! case {
    ($name:ident, $kind:literal, $placement:literal, $argument:literal) => {
        #[test]
        fn $name() {
            run(stringify!($name), $kind, $placement, $argument);
        }
    };
}

case!(direct_jcc_taken, "jcc", "direct", 0);
case!(direct_jcc_fallthrough, "jcc", "direct", 1);
case!(direct_call_return, "call", "direct", 0);
case!(near_jcc_taken, "jcc", "near", 0);
case!(near_jcc_fallthrough, "jcc", "near", 1);
case!(near_call_return, "call", "near", 0);
case!(far_near_return_jcc_taken, "jcc", "far-near-return", 0);
case!(far_near_return_jcc_fallthrough, "jcc", "far-near-return", 1);
case!(far_near_return_call_return, "call", "far-near-return", 0);
case!(far_far_return_jcc_taken, "jcc", "far-far-return", 0);
case!(far_far_return_jcc_fallthrough, "jcc", "far-far-return", 1);
case!(far_far_return_call_return, "call", "far-far-return", 0);
