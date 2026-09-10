use super::*;

static UNAVAILABLE_TLS: Control = Control {
    private_fs: AtomicU64::new(0),
    owner: AtomicU64::new(0),
    guest_fs: AtomicU64::new(0),
    deferred: AtomicU64::new(0),
    phase: AtomicU64::new(0),
    private_gs: AtomicU64::new(0),
    guest_gs: AtomicU64::new(0),
};

extern "C" fn unavailable_arch_prctl(_: u64, _: u64, _: *mut Observation) {
    eprintln!("private-crt unit test invoked unavailable native arch_prctl");
    std::process::abort();
}

std::arch::global_asm!(
    ".weak pl_tls",
    ".set pl_tls, {control}",
    ".weak pl_guest_arch_prctl",
    ".set pl_guest_arch_prctl, {arch_prctl}",
    control = sym UNAVAILABLE_TLS,
    arch_prctl = sym unavailable_arch_prctl,
);

#[test]
fn unit_support_has_no_tls_owner_or_provider_access() {
    assert!(available());
    assert!(!installed());
    let before = state().unwrap();
    assert_eq!(before.private, [0; 2]);
    assert_eq!(before.guest, [0; 2]);
    assert_eq!((before.owner, before.phase, before.deferred), (0, 0, 0));
    for operation in 0x1001..=0x1004 {
        assert_eq!(
            execute_with(before, 51, super::super::RUNTIME, operation, 0x1000, || {
                panic!("unavailable TLS reached provider")
            }),
            Err("TLS owner/continuation unavailable")
        );
    }
    assert_eq!(state().unwrap(), before);
}

#[test]
fn unit_support_unexpected_arch_prctl_aborts() {
    use std::os::unix::process::ExitStatusExt;

    const CHILD: &str = "REVERIE_PRIVATE_UNIT_ARCH_PRCTL_ABORT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        unsafe { pl_guest_arch_prctl.unwrap()(0x1002, 0, std::ptr::null_mut()) };
        panic!("unavailable native arch_prctl returned");
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "owned_context::tls::tests::unit_support_unexpected_arch_prctl_aborts",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGABRT));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("private-crt unit test invoked unavailable native arch_prctl")
    );
}

fn control(state: State) -> Control {
    Control {
        private_fs: AtomicU64::new(state.private[0]),
        private_gs: AtomicU64::new(state.private[1]),
        guest_fs: AtomicU64::new(state.guest[0]),
        guest_gs: AtomicU64::new(state.guest[1]),
        owner: AtomicU64::new(state.owner),
        phase: AtomicU64::new(state.phase),
        deferred: AtomicU64::new(state.deferred),
    }
}

#[test]
fn retained_return_owner_changes_only_after_verified_kernel_success() {
    for (operation, index) in [(0x1002, 0), (0x1001, 1)] {
        let before = initial();
        let control = control(before);
        let mut after = before;
        after.guest[index] = 0x2000;
        assert_eq!(
            publish(
                &control,
                before,
                operation,
                0x2000,
                Observation {
                    result: 0,
                    guest: after.guest
                }
            ),
            Ok(0)
        );
        assert_eq!(snapshot(&control), after);
        assert!(
            publish(
                &control,
                before,
                operation,
                0x2000,
                Observation {
                    result: 0,
                    guest: after.guest
                }
            )
            .is_err()
        );
        assert_eq!(snapshot(&control), after);
    }
}

#[test]
fn failed_and_inconsistent_results_never_change_retained_owner() {
    let before = initial();
    let control = control(before);
    for operation in 0x1001..=0x1004 {
        assert_eq!(
            publish(
                &control,
                before,
                operation,
                0x2000,
                Observation {
                    result: -i64::from(libc::EFAULT),
                    guest: before.guest
                }
            ),
            Ok(-i64::from(libc::EFAULT))
        );
        assert_eq!(snapshot(&control), before);
        assert!(
            publish(
                &control,
                before,
                operation,
                0x2000,
                Observation {
                    result: -i64::from(libc::EFAULT),
                    guest: [0x2000, 0x2000]
                }
            )
            .is_err()
        );
        assert_eq!(snapshot(&control), before);
    }
}

fn initial() -> State {
    State {
        private: [0x9000, 0xa000],
        guest: [0, 0],
        owner: 51,
        phase: 2,
        deferred: 1,
    }
}

#[test]
fn set_success_changes_only_selected_guest_base() {
    for (operation, index) in [(0x1001, 1), (0x1002, 0)] {
        for argument in [0, 0x1000, 0x2000, 0x4000, 0xa000] {
            let before = initial();
            let mut guest = before.guest;
            guest[index] = argument;
            let observed = execute_with(
                before,
                51,
                super::super::RUNTIME,
                operation,
                argument,
                || Observation { result: 0, guest },
            )
            .unwrap();
            assert_eq!(observed.guest, guest);
            assert_eq!(before.private, [0x9000, 0xa000]);
            let mut wrong = before.guest;
            wrong[index] = argument ^ 1;
            assert!(
                completion(
                    before,
                    operation,
                    argument,
                    Observation {
                        result: 0,
                        guest: wrong
                    }
                )
                .is_err()
            );
        }
    }
}

