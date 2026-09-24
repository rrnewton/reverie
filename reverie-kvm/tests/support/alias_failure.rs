// Shared by the unit and KVM integration controls. Every injected run is a
// separate bounded process, and the shim remains dormant until explicitly armed.
pub(crate) struct Fault {
    stage: i32,
    arm: unsafe extern "C" fn(i32),
    count: unsafe extern "C" fn(i32) -> libc::c_ulong,
}

pub(crate) fn child(test: &str) -> Option<Fault> {
    if std::env::var("REVERIE_ALIAS_FAILURE_TEST").as_deref() == Ok(test) {
        let stage = std::env::var("REVERIE_ALIAS_FAILURE_STAGE")
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: the child is launched with our fixture loaded. Both symbols
        // have these exact C signatures and outlive the test process.
        let (arm, count) = unsafe {
            let arm = libc::dlsym(libc::RTLD_DEFAULT, c"reverie_alias_failure_arm".as_ptr());
            let count = libc::dlsym(libc::RTLD_DEFAULT, c"reverie_alias_failure_count".as_ptr());
            assert!(!arm.is_null() && !count.is_null());
            (
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn(i32)>(arm),
                std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn(i32) -> libc::c_ulong>(
                    count,
                ),
            )
        };
        return Some(Fault { stage, arm, count });
    }
    let directory = std::env::temp_dir().join(format!(
        "reverie-alias-failure-{}-{}",
        std::process::id(),
        test.replace(':', "_")
    ));
    std::fs::create_dir(&directory).unwrap();
    let library = directory.join("fault.so");
    let build = std::process::Command::new("timeout")
        .args([
            "--kill-after=2s",
            "30s",
            "/usr/bin/gcc",
            "-O2",
            "-shared",
            "-fPIC",
        ])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/getdents_alias_failure.c"
        ))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(build.status.success(), "{build:?}");
    let outputs = ["1", "2"].map(|stage| {
        std::process::Command::new("timeout")
            .args(["--kill-after=2s", "30s"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env("REVERIE_ALIAS_FAILURE_TEST", test)
            .env("REVERIE_ALIAS_FAILURE_STAGE", stage)
            .env("LD_PRELOAD", &library)
            .output()
            .unwrap()
    });
    std::fs::remove_dir_all(directory).unwrap();
    for (stage, output) in outputs.iter().enumerate() {
        eprintln!("stage={} {output:?}", stage + 1);
    }
    assert!(outputs.iter().all(|output| output.status.success()));
    None
}

impl Fault {
    pub(crate) fn arm(&self) {
        // SAFETY: resolved from this process's retained test shim above.
        unsafe { (self.arm)(self.stage) };
    }

    pub(crate) fn assert_fired(&self) {
        // SAFETY: all five indices are valid in the retained fixture.
        let counts = std::array::from_fn::<_, 5, _>(|i| unsafe { (self.count)(i as i32) });
        let expected = if self.stage == 1 {
            [1, 0, 1, 0, 0]
        } else {
            [1, 2, 1, 1, 1]
        };
        assert_eq!(counts, expected);
        eprintln!("alias failure stage={} counters={counts:?}", self.stage);
    }
}
