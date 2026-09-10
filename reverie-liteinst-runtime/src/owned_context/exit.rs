use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Binding {
    pub(super) owner: i64,
    pub(super) generation: u64,
    pub(super) number: i64,
    pub(super) site: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Request {
    binding: Binding,
    number: i64,
    args: [u64; 6],
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Returned(i64),
    Exit(Request),
}

pub(crate) fn operation(number: i64) -> bool {
    matches!(number, libc::SYS_exit | libc::SYS_exit_group)
}

pub(crate) fn authorize(
    binding: Option<Binding>,
    root: i64,
    tid: i64,
    pid: i64,
    states: usize,
    number: i64,
    args: [u64; 6],
) -> io::Result<Request> {
    let binding = binding.ok_or_else(super::unsupported)?;
    if !operation(number)
        || binding.owner <= 0
        || binding.generation == 0
        || binding.site == 0
        || root != binding.owner
        || tid != binding.owner
        || pid != root
        || states != 1
    {
        return Err(super::unsupported());
    }
    Ok(Request {
        binding,
        number,
        args,
    })
}

impl Request {
    pub(super) fn matches(&self, binding: Binding) -> bool {
        self.binding == binding && operation(self.number)
    }

    pub(super) fn terminate(self) -> ! {
        crate::guest_log::finish();
        let result = unsafe { reverie_preload::trap::raw_syscall6(self.number, self.args) };
        failed("owned-exit/kernel-return", Some(result))
    }
}

pub(crate) fn failed(stage: &'static str, result: Option<i64>) -> ! {
    crate::guest_log::mark_failed();
    unsafe { reverie_preload::trap::terminal126(stage, "terminal-exit", result) }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    use super::*;

    fn binding() -> Binding {
        Binding {
            owner: 5,
            generation: 7,
            number: libc::SYS_exit_group,
            site: 0x4000,
        }
    }

    #[test]
    fn exit_requests_retain_kernel_operation_and_all_argument_bits() {
        for number in [libc::SYS_exit, libc::SYS_exit_group] {
            for status in [
                0,
                1,
                119,
                125,
                126,
                130,
                255,
                256,
                u64::MAX,
                0x9876_5432_0000_0007,
            ] {
                let args = [status, 11, 22, 33, 44, 55];
                let request = authorize(Some(binding()), 5, 5, 5, 1, number, args).unwrap();
                assert_eq!(request.number, number);
                assert_eq!(request.args, args);
                assert!(request.matches(binding()));
            }
        }
    }

    #[test]
    fn exit_requests_require_owned_binding_and_sole_root_tool_state() {
        for (root, tid, pid, states) in [
            (6, 5, 5, 1),
            (5, 6, 5, 1),
            (5, 5, 6, 1),
            (5, 5, 5, 0),
            (5, 5, 5, 2),
        ] {
            assert!(
                authorize(
                    Some(binding()),
                    root,
                    tid,
                    pid,
                    states,
                    libc::SYS_exit,
                    [0; 6]
                )
                .is_err()
            );
        }
        assert!(authorize(None, 5, 5, 5, 1, libc::SYS_exit_group, [0; 6]).is_err());
        for invalid in [
            Binding {
                owner: 0,
                ..binding()
            },
            Binding {
                generation: 0,
                ..binding()
            },
            Binding {
                site: 0,
                ..binding()
            },
        ] {
            assert!(authorize(Some(invalid), 5, 5, 5, 1, libc::SYS_exit, [0; 6]).is_err());
        }
        assert!(authorize(Some(binding()), 5, 5, 5, 1, libc::SYS_write, [0; 6]).is_err());
    }

    #[test]
    fn exit_consumption_rejects_every_stale_binding_field() {
        let request = authorize(Some(binding()), 5, 5, 5, 1, libc::SYS_exit, [0; 6]).unwrap();
        for other in [
            Binding {
                owner: 6,
                ..binding()
            },
            Binding {
                generation: 8,
                ..binding()
            },
            Binding {
                number: libc::SYS_getpid,
                ..binding()
            },
            Binding {
                site: 0x4002,
                ..binding()
            },
        ] {
            assert!(!request.matches(other));
        }
    }

    #[test]
    fn exit_finish_and_late_failure_use_real_buffer_and_actual_wait_status() {
        const KEY: &str = "LITEINST_EXIT_LOG_CONTROL";
        if let Ok(control) = std::env::var(KEY) {
            let (descriptor, mode) = control.split_once(':').unwrap();
            let descriptor: i32 = descriptor.parse().unwrap();
            let identity = crate::guest_log::identity(descriptor).unwrap();
            let log = unsafe { crate::guest_log::from_ordered_identity(&identity) }.unwrap();
            let mut writer = unsafe { log.install() }.unwrap();
            writer.write_record(b"callbacks-complete\n").unwrap();
            match mode {
                "missing-finish" => unsafe {
                    reverie_preload::trap::raw_syscall6(libc::SYS_exit_group, [0; 6]);
                },
                "early-failure" => failed("owned-exit/test-before-finish", None),
                "late-failure" => {
                    crate::guest_log::finish();
                    assert!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) } >= 0);
                    failed("owned-exit/test-after-finish", Some(-22));
                }
                "kernel-return" => {
                    let mut request =
                        authorize(Some(binding()), 5, 5, 5, 1, libc::SYS_exit_group, [0; 6])
                            .unwrap();
                    request.number = libc::SYS_getpid;
                    request.terminate();
                }
                status => {
                    let mut args = [0; 6];
                    args[0] = status.parse().unwrap();
                    authorize(Some(binding()), 5, 5, 5, 1, libc::SYS_exit_group, args)
                        .unwrap()
                        .terminate();
                }
            }
            panic!("raw exit returned");
        }
        use reverie_rpc_transport::guest_log::ordered;
        for (mode, status, complete) in [
            ("0", 0, true),
            ("125", 125, true),
            ("130", 130, true),
            ("263", 7, true),
            ("missing-finish", 0, false),
            ("early-failure", 126, false),
            ("late-failure", 126, false),
            ("kernel-return", 126, false),
        ] {
            let (host, guest) = ordered::channel_pair(ordered::Limits {
                producers: 2,
                slots: 8,
                max_record_bytes: 1024,
                host_pending_bytes: 4096,
                guest_pending_bytes: 4096,
                pending_records: 8,
            })
            .unwrap();
            let buffer = ordered::Buffer::receive(host.as_raw_fd()).unwrap();
            let mut collector = buffer.collector().unwrap();
            let descriptor = guest.as_raw_fd();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "owned_context::exit::tests::exit_finish_and_late_failure_use_real_buffer_and_actual_wait_status", "--nocapture"])
                .env(KEY, format!("{descriptor}:{mode}"));
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(descriptor, libc::F_GETFD);
                    if flags == -1
                        || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let child = command.spawn().unwrap();
            let pid = child.id();
            drop(guest);
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(status),
                "{mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut bytes = Vec::new();
            while let Some(record) = collector.poll().unwrap() {
                bytes.extend_from_slice(record.bytes());
                record.release().unwrap();
            }
            assert_eq!(bytes, b"callbacks-complete\n", "{mode}");
            assert_eq!(collector.guest_complete(), complete, "{mode}");
            let mut byte = 0u8;
            assert_eq!(
                unsafe {
                    libc::recv(
                        host.as_raw_fd(),
                        (&raw mut byte).cast(),
                        1,
                        libc::MSG_DONTWAIT,
                    )
                },
                0,
                "endpoint remained open: {mode}"
            );
            if mode == "kernel-return" {
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert!(stderr.contains("owned-exit/kernel-return"), "{stderr}");
                assert!(stderr.contains(&format!("{pid}")), "{stderr}");
            }
        }
    }
}
