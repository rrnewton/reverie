//! The one place a captured child is WAITED FOR instead of a pipe being read to EOF.
//!
//! ⚠️ WHAT IS WRONG WITH `Command::output()`. It gives the child a PIPE and reads
//! it to EOF. EOF arrives when the LAST HOLDER of the write end closes it, which
//! is not the same event as the child exiting. Any background grandchild that
//! inherited the descriptor keeps the caller blocked on a child that has already
//! been reaped and whose exit status is already known.
//!
//! `git` triggers this with no unusual configuration: `gc.auto` defaults to 6700
//! and `gc.autoDetach` defaults to true, so an ordinary `git rev-list` can fork a
//! detached `git gc --auto` that inherits stderr and outlives it.
//!
//! ⚠️ AND RUST HAS NO TIMEOUT PARAMETER AT ALL. `std::process::Command` exposes
//! no bounded wait, so unlike the Python side there is no "already bounded"
//! subset to exempt -- every `.output()` in this tree was exposed.
//!
//! ⚠️ WRAPPING THE COMMAND IN `timeout(1)` DOES NOT FIX IT, AND THAT WAS
//! MEASURED RATHER THAN ASSUMED (2026-08-25). Capturing
//! `timeout --signal=TERM --kill-after=1s 2s sh -c 'sleep 30 & echo started'`
//! still blocked past 10s, exactly as the same command without `timeout` did.
//! A control with no grandchild returned in 0.0s, so the probe was measuring
//! something real. `timeout` bounds the CHILD; the caller is stuck on the PIPE,
//! and those are different waits. `validate_lock::current_main_freshness_check`
//! wraps its `git fetch` in `timeout` and was exposed anyway -- on the validate
//! admission path.
//!
//! THE FIX IS TO REMOVE THE PIPE. Output goes to a scratch FILE, which has no
//! EOF-waiting semantics, so `wait()` on the child is the only gate. This is
//! deliberately NOT a timeout (bounds the wait and discards the reason, and
//! needs a number that is wrong for either a fast box or a slow one) and NOT
//! `gc.auto=0` (hides this grandchild and leaves the next hook, pager,
//! credential helper or ssh multiplexer to reintroduce the hang elsewhere).
//!
//! The Python half of this defect is `ci-hub/lib/bounded_capture.py`, landed at
//! 07159978/b05eb161. Same reasoning, same non-remedies rejected.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A read/write scratch file that is UNLINKED IMMEDIATELY.
///
/// The descriptors stay valid after the name is gone, so the capture works
/// normally, but nothing survives in the temp directory -- not on panic, not on
/// SIGKILL, not when a landing is interrupted. A named temp file cleaned up in a
/// `Drop` would leak on exactly the abnormal exits this tree keeps hitting.
fn scratch_file(tag: &str) -> io::Result<File> {
    let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "ci-hub-bounded-output-{}-{sequence}-{tag}",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

/// Capture a command's output by waiting for the CHILD, not for pipe EOF.
///
/// Both methods return the same `std::process::Output` that `Command::output()`
/// and `Child::wait_with_output()` return, so a call site changes by one token
/// and its error handling, status matching and byte handling stay as they were.
pub trait BoundedOutput {
    /// Drop-in for `Command::output()`. `stdin` is `null`, which is the other
    /// half of not hanging: a child that decides to prompt gets EOF rather than
    /// blocking on a terminal no agent is watching.
    fn bounded_output(&mut self) -> io::Result<Output>;

    /// Drop-in for the `spawn()` / write stdin / `wait_with_output()` idiom.
    fn bounded_output_with_input(&mut self, input: &[u8]) -> io::Result<Output>;

    /// Capture output while bounding the direct child and its process group.
    ///
    /// This is intentionally separate from [`Self::bounded_output`]. Most
    /// commands have no defensible wall-clock deadline. Remote freshness probes
    /// do: an unavailable remote must become UNKNOWN instead of holding every
    /// caller indefinitely. `None` means that deadline was reached after the
    /// complete process group was terminated and the direct child was reaped.
    fn bounded_output_with_timeout(&mut self, timeout: Duration) -> io::Result<Option<Output>>;

    /// Stream stdout normally while retaining stderr in a regular file until
    /// the direct child exits. Unlike a pipe, the file cannot keep the caller
    /// waiting merely because a detached grandchild inherited its descriptor.
    fn bounded_status_with_stderr(&mut self) -> io::Result<(std::process::ExitStatus, Vec<u8>)>;
}

impl BoundedOutput for Command {
    fn bounded_output(&mut self) -> io::Result<Output> {
        capture(self, None)
    }

    fn bounded_output_with_input(&mut self, input: &[u8]) -> io::Result<Output> {
        capture(self, Some(input))
    }

    fn bounded_output_with_timeout(&mut self, timeout: Duration) -> io::Result<Option<Output>> {
        capture_with_timeout(self, timeout)
    }

    fn bounded_status_with_stderr(&mut self) -> io::Result<(std::process::ExitStatus, Vec<u8>)> {
        let mut stderr_file = scratch_file("err")?;
        self.stderr(Stdio::from(stderr_file.try_clone()?));
        let status = wait_for_child(&mut self.spawn()?)?;
        stderr_file.seek(SeekFrom::Start(0))?;
        let mut stderr = Vec::new();
        stderr_file.read_to_end(&mut stderr)?;
        Ok((status, stderr))
    }
}

fn capture_with_timeout(command: &mut Command, timeout: Duration) -> io::Result<Option<Output>> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bounded command timeout must be positive",
        ));
    }
    let mut stdout_file = scratch_file("timed-out")?;
    let mut stderr_file = scratch_file("timed-err")?;
    command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .stdin(Stdio::null())
        // The child is its own process-group leader. A timeout therefore owns
        // a narrowly scoped kill target that includes ordinary descendants,
        // without ever signalling the caller's group.
        .process_group(0);
    let mut child = command.spawn()?;
    let pgid = child.id();
    let deadline = Instant::now() + timeout;
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            terminate_process_group(pgid);
            // Reap the direct child. Descendants are separately covered by the
            // process-group signals and the absence check below.
            let _ = child.wait()?;
            wait_for_process_group_absence(pgid)?;
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };

    stdout_file.seek(SeekFrom::Start(0))?;
    stderr_file.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    stdout_file.read_to_end(&mut stdout)?;
    let mut stderr = Vec::new();
    stderr_file.read_to_end(&mut stderr)?;
    if timed_out {
        return Ok(None);
    }
    let status = child.wait()?;
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

