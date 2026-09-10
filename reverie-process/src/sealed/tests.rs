use std::os::unix::fs::FileExt;

use super::*;

#[test]
fn immutable_bytes_and_parent_cloexec() {
    let descriptor = create(c"sealed-host-control", b"stable image", 3).unwrap();
    let raw = descriptor.as_raw_fd();
    assert!(raw > 2);
    assert_eq!(
        unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
        libc::FD_CLOEXEC
    );
    assert_eq!(
        unsafe { libc::fcntl(raw, libc::F_GET_SEALS) },
        libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE
    );
    let file = File::from(descriptor);
    let mut bytes = [0; 12];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"stable image");
    assert_eq!(
        file.write_at(b"X", 0).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        file.set_len(0).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
}

#[test]
fn failure_and_stdio_floors_preserve_owned_descriptor_cleanup() {
    const CHILD: &str = "REVERIE_SEALED_FILE_HOST_CONTROL";
    if let Ok(mode) = std::env::var(CHILD) {
        if mode == "floor" {
            assert_eq!(unsafe { libc::close(0) }, 0);
            let raised = create(c"raised", b"bytes", 3).unwrap();
            assert!(raised.as_raw_fd() > 2);
            assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFD) }, -1);
            drop(raised);
            let legacy = create(c"legacy-floor", b"bytes", 0).unwrap();
            assert_eq!(legacy.as_raw_fd(), 0);
            return;
        }
        let count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = count();
        assert!(create(c"negative", b"bytes", -1).is_err());
        assert!(create(c"dup-fails", b"bytes", i32::MAX).is_err());
        assert_eq!(count(), before);
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        let blocked = libc::rlimit {
            rlim_cur: 0,
            rlim_max: limit.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &blocked) }, 0);
        let error = create(c"no-fds", b"bytes", 3).unwrap_err();
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
        assert_eq!(count(), before);
        return;
    }
    for mode in ["floor", "limits"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "sealed::tests::failure_and_stdio_floors_preserve_owned_descriptor_cleanup",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD, mode)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
