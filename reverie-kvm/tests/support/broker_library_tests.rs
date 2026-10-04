// Source-only library-test adapter. Bootstrap occurs only in the separately
// built ordinary-main launcher, never in a libtest worker or constructor.
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::native_exit_broker;
use crate::native_exit_broker::BrokerClient;

#[path = "broker_test_client.rs"]
mod broker_test_client;
#[path = "broker_test_protocol.rs"]
mod broker_test_protocol;

const LAUNCHER_ENV: &str = "REVERIE_KVM_TEST_BROKER_LAUNCHER";
const LAUNCHER_NAME: &str = "reverie-kvm-broker-test-launcher";

/// Some means the authenticated, selected child may enter the test body.
/// None means its isolated exact child completed successfully and was reaped.
/// Neither unavailable helpers nor empty libtest selections are successes.
pub(crate) fn selected_child(test: &str) -> Option<BrokerClient> {
    assert!(matches!(
        test,
        "executor::tests::terminal_cleanup_preserves_live_shared_table_and_does_not_wait_for_its_lock"
            | "terminal_cleanup::tests::dropped_waiter_keeps_native_wait_and_owned_service_alive"
    ));
    if std::env::var(broker_test_protocol::CHILD_ENV).as_deref() == Ok(test) {
        broker_test_client::attach(test);
        return Some(broker_test_client::client());
    }
    let executable = std::env::current_exe().expect("library test executable");
    let launcher = launcher_path(&executable);
    let result_directory = ResultDirectory::new();
    let execution_log = result_directory.0.join("libtest.log");
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "30s"])
        .arg(launcher)
        .arg(executable)
        .args(["--exact", test, "--nocapture", "--logfile"])
        .arg(&execution_log)
        .env(broker_test_protocol::CHILD_ENV, test)
        .output()
        .expect("start ordinary-main broker test launcher");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{test}: status={:?} stdout={} stderr={}",
        output.status.code(),
        stdout,
        stderr
    );
    let executions = std::fs::read_to_string(&execution_log)
        .expect("selected libtest child must write its own execution record");
    assert_eq!(
        executions,
        format!("ok {test}\n"),
        "\n{test}: bounded child execution record mismatch; exit_code={:?}\nexecution records: {executions:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
    None
}

fn launcher_path(executable: &std::path::Path) -> PathBuf {
    let path = if let Some(value) = std::env::var_os(LAUNCHER_ENV) {
        let path = PathBuf::from(value);
        assert!(
            path.is_absolute(),
            "{LAUNCHER_ENV} must be an absolute helper path"
        );
        path
    } else {
        // Normal Cargo unit ELF is <target>/<profile>/deps/reverie_kvm-HASH.
        // Derive its profile directory, never search for a newest executable.
        let parent = executable.parent().expect("library executable parent");
        let profile = if parent.file_name().is_some_and(|name| name == "deps") {
            parent.parent().expect("Cargo profile directory")
        } else {
            parent
        };
        profile.join(LAUNCHER_NAME)
    };
    let metadata = std::fs::metadata(&path).unwrap_or_else(|error| {
        panic!("required ordinary-main broker helper {} is unavailable: {error}; build --bin {LAUNCHER_NAME} or supply {LAUNCHER_ENV}; no runtime bootstrap fallback", path.display())
    });
    assert!(
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        "required broker helper is not an executable file: {}",
        path.display()
    );
    path
}

struct ResultDirectory(PathBuf);
impl ResultDirectory {
    fn new() -> Self {
        let mut template = b"/tmp/reverie-kvm-broker-libtest-XXXXXX\0".to_vec();
        let result = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        assert!(
            !result.is_null(),
            "private libtest record directory: {}",
            std::io::Error::last_os_error()
        );
        let bytes = unsafe { std::ffi::CStr::from_ptr(result) }
            .to_bytes()
            .to_vec();
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        // mkdtemp atomically creates a new 0700 directory, with no reused path.
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        Self(path)
    }
}
impl Drop for ResultDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
