use super::Errno;
use super::OwnedInjection;
use super::classify_owned_injection;
use super::guarded_raw_injection;
use super::injected_syscall_guard;

const HEAD_LEN: u64 = 24;

fn isolated(name: &str) -> bool {
    const KEY: &str = "REVERIE_LITEINST_ROBUST_TEST";
    if std::env::var(KEY).as_deref() == Ok(name) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("tool_host::robust_list_tests::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(KEY, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    false
}

fn query_registration() -> Result<(u64, u64), i32> {
    let mut head = 0x1234_5678_u64;
    let mut length = 0x2345_6789_u64;
    let result = unsafe {
        libc::syscall(
            libc::SYS_get_robust_list,
            0,
            &mut head as *mut u64,
            &mut length as *mut u64,
        )
    };
    if result != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap();
        assert_eq!(result, -1);
        assert_eq!(head, 0x1234_5678);
        assert_eq!(length, 0x2345_6789);
        return Err(errno);
    }
    assert_eq!(length, HEAD_LEN);
    Ok((head, length))
}

fn registration() -> (u64, u64) {
    query_registration().expect("self get_robust_list must return the ordinary saved registration")
}

struct RestoreRegistration((u64, u64));

impl Drop for RestoreRegistration {
    fn drop(&mut self) {
        if guarded_raw_injection(libc::SYS_set_robust_list, [self.0.0, self.0.1, 0, 0, 0, 0]) != 0 {
            std::process::abort();
        }
    }
}

#[test]
fn registration_replacement_errors_and_exact_pointer() {
    if !isolated("registration_replacement_errors_and_exact_pointer") {
        return;
    }
    let original = registration();
    let first = [0x1234_5678_u64; 3];
    let second = [0x2345_6789_u64; 3];
    static READ_ONLY: [u64; 3] = [0x3456_789a; 3];
    {
        let _restore = RestoreRegistration(original);
        for head in [
            first.as_ptr() as u64,
            second.as_ptr() as u64,
            first.as_ptr() as u64 + 1,
            READ_ONLY.as_ptr() as u64,
            1,
            1_u64 << 63,
            u64::MAX - 4095,
            u64::MAX - 4094,
            u64::MAX,
            0,
        ] {
            let expected_query = if head >= u64::MAX - 4094 {
                Err(-(head as i64) as i32)
            } else {
                Ok((head, HEAD_LEN))
            };
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_set_robust_list, head, HEAD_LEN) },
                0,
                "raw Linux setter head={head:#x}"
            );
            assert_eq!(
                query_registration(),
                expected_query,
                "raw Linux head={head:#x}"
            );
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_set_robust_list, original.0, original.1) },
                0
            );
            assert_eq!(registration(), original);
            let args = [head, HEAD_LEN, 0, 0, 0, 0];
            assert_eq!(
                injected_syscall_guard(libc::SYS_set_robust_list, args),
                None
            );
            assert_eq!(guarded_raw_injection(libc::SYS_set_robust_list, args), 0);
            assert_eq!(query_registration(), expected_query, "head={head:#x}");
            for length in [0, 12, 23, 25, (1_u64 << 32) | HEAD_LEN, u64::MAX] {
                for rejected_head in [0, second.as_ptr() as u64, u64::MAX] {
                    let result = guarded_raw_injection(
                        libc::SYS_set_robust_list,
                        [rejected_head, length, 0, 0, 0, 0],
                    );
                    assert_eq!(result, -i64::from(libc::EINVAL));
                    assert_eq!(Errno::from_ret(result as usize), Err(Errno::EINVAL));
                    assert_eq!(query_registration(), expected_query, "head={head:#x}");
                }
            }
            assert_eq!(unsafe { std::ptr::read_volatile(&first) }, [0x1234_5678; 3]);
            assert_eq!(
                unsafe { std::ptr::read_volatile(&second) },
                [0x2345_6789; 3]
            );
            assert_eq!(
                unsafe { std::ptr::read_volatile(&READ_ONLY) },
                [0x3456_789a; 3]
            );
        }
    }
    assert_eq!(registration(), original);
}

#[test]
fn registration_restores_on_host_unwind() {
    if !isolated("registration_restores_on_host_unwind") {
        return;
    }
    let original = registration();
    let unwind = std::panic::catch_unwind(|| {
        let _restore = RestoreRegistration(original);
        assert_eq!(
            guarded_raw_injection(libc::SYS_set_robust_list, [0, HEAD_LEN, 0, 0, 0, 0]),
            0
        );
        assert_eq!(registration(), (0, HEAD_LEN));
        std::panic::panic_any("controlled ordinary-host registration unwind");
    });
    let payload = unwind.expect_err("controlled unwind must occur");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"controlled ordinary-host registration unwind")
    );
    assert_eq!(registration(), original);
}

#[test]
fn admission_is_registration_only() {
    assert_eq!(
        classify_owned_injection(true, libc::SYS_set_robust_list),
        OwnedInjection::Admitted
    );
    assert!(crate::syscall_event::observable(libc::SYS_set_robust_list));
    assert!(!crate::syscall_event::backed_returning(
        libc::SYS_set_robust_list
    ));
    assert!(!crate::mapping::operation(libc::SYS_set_robust_list));
    for number in [
        libc::SYS_set_robust_list | 0x4000_0000,
        libc::SYS_get_robust_list,
        libc::SYS_futex,
        libc::SYS_clone,
        libc::SYS_execve,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_rt_sigreturn,
    ] {
        assert_eq!(
            classify_owned_injection(true, number),
            OwnedInjection::Terminal
        );
    }
}
