//! Cell E17, in the group EXCLUDED-DECLARED: a ring descriptor can survive
//! exec, but its polling thread does not. The launcher, a mode that never
//! installs the runtime, makes a submission-polling ring, clears the
//! descriptor's close-on-exec flag with `F_SETFD, 0` (the flag is set at
//! creation and can be cleared), and checks that its polling thread exists.
//! It then executes this binary in the mode that installs the runtime first
//! and only then looks: the inherited descriptor is still open and no
//! `iou-sqp-` thread is left, because exec ends every other thread of the
//! process before the new image runs.
//!
//! The cell gives NO RESULT, never a pass, when the launcher's `F_SETFD`
//! does not return 0, when `F_GETFD` does not then read 0, or when no
//! polling thread existed before exec: the successor's checks would then
//! show nothing.

use std::ffi::CString;
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use super::activate;
use super::io_uring;
use super::report;

const IDLE_MS: u32 = 60_000;
/// How long the launcher waits for the polling thread to name itself.
const POLLER_LIMIT: Duration = Duration::from_secs(2);

/// The launcher. It runs natively and either reports why the cell has no
/// result or replaces itself with the successor.
pub(super) fn launch(path: &Path) {
    let ring = match io_uring::polling_descriptor(IDLE_MS) {
        Ok(ring) => ring,
        Err(errno) => {
            let setup = "native io_uring_setup with IORING_SETUP_SQPOLL";
            let reason = format!("{setup} answered errno={errno}");
            report("refused", "E17", "exec", &reason);
            return;
        }
    };
    // The ring's descriptor is created close-on-exec; F_SETFD 0 clears the
    // flag so that the descriptor survives exec.
    let cleared = unsafe { libc::fcntl(ring, libc::F_SETFD, 0) };
    let flags = unsafe { libc::fcntl(ring, libc::F_GETFD) };
    let pollers = io_uring::polling_threads_within(POLLER_LIMIT);
    let setup = format!("ring={ring} F_SETFD={cleared} F_GETFD={flags} pollers={pollers}");
    report("setup", "E17", "exec", &setup);
    let missing = if cleared != 0 {
        Some(format!("the launcher's F_SETFD 0 answered {cleared}"))
    } else if flags != 0 {
        Some(format!("F_GETFD reads {flags:#x} after F_SETFD 0, not 0"))
    } else if pollers == 0 {
        Some("no iou-sqp- thread existed before exec".to_string())
    } else {
        None
    };
    if let Some(reason) = missing {
        report("no-result", "E17", "exec", &reason);
        return;
    }
    let mut stdout = std::io::stdout();
    stdout.flush().expect("flushing stdout before exec");
    let program = std::env::current_exe().expect("this executable");
    let arguments = [
        CString::new(program.as_os_str().as_bytes()),
        CString::new("exec-ring-guest"),
        CString::new(path.as_os_str().as_bytes()),
        CString::new(ring.to_string()),
    ]
    .map(|argument| argument.expect("an argument without NUL"));
    let mut pointers: Vec<*const libc::c_char> = arguments.iter().map(|a| a.as_ptr()).collect();
    pointers.push(core::ptr::null());
    unsafe { libc::execv(arguments[0].as_ptr(), pointers.as_ptr()) };
    let reason = format!("execv answered {}", std::io::Error::last_os_error());
    report("fail", "E17", "exec", &reason);
    std::process::exit(1);
}

/// The successor: it activates the runtime before anything else, then
/// reports whether the inherited ring descriptor is open and how many
/// polling threads the process has.
pub(super) fn guest(path: &Path, descriptor: &OsStr) {
    let descriptor: Option<i32> = descriptor.to_str().and_then(|text| text.parse().ok());
    let descriptor = descriptor.expect("the inherited ring descriptor");
    activate(path, "exec");
    let state = match std::fs::read_link(format!("/proc/self/fd/{descriptor}")) {
        Ok(target) if target.as_os_str() == "anon_inode:[io_uring]" => "open".to_string(),
        Ok(target) => format!("other:{}", target.display()),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => "closed".to_string(),
        Err(error) => format!("unreadable:{error}"),
    };
    let pollers = io_uring::polling_threads();
    let observed = format!("fd={state} poller-threads={pollers}");
    report("cell", "E17", "exec", &observed);
}