fn terminate_process_group(pgid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    if pgid <= 1 {
        return;
    }
    // SAFETY: the target is the negative ID of the process group created just
    // above, never -1 or zero. SIGKILL makes the deadline independent of a
    // credential helper or hook that ignores SIGTERM.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

fn process_group_exists(pgid: u32) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return true;
    };
    if pgid <= 1 {
        return true;
    }
    // SAFETY: signal zero only probes the narrowly scoped process group.
    let rc = unsafe { libc::kill(-pgid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn wait_for_process_group_absence(pgid: u32) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_group_exists(pgid) {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed-out process group {pgid} remained live after SIGKILL"),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn capture(command: &mut Command, input: Option<&[u8]>) -> io::Result<Output> {
    let mut stdout_file = scratch_file("out")?;
    let mut stderr_file = scratch_file("err")?;
    command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = command.spawn()?;

    if let Some(bytes) = input {
        let write_result = match child.stdin.take() {
            // Dropping the handle at the end of this block closes it, which is
            // what lets the child see EOF and exit.
            Some(mut stdin) => stdin.write_all(bytes).and_then(|()| stdin.flush()),
            None => Ok(()),
        };
        if let Err(error) = write_result {
            // A child that exited before reading its input gives EPIPE. That is
            // not a capture failure -- its exit status IS the answer -- so keep
            // going and let the caller judge the status.
            if error.kind() != io::ErrorKind::BrokenPipe {
                // ⚠️ REAP BEFORE RETURNING. An early return here would drop the
                // `Child`, and dropping a `Child` does NOT wait, so the failure
                // path would leak a zombie every time. That is the same
                // spawn-without-reap defect this file exists next to.
                let _ = wait_for_child(&mut child);
                return Err(error);
            }
        }
    }

    let status = wait_for_child(&mut child)?;

    stdout_file.seek(SeekFrom::Start(0))?;
    stderr_file.seek(SeekFrom::Start(0))?;
    let mut stdout = Vec::new();
    stdout_file.read_to_end(&mut stdout)?;
    let mut stderr = Vec::new();
    stderr_file.read_to_end(&mut stderr)?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn wait_for_child(child: &mut Child) -> io::Result<ExitStatus> {
    child.wait()
}

#[cfg(test)]
mod tests {
    use super::{wait_for_child, BoundedOutput, SCRATCH_SEQUENCE};
    use std::io;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A child that exits IMMEDIATELY while leaving one background grandchild
    /// holding the inherited stdout/stderr. This is the owner's hung box in
    /// miniature: the child is gone and its status is knowable, but the pipe
    /// never sees EOF.
    const ORPHAN_HOLDER: &str = "sleep 30 & echo started; exit 7";

    #[test]
    fn a_genuinely_missing_child_is_still_reported_as_echild() {
        let mut child = Command::new("/bin/true").spawn().unwrap();
        let mut status = 0;
        // SAFETY: this waits for the exact child just spawned by this test.
        let reaped = unsafe { libc::waitpid(child.id() as libc::pid_t, &mut status, 0) };
        assert_eq!(reaped, child.id() as libc::pid_t);

        let error = wait_for_child(&mut child)
            .expect_err("a child reaped outside Child::wait must remain a loud failure");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }

    /// Run `body` on a worker thread and report whether it FINISHED.
    ///
    /// ⚠️ THE BOUND IS ON THE TEST, NEVER ON THE PRODUCT. The defect under test
    /// is a hang, so a test that simply called the bad code would block the
    /// whole suite on regression instead of naming itself. `bounded_output`
    /// itself takes no timeout, and giving it one would be the fix this change
    /// explicitly rejects.
    fn finishes<T: Send + 'static>(
        body: impl FnOnce() -> T + Send + 'static,
        seconds: u64,
    ) -> Option<T> {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(body());
        });
        receiver.recv_timeout(Duration::from_secs(seconds)).ok()
    }

    #[test]
    fn returns_while_an_orphan_grandchild_still_holds_the_descriptors() {
        let finished = finishes(
            || {
                Command::new("sh")
                    .args(["-c", ORPHAN_HOLDER])
                    .bounded_output()
            },
            25,
        );
        let output = finished
            .expect(
                "bounded_output did not return while an orphan grandchild held the \
                 inherited descriptors -- it has regressed to pipe semantics",
            )
            .expect("the command itself ran");
        assert_eq!(
            output.status.code(),
            Some(7),
            "the child's real exit status"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
    }

    /// ⚠️ THE MUST-FAIL CONTROL. If plain `Command::output()` does NOT hang on
    /// this same fixture, the fixture has stopped reproducing the defect and the
    /// test above is vacuous -- it would pass against any implementation at all.
    #[test]
    fn the_control_confirms_plain_output_really_does_hang_here() {
        let finished = finishes(
            || {
                Command::new("sh")
                    .args(["-c", ORPHAN_HOLDER])
                    .output()
                    .map(|output| output.status.code())
            },
            8,
        );
        assert!(
            finished.is_none(),
            "Command::output() returned promptly on a command that leaves an orphan \
             holding the pipe. The scenario no longer reproduces the defect, so the \
             sibling test proves nothing -- fix this fixture before trusting it."
        );
    }

    #[test]
    fn reports_stdout_stderr_and_status_like_output_does() {
        let output = Command::new("sh")
            .args(["-c", "echo out; echo err >&2; exit 3"])
            .bounded_output()
            .expect("ran");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "err");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn status_with_stderr_streams_stdout_and_captures_stderr() {
        let (status, stderr) = Command::new("sh")
            .args(["-c", "echo err >&2; exit 3"])
            .stdout(Stdio::null())
            .bounded_status_with_stderr()
            .expect("ran");
        assert_eq!(status.code(), Some(3));
        assert_eq!(stderr, b"err\n");
    }

    #[test]
    fn status_with_stderr_does_not_wait_for_a_grandchild_holding_the_file() {
        let finished = finishes(
            || {
                Command::new("sh")
                    .args(["-c", "sleep 30 >&2 & echo err >&2; exit 7"])
                    .stdout(Stdio::null())
                    .bounded_status_with_stderr()
            },
            5,
        );
        let (status, stderr) = finished.expect("regular-file stderr capture hung").unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(stderr, b"err\n");
    }

    #[test]
    fn carries_bytes_that_are_not_utf8() {
        let output = Command::new("printf")
            .args(["\\x00\\xff"])
            .bounded_output()
            .expect("ran");
        assert_eq!(output.stdout, vec![0x00, 0xff]);
    }

    #[test]
    fn delivers_stdin_and_closes_it() {
        let output = Command::new("cat")
            .bounded_output_with_input(b"hello")
            .expect("ran");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hello");
        assert_eq!(output.status.code(), Some(0));
    }

    #[test]
    fn a_child_that_ignores_its_input_is_not_reported_as_a_capture_failure() {
        // `true` exits without reading stdin, so the write takes EPIPE. The
        // child's status is the answer and must survive.
        let output = Command::new("true")
            .bounded_output_with_input(&[b'x'; 1024 * 512])
            .expect("an early-exiting child is not a capture error");
        assert_eq!(output.status.code(), Some(0));
    }

    #[test]
    fn stdin_defaults_to_null_so_a_prompting_child_cannot_block() {
        let finished = finishes(
            || {
                Command::new("sh")
                    .args(["-c", "read line; echo \"[$line]\""])
                    .bounded_output()
            },
            20,
        );
        let output = finished
            .expect("a child reading stdin blocked; stdin is not null")
            .expect("ran");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[]");
    }

    fn pid_is_absent(pid: i32) -> bool {
        // SAFETY: signal zero is a liveness probe for the exact positive PID
        // written by the fixture itself.
        (unsafe { libc::kill(pid, 0) == -1 })
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    fn read_pid(path: &Path) -> i32 {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[test]
    fn timeout_kills_and_reaps_hanging_child_and_grandchild() {
        let root = std::env::temp_dir().join(format!(
            "ci-hub-bounded-timeout-{}-{}",
            std::process::id(),
            SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let child_pid = root.join("child.pid");
        let grandchild_pid = root.join("grandchild.pid");
        let script = format!(
            "echo $$ > '{}'; sh -c 'echo $$ > \"$1\"; while :; do sleep 10; done' sh '{}' & while [ ! -s '{}' ]; do :; done; wait",
            child_pid.display(),
            grandchild_pid.display(),
            grandchild_pid.display(),
        );
        let started = Instant::now();
        let output = Command::new("sh")
            .args(["-c", &script])
            .bounded_output_with_timeout(Duration::from_millis(200))
            .expect("the timeout must terminate the complete process group");

        assert!(output.is_none(), "the hanging fixture must time out");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the 200ms bound plus process cleanup took {:?}",
            started.elapsed()
        );
        let child = read_pid(&child_pid);
        let grandchild = read_pid(&grandchild_pid);
        assert!(pid_is_absent(child), "direct child {child} was not reaped");
        assert!(
            pid_is_absent(grandchild),
            "grandchild {grandchild} survived the process-group timeout"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timed_capture_preserves_completed_output_and_status() {
        let output = Command::new("sh")
            .args(["-c", "printf out; printf err >&2; exit 7"])
            .bounded_output_with_timeout(Duration::from_secs(2))
            .expect("completed command capture succeeds")
            .expect("completed command is not a timeout");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn timed_capture_rejects_an_unbounded_zero_deadline() {
        let error = Command::new("true")
            .bounded_output_with_timeout(Duration::ZERO)
            .expect_err("zero must not disable the deadline");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn scratch_files_do_not_accumulate_in_the_temp_directory() {
        let before = leaked_scratch_files();
        for _ in 0..8 {
            let _ = Command::new("true").bounded_output().expect("ran");
        }
        assert_eq!(
            leaked_scratch_files(),
            before,
            "scratch files are unlinked at creation and must never appear by name"
        );
    }

    /// ⚠️ THE INVARIANT, ENFORCED RATHER THAN DOCUMENTED. A shared wrapper that
    /// nothing makes you use decays into the fourth copy of the problem: the
    /// Python side of this defect was written independently THREE times and one
    /// copy had already drifted before it was collapsed. This fails the moment a
    /// bare `.output()` reappears in non-test ci-hub Rust, naming the file and
    /// line, so the next person does not have to re-derive why.
    ///
    /// Test code is exempt on purpose -- the must-fail control above is REQUIRED
    /// to call bare `.output()`, and a rule that forbade it would forbid the
    /// evidence that this whole change works.
    #[test]
    fn no_bare_output_call_survives_outside_test_code() {
        let ci_hub = std::path::Path::new(file!())
            .parent()
            .and_then(|lib| lib.parent())
            .expect("this file lives at <ci-hub>/lib/bounded_output.rs")
            .to_path_buf();
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        visit_rust_files(&ci_hub, &mut |path| {
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            if path.file_name().and_then(|n| n.to_str()) == Some("bounded_output.rs") {
                return;
            }
            scanned += 1;
            let test_mod = text
                .find("\nmod tests {")
                .or_else(|| text.find("\n    mod tests {"));
            for (index, line) in text.lines().enumerate() {
                if !line.contains(".output()") || line.contains(".bounded_output()") {
                    continue;
                }
                let offset = text.lines().take(index).map(|l| l.len() + 1).sum::<usize>();
                if test_mod.is_some_and(|start| offset > start) {
                    continue;
                }
                offenders.push(format!("{}:{}", path.display(), index + 1));
            }
        });
        assert!(
            scanned > 3,
            "the scan found only {scanned} Rust files under {} -- it is measuring nothing, \
             which would make this guard pass for the wrong reason",
            ci_hub.display()
        );
        assert!(
            offenders.is_empty(),
            "bare Command::output() reads a pipe to EOF and can hang on an orphan \
             grandchild. Use `.bounded_output()` from lib/bounded_output.rs. Offenders: {offenders:?}"
        );
    }

    fn visit_rust_files(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                visit_rust_files(&path, visit);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                visit(&path);
            }
        }
    }

    fn leaked_scratch_files() -> usize {
        std::fs::read_dir(std::env::temp_dir())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("ci-hub-bounded-output-")
                    })
                    .count()
            })
            .unwrap_or(0)
    }
}
