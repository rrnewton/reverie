// Shared by unit and KVM integration controls. Each selected case runs in a
// separate bounded process; the interposer remains dormant until explicitly armed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub(crate) enum Case {
    Reservation = 1,
    SecondExtent = 2,
    Foreign = 3,
    Collision = 4,
    Unmap = 5,
    WrongAddress = 6,
    MiddleExtent = 7,
}

impl Case {
    fn from_stage(stage: i32) -> Self {
        match stage {
            1 => Self::Reservation,
            2 => Self::SecondExtent,
            3 => Self::Foreign,
            4 => Self::Collision,
            5 => Self::Unmap,
            6 => Self::WrongAddress,
            7 => Self::MiddleExtent,
            _ => panic!("unknown alias failure stage: {stage}"),
        }
    }
}

pub(crate) struct Fault {
    case: Case,
    arm: unsafe extern "C" fn(i32),
    finish: unsafe extern "C" fn(),
    count: unsafe extern "C" fn(i32) -> libc::c_ulong,
}

struct TestDirectory {
    path: std::path::PathBuf,
    removed: bool,
}

impl TestDirectory {
    fn new(path: std::path::PathBuf) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self {
            path,
            removed: false,
        })
    }

    fn close(mut self) -> std::io::Result<()> {
        std::fs::remove_dir_all(&self.path)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if !self.removed {
            // Keep the original failure if compilation or child setup panics.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// Retain the original two stages for every existing caller.
pub(crate) fn child(test: &str) -> Option<Fault> {
    child_cases(test, &[Case::Reservation, Case::SecondExtent])
}

pub(crate) fn child_case(test: &str, case: Case) -> Option<Fault> {
    child_cases(test, &[case])
}

fn child_cases(test: &str, cases: &[Case]) -> Option<Fault> {
    if std::env::var("REVERIE_ALIAS_FAILURE_TEST").as_deref() == Ok(test) {
        let stage = std::env::var("REVERIE_ALIAS_FAILURE_STAGE")
            .unwrap()
            .parse()
            .unwrap();
        let case = Case::from_stage(stage);
        assert!(cases.contains(&case), "unselected alias case: {case:?}");
        // SAFETY: the child retains our explicitly loaded fixture. These
        // exported symbols have the exact signatures below.
        let (arm, finish, count) = unsafe {
            let arm = libc::dlsym(libc::RTLD_DEFAULT, c"reverie_alias_failure_arm".as_ptr());
            let finish = libc::dlsym(libc::RTLD_DEFAULT, c"reverie_alias_failure_finish".as_ptr());
            let count = libc::dlsym(libc::RTLD_DEFAULT, c"reverie_alias_failure_count".as_ptr());
            assert!(!arm.is_null() && !finish.is_null() && !count.is_null());
            (
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn(i32)>(arm),
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn()>(finish),
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn(i32) -> libc::c_ulong>(
                    count,
                ),
            )
        };
        return Some(Fault {
            case,
            arm,
            finish,
            count,
        });
    }
    assert!(!cases.is_empty(), "an alias child must select a case");
    let directory = TestDirectory::new(std::env::temp_dir().join(format!(
        "reverie-alias-failure-{}-{}",
        std::process::id(),
        test.replace(':', "_")
    )))
    .unwrap();
    let library = directory.path.join("fault.so");
    let source = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(
            || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            std::path::PathBuf::from,
        )
        .join("tests/fixtures/getdents_alias_failure.c");
    let build = std::process::Command::new("timeout")
        .args([
            "--kill-after=2s",
            "30s",
            "/usr/bin/gcc",
            "-O2",
            "-shared",
            "-fPIC",
            "-pthread",
        ])
        .arg(source)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(build.status.success(), "{build:?}");
    let outputs: Vec<_> = cases
        .iter()
        .map(|&case| {
            let stage = (case as i32).to_string();
            // Fresh per-case logfile in a newly created private directory.
            let execution_log = directory.path.join(format!("libtest-{stage}.log"));
            let output = std::process::Command::new("timeout")
                .args(["--kill-after=2s", "30s"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    test,
                    "--nocapture",
                    "--test-threads=1",
                    "--logfile",
                ])
                .arg(&execution_log)
                .env("REVERIE_ALIAS_FAILURE_TEST", test)
                .env("REVERIE_ALIAS_FAILURE_STAGE", &stage)
                .env("LD_PRELOAD", &library)
                .output()
                .unwrap();
            let executions = std::fs::read_to_string(&execution_log);
            (stage, output, executions)
        })
        .collect();
    directory.close().unwrap();
    for (stage, output, executions) in outputs {
        eprintln!("\nstage={stage} {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{test}: stage={stage} status={:?} stdout={stdout} stderr={stderr}",
            output.status.code()
        );
        let executions = executions.unwrap_or_else(|error| panic!(
            "{test}: stage={stage} missing execution logfile: {error}; stdout={stdout} stderr={stderr}"
        ));
        // stdout can contain arbitrary guest bytes and is never execution proof.
        assert_eq!(
            executions,
            format!("ok {test}\n"),
            "\n{test}: alias child execution record mismatch; stage={stage}; exit_code={:?}\nexecution records: {executions:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        );
    }
    None
}

impl Fault {
    pub(crate) fn arm(&self) {
        // SAFETY: resolved from the retained fixture.
        unsafe { (self.arm)(self.case as i32) };
    }

    pub(crate) fn pages(&self) -> usize {
        if self.case == Case::MiddleExtent {
            5
        } else {
            3
        }
    }

    pub(crate) fn cause<'a>(&self, error: &'a crate::Error) -> Option<&'a crate::Error> {
        // The caller selected this case before any observed error existed.
        match self.case {
            Case::Collision => mapping_collision_cause(error),
            Case::WrongAddress => mapping_unexpected_address_cause(error),
            _ => mapping_cause(error),
        }
    }

    pub(crate) fn assert_fired(&self) {
        // Finish observes the foreign marker before its owner releases it.
        // SAFETY: resolved from the retained fixture; this method is called once.
        unsafe { (self.finish)() };
        // Columns match the fixed C enum; the first five preserve the old oracle.
        // reserve, extent, fired, cleanup, first-bytes, prep, released, page-mask,
        // worker-created/joined, foreign-created/before/after/finish/released,
        // real-EEXIST, wrong-created/released, distinct-thread, bad-cleanup.
        let counts = std::array::from_fn::<_, 20, _>(|i| unsafe { (self.count)(i as i32) });
        let expected = match self.case {
            Case::Reservation => [1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Case::SecondExtent => [1, 2, 1, 1, 1, 2, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Case::Foreign => [1, 2, 1, 1, 1, 2, 2, 3, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 0],
            Case::Collision => [1, 2, 1, 1, 1, 2, 2, 3, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 1, 0],
            Case::Unmap => [1, 1, 1, 1, 1, 2, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Case::WrongAddress => [1, 2, 1, 1, 1, 2, 2, 3, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0],
            Case::MiddleExtent => [1, 2, 1, 2, 1, 2, 2, 27, 1, 1, 1, 2, 2, 1, 1, 0, 0, 0, 1, 0],
        };
        assert_eq!(counts, expected, "alias case {:?}", self.case);
        eprintln!("\nalias failure case={:?} counters={counts:?}", self.case);
    }
}

// Ownership may repeat a cause, but every leaf must retain that exact cause.
// Context-bearing errors are not ownership envelopes and must remain visible.
pub(crate) fn mapping_cause(error: &crate::Error) -> Option<&crate::Error> {
    mapping_cause_for_errno(error, libc::ENOMEM)
}

// A distinct collision control expects exactly EEXIST. Existing resource
// failure controls continue to call the ENOMEM-only mapping_cause above.
pub(crate) fn mapping_collision_cause(error: &crate::Error) -> Option<&crate::Error> {
    mapping_cause_for_errno(error, libc::EEXIST)
}

const UNEXPECTED_ADDRESS: &str =
    "MAP_FIXED_NOREPLACE installed a writable alias at an unexpected address";

pub(crate) fn mapping_unexpected_address_cause(error: &crate::Error) -> Option<&crate::Error> {
    mapping_cause_expected(error, ExpectedCause::UnexpectedAddress)
}

#[derive(Clone, Copy)]
enum ExpectedCause {
    Errno(libc::c_int),
    UnexpectedAddress,
}

fn mapping_cause_for_errno(error: &crate::Error, errno: libc::c_int) -> Option<&crate::Error> {
    mapping_cause_expected(error, ExpectedCause::Errno(errno))
}

// Fixed public wrappers select the expected leaf independently of the observed
// result. Every repeated leaf must be the same object; context is never erased.
fn mapping_cause_expected(error: &crate::Error, expected: ExpectedCause) -> Option<&crate::Error> {
    fn visit<'a>(
        error: &'a crate::Error,
        original: &mut Option<&'a crate::Error>,
        expected: ExpectedCause,
    ) -> bool {
        match error {
            crate::Error::MemoryMapping(io) => {
                let matches = match expected {
                    ExpectedCause::Errno(errno) => io.raw_os_error() == Some(errno),
                    ExpectedCause::UnexpectedAddress => {
                        io.raw_os_error().is_none()
                            && io.kind() == std::io::ErrorKind::Other
                            && io.to_string() == UNEXPECTED_ADDRESS
                    }
                };
                if !matches {
                    return false;
                }
                if let Some(original) = original {
                    std::ptr::eq(*original, error)
                } else {
                    *original = Some(error);
                    true
                }
            }
            crate::Error::SharedFailure(cause) => visit(cause, original, expected),
            crate::Error::WithCleanup { primary, cleanup } => {
                visit(primary, original, expected)
                    && cleanup.iter().all(|cause| visit(cause, original, expected))
            }
            _ => false,
        }
    }
    let mut original = None;
    if visit(error, &mut original, expected) {
        original
    } else {
        None
    }
}

#[test]
fn mapping_failure_oracle_keeps_all_causes_and_context() {
    use std::sync::Arc;

    use crate::Error;

    let original = Arc::new(Error::MemoryMapping(std::io::Error::from_raw_os_error(
        libc::ENOMEM,
    )));
    let direct = Error::SharedFailure(original.clone());
    assert!(std::ptr::eq(mapping_cause(&direct).unwrap(), &*original));
    let positive = Error::WithCleanup {
        primary: Arc::new(Error::SharedFailure(Arc::new(Error::SharedFailure(
            original.clone(),
        )))),
        cleanup: vec![Arc::new(Error::SharedFailure(original.clone()))],
    };
    assert!(std::ptr::eq(mapping_cause(&positive).unwrap(), &*original));
    for extra in [
        Error::MemoryMapping(std::io::Error::from_raw_os_error(libc::ENOMEM)),
        Error::MemoryMapping(std::io::Error::from_raw_os_error(libc::EACCES)),
        Error::UnexpectedVcpuExit("unrelated cleanup failure".to_owned()),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![original.clone(), Arc::new(extra)],
        };
        assert!(mapping_cause(&negative).is_none(), "{negative:?}");
    }
    for context in [
        Error::WorkerFailure {
            tid: 3,
            error: original.clone(),
        },
        Error::Cleanup {
            phase: "unexpected cleanup context",
            error: original.clone(),
        },
        Error::SignalEffects {
            cause: original.clone(),
            dequeues: Vec::new(),
            acknowledged_through: 0,
            publications: Vec::new(),
            raw_result: None,
            context: None,
        },
        Error::ExecWorkerTeardown(Box::new(Error::SharedFailure(original.clone()))),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![Arc::new(context)],
        };
        assert!(mapping_cause(&negative).is_none(), "{negative:?}");
    }
}

// Controls for https://github.com/rrnewton/reverie/issues/813. The support
// module has this same libtest prefix in unit and integration test binaries.
#[test]
fn bounded_alias_child_records_exact_completion() {
    const TEST: &str = "alias_failure::bounded_alias_child_records_exact_completion";
    if let Some(fault) = child(TEST) {
        // Resolve the fixture in the child but leave it dormant. This tests the
        // subprocess execution contract independently of KVM or fault stages.
        assert!(matches!(fault.case, Case::Reservation | Case::SecondExtent));
    }
}

#[test]
fn bounded_alias_child_rejects_zero_matched_tests() {
    const MISSING: &str = "__reverie_deliberately_nonexistent_alias_failure_test__";
    let rejected = std::panic::catch_unwind(|| {
        let _ = child(MISSING);
    })
    .expect_err("the alias child helper accepted a zero-match child");
    let message = rejected
        .downcast_ref::<String>()
        .expect("the alias child helper did not emit a diagnostic");
    let expected =
        format!("{MISSING}: alias child execution record mismatch; stage=1; exit_code=Some(0)");
    assert!(
        message.lines().any(|line| line == expected),
        "unexpected alias child refusal: {message}"
    );
    assert!(
        message
            .lines()
            .any(|line| line == "execution records: \"\""),
        "the zero-match child unexpectedly recorded an execution: {message}"
    );
    assert!(
        message.lines().any(|line| line == "running 0 tests"),
        "the rejected child did not report zero matched tests: {message}"
    );
    assert!(
        message
            .lines()
            .any(|line| line
                .starts_with("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; ")),
        "the rejected child did not finish a successful zero-test run: {message}"
    );
}

#[test]
fn bounded_alias_child_rejects_forged_stdout_without_execution_record() {
    const TEST: &str =
        "alias_failure::bounded_alias_child_rejects_forged_stdout_without_execution_record";
    if std::env::var("REVERIE_ALIAS_FAILURE_TEST").as_deref() == Ok(TEST) {
        let forged = format!(
            "\ntest {TEST} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        );
        // SAFETY: the buffer is valid for this synchronous host write.
        assert_eq!(
            unsafe { libc::write(libc::STDOUT_FILENO, forged.as_ptr().cast(), forged.len()) },
            forged.len() as isize
        );
        // Exit before libtest writes its separate completion record.
        std::process::exit(0);
    }
    let rejected = std::panic::catch_unwind(|| {
        let _ = child(TEST);
    })
    .expect_err("the alias child helper accepted stdout without an execution record");
    let message = rejected
        .downcast_ref::<String>()
        .expect("the alias child helper did not emit a diagnostic");
    let expected =
        format!("{TEST}: alias child execution record mismatch; stage=1; exit_code=Some(0)");
    assert!(
        message.lines().any(|line| line == expected),
        "unexpected alias child refusal: {message}"
    );
    assert!(
        message
            .lines()
            .any(|line| line == "execution records: \"\""),
        "the prematurely exited child unexpectedly recorded an execution: {message}"
    );
    assert!(
        message.lines().any(|line| line == "running 1 test"),
        "the rejected child did not select the named test: {message}"
    );
    let forged_completion = format!("test {TEST} ... ok");
    assert!(
        message.lines().any(|line| line == forged_completion),
        "the rejected child did not emit forged stdout completion: {message}"
    );
}

#[test]
fn mapping_collision_oracle_keeps_all_causes_and_context() {
    use std::sync::Arc;

    use crate::Error;

    let original = Arc::new(Error::MemoryMapping(std::io::Error::from_raw_os_error(
        libc::EEXIST,
    )));
    let direct = Error::SharedFailure(original.clone());
    assert!(mapping_cause(&direct).is_none());
    assert!(mapping_cause_for_errno(&direct, libc::ENOMEM).is_none());
    assert!(std::ptr::eq(
        mapping_collision_cause(&direct).unwrap(),
        &*original
    ));
    let positive = Error::WithCleanup {
        primary: Arc::new(Error::SharedFailure(Arc::new(Error::SharedFailure(
            original.clone(),
        )))),
        cleanup: vec![Arc::new(Error::SharedFailure(original.clone()))],
    };
    assert!(std::ptr::eq(
        mapping_collision_cause(&positive).unwrap(),
        &*original
    ));
    for extra in [
        Error::MemoryMapping(std::io::Error::from_raw_os_error(libc::EEXIST)),
        Error::MemoryMapping(std::io::Error::from_raw_os_error(libc::ENOMEM)),
        Error::UnexpectedVcpuExit("unrelated cleanup failure".to_owned()),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![original.clone(), Arc::new(extra)],
        };
        assert!(mapping_collision_cause(&negative).is_none(), "{negative:?}");
    }
    for context in [
        Error::WorkerFailure {
            tid: 3,
            error: original.clone(),
        },
        Error::Cleanup {
            phase: "unexpected cleanup context",
            error: original.clone(),
        },
        Error::SignalEffects {
            cause: original.clone(),
            dequeues: Vec::new(),
            acknowledged_through: 0,
            publications: Vec::new(),
            raw_result: None,
            context: None,
        },
        Error::ExecWorkerTeardown(Box::new(Error::SharedFailure(original.clone()))),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![Arc::new(context)],
        };
        assert!(mapping_collision_cause(&negative).is_none(), "{negative:?}");
    }
}