#[test]
fn kernel_failures_leave_guest_bases_and_exact_errno_unchanged() {
    for operation in 0x1001..=0x1004 {
        for result in [
            -i64::from(libc::EPERM),
            -i64::from(libc::EFAULT),
            -i64::from(libc::EINTR),
            -4095,
        ] {
            let expected = Observation {
                result,
                guest: initial().guest,
            };
            assert_eq!(
                execute_with(
                    initial(),
                    51,
                    super::super::RUNTIME,
                    operation,
                    0x1000,
                    || expected
                ),
                Ok(expected)
            );
            assert!(
                completion(
                    initial(),
                    operation,
                    0x1000,
                    Observation {
                        result,
                        guest: [0x1000, 0]
                    }
                )
                .is_err()
            );
        }
    }
}

#[test]
fn get_uses_kernel_output_without_adopting_private_bases() {
    for operation in [0x1003, 0x1004] {
        let mut before = initial();
        before.guest = [0x1100, 0x1200];
        assert_eq!(
            execute_with(before, 51, super::super::RUNTIME, operation, 0x17fc, || {
                Observation {
                    result: 0,
                    guest: before.guest,
                }
            })
            .unwrap()
            .guest,
            before.guest
        );
        assert!(
            completion(
                before,
                operation,
                0x17fc,
                Observation {
                    result: 0,
                    guest: before.private
                }
            )
            .is_err()
        );
    }
}

#[test]
fn ownership_and_lifecycle_refuse_before_provider() {
    let mut cases = vec![
        (initial(), 52, super::super::RUNTIME),
        (initial(), -1, super::super::RUNTIME),
    ];
    for phase in [
        super::super::IDLE,
        super::super::CAPTURED,
        super::super::RETURNING,
        super::super::FAILED,
    ] {
        cases.push((initial(), 51, phase));
    }
    for field in 0..5 {
        let mut changed = initial();
        match field {
            0 => changed.phase = 0,
            1 => changed.deferred = 0,
            2 => changed.owner = 0,
            3 => changed.private[0] = 0,
            _ => changed.guest[0] = changed.private[0],
        }
        cases.push((changed, 51, super::super::RUNTIME));
    }
    for (state, tid, phase) in cases {
        assert!(
            execute_with(state, tid, phase, 0x1002, 0x1000, || panic!(
                "provider must not run"
            ))
            .is_err()
        );
    }
}

#[test]
fn unsupported_modes_never_reach_kernel() {
    for operation in [0, 0x1011, 0x1012, 0x1022, 0x1023, 0x3001, u64::MAX] {
        assert!(
            execute_with(
                initial(),
                51,
                super::super::RUNTIME,
                operation,
                0x1000,
                || panic!("unsupported operation reached kernel")
            )
            .is_err()
        );
    }
}

#[test]
fn only_private_fs_collision_is_refused_before_kernel_base_validation() {
    for operation in [0x1001, 0x1002] {
        for address in [0, 0x1000, 0x4000, 0xa000, 1 << 47, u64::MAX] {
            assert!(request(initial(), operation, address).is_ok());
        }
    }
    assert!(request(initial(), 0x1001, 0x9000).is_ok());
    assert_eq!(
        request(initial(), 0x1002, 0x9000),
        Err("guest/private FS identity collision")
    );
    assert!(
        execute_with(
            initial(),
            51,
            super::super::RUNTIME,
            0x1002,
            0x9000,
            || panic!("private FS collision reached provider")
        )
        .is_err()
    );
}

#[test]
fn get_fault_results_are_not_replaced_by_userspace_mappedness_checks() {
    for operation in [0x1003, 0x1004] {
        for address in [0, 0xfff, 0x1ff9, 0x2000, u64::MAX - 3] {
            let observed = Observation {
                result: -i64::from(libc::EFAULT),
                guest: initial().guest,
            };
            assert_eq!(
                execute_with(
                    initial(),
                    51,
                    super::super::RUNTIME,
                    operation,
                    address,
                    || observed
                ),
                Ok(observed)
            );
        }
    }
}

#[test]
fn unexplained_success_or_error_does_not_publish_state() {
    for result in [1, 42, -4096, i64::MIN] {
        assert!(
            completion(
                initial(),
                0x1002,
                0x1000,
                Observation {
                    result,
                    guest: [0x1000, 0]
                }
            )
            .is_err()
        );
    }
    assert!(
        completion(
            initial(),
            0x1002,
            0x1000,
            Observation {
                result: 0,
                guest: [0x1000, 0x2000]
            }
        )
        .is_err()
    );
}