#[test]
fn mapping_unexpected_address_oracle_keeps_all_causes_and_context() {
    use std::sync::Arc;

    use crate::Error;

    let original = Arc::new(Error::MemoryMapping(std::io::Error::other(
        UNEXPECTED_ADDRESS,
    )));
    let direct = Error::SharedFailure(original.clone());
    assert!(mapping_cause(&direct).is_none());
    assert!(mapping_cause_for_errno(&direct, libc::ENOMEM).is_none());
    assert!(std::ptr::eq(
        mapping_unexpected_address_cause(&direct).unwrap(),
        &*original
    ));
    let positive = Error::WithCleanup {
        primary: Arc::new(Error::SharedFailure(Arc::new(Error::SharedFailure(
            original.clone(),
        )))),
        cleanup: vec![Arc::new(Error::SharedFailure(original.clone()))],
    };
    assert!(std::ptr::eq(
        mapping_unexpected_address_cause(&positive).unwrap(),
        &*original
    ));
    for extra in [
        Error::MemoryMapping(std::io::Error::other(UNEXPECTED_ADDRESS)),
        Error::MemoryMapping(std::io::Error::from_raw_os_error(libc::EACCES)),
        Error::MemoryMapping(std::io::Error::other("a different protocol error")),
        Error::MemoryMapping(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            UNEXPECTED_ADDRESS,
        )),
        Error::UnexpectedVcpuExit("unrelated cleanup failure".to_owned()),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![original.clone(), Arc::new(extra)],
        };
        assert!(
            mapping_unexpected_address_cause(&negative).is_none(),
            "{negative:?}"
        );
    }
    for context in [
        Error::WorkerFailure {
            tid: 3,
            error: original.clone(),
        },
        Error::Cleanup {
            phase: "unexpected cleanup context",
            error: original.clone(),
        },
        Error::SignalEffects {
            cause: original.clone(),
            dequeues: Vec::new(),
            acknowledged_through: 0,
            publications: Vec::new(),
            raw_result: None,
            context: None,
        },
        Error::ExecWorkerTeardown(Box::new(Error::SharedFailure(original.clone()))),
    ] {
        let negative = Error::WithCleanup {
            primary: original.clone(),
            cleanup: vec![Arc::new(context)],
        };
        assert!(
            mapping_unexpected_address_cause(&negative).is_none(),
            "{negative:?}"
        );
    }
}
