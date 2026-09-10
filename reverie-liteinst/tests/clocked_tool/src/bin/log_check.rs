#[cfg(not(any(feature = "logged-inactive", feature = "logged-clocked")))]
fn main() {
    panic!("select a logged fixture profile");
}

#[cfg(any(feature = "logged-inactive", feature = "logged-clocked"))]
fn main() {
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "--v4") {
        v4::run();
        return;
    }
    selected::run();
}

#[cfg(any(feature = "logged-inactive", feature = "logged-clocked"))]
#[path = "log_check/v4.rs"]
mod v4;

#[cfg(any(feature = "logged-inactive", feature = "logged-clocked"))]
mod selected {
    use std::io::Read;
    use std::io::Seek;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use liteinst_clocked_tool_fixture::ClockTool;
    use liteinst_clocked_tool_fixture::logged;
    use reverie_rpc_transport::guest_log::Options;
    use reverie_rpc_transport::guest_log::RunState;
    use reverie_rpc_transport::guest_log::fixture::Control;
    use reverie_rpc_transport::guest_log::retained_log;

    pub fn run() {
        let mut args: Vec<_> = std::env::args_os().skip(1).collect();
        let inner = args.first().is_some_and(|arg| arg == "--inner");
        if inner {
            args.remove(0);
        }
        assert_eq!(
            args.len(),
            6,
            "log_check DSO GUEST captured|inherited success|cancel|rpc|quota WORK REPORT"
        );
        let inherited = match args[2].to_str().unwrap() {
            "captured" => false,
            "inherited" => true,
            _ => panic!("stdio mode"),
        };
        let case = match args[3].to_str().unwrap() {
            "success" => 0,
            "cancel" => 1,
            "rpc" => 2,
            "quota" => 3,
            _ => panic!("case"),
        };
        let work: u64 = args[4].to_str().unwrap().parse().unwrap();
        if !inner {
            let mut input = tempfile::tempfile().unwrap();
            input.write_all(b"IN\0\xfe!").unwrap();
            input.rewind().unwrap();
            let mut output = tempfile::tempfile().unwrap();
            let mut errors = tempfile::tempfile().unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--inner")
                .args(&args)
                .stdin(input)
                .stdout(output.try_clone().unwrap())
                .stderr(errors.try_clone().unwrap())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(75);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    break child.wait().unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            output.rewind().unwrap();
            output.read_to_end(&mut stdout).unwrap();
            errors.rewind().unwrap();
            errors.read_to_end(&mut stderr).unwrap();
            let mut stdout_path = args[5].clone();
            stdout_path.push(".outer.stdout");
            let mut stderr_path = args[5].clone();
            stderr_path.push(".outer.stderr");
            std::fs::write(stdout_path, &stdout).unwrap();
            std::fs::write(stderr_path, &stderr).unwrap();
            assert!(
                status.success(),
                "fixture checker failed: status={status} stdout={stdout:?} stderr={stderr:?}"
            );
            assert_eq!(
                stdout,
                if inherited && case == 0 {
                    b"OUT\0\xff".as_slice()
                } else {
                    b""
                }
            );
            if case == 0 {
                assert_eq!(
                    stderr,
                    if inherited {
                        b"ERR\0\xfe".as_slice()
                    } else {
                        b""
                    }
                );
            }
            println!(
                "logged fixture profile={} stdio={} case={} work={work}: qualified bounded case",
                args[0].to_string_lossy(),
                args[2].to_string_lossy(),
                args[3].to_string_lossy()
            );
            return;
        }
        let control = Arc::new(Control::new().unwrap());
        let release = control.release_on_drop();
        let (sink, handle) = retained_log(Options {
            byte_limit: if case == 3 { 64 } else { 100_000 },
            producers: 1,
            slots: 4,
        });
        let sink = sink.with_fixture_control(control.clone());
        let mut data = vec![0; 32];
        data[..8].copy_from_slice(b"LOGTEST1");
        data[8] = u8::from(logged::clocked());
        data[9] = case;
        data[12..16].copy_from_slice(&control.as_raw_fd().to_le_bytes());
        data[16..24].copy_from_slice(&work.to_le_bytes());
        let mut command = reverie::process::Command::new(&args[1]);
        let observer_fd = control.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(observer_fd, libc::F_SETFD, 0) < 0 {
                    return Err(reverie::syscalls::Errno::last());
                }
                Ok(())
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut ended_before_full = false;
        let mut timed_out = false;
        let mut drain_timed_out = false;
        let mut gate_prefix = None;
        let result = runtime.block_on(async {
            let (_, future) = super::prepare::<ClockTool>(
                command,
                (),
                &args[0],
                data,
                sink,
                if inherited {
                    reverie_liteinst::run_evidence::StdioMode::Inherited
                } else {
                    reverie_liteinst::run_evidence::StdioMode::Captured
                },
            );
            let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
                Box::pin(future);
            let result = tokio::time::timeout(Duration::from_secs(45), async {
                if case <= 1 {
                    tokio::select! {
                        result = &mut future => {
                            ended_before_full = true;
                            control.release();
                            return Some(result);
                        },
                        _ = async { loop {
                            let seen = control.observations();
                            if seen.gate_reached.load(Ordering::Acquire) != 0 && seen.wait_enter.load(Ordering::Acquire) > seen.gate_wait_baseline.load(Ordering::Acquire) { break; }
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        } } => {},
                    }
                    gate_prefix = Some(handle.snapshot().streams.first().map(|stream| stream.bytes.clone()));
                    if case == 1 {
                        drop(future);
                        handle.finished().await;
                        return None;
                    }
                }
                control.release();
                Some(future.await)
            }).await;
            timed_out = result.is_err();
            if timed_out {
                control.release();
                handle.stop(reverie_rpc_transport::guest_log::IssueKind::Cutoff, "fixture timeout");
                drain_timed_out = tokio::time::timeout(Duration::from_secs(5), handle.finished()).await.is_err();
            }
            result.ok().flatten()
        });
        drop(release);
        runtime.shutdown_timeout(Duration::from_secs(2));
        let report = handle.snapshot();
        if !inherited && let Some(result) = &result {
            let (stdout, stderr) = match result {
                Ok((output, _)) => (&output.stdout, &output.stderr),
                Err(error) => (&error.stdout, &error.stderr),
            };
            let mut stdout_path = args[5].clone();
            stdout_path.push(".guest.stdout");
            let mut stderr_path = args[5].clone();
            stderr_path.push(".guest.stderr");
            std::fs::write(stdout_path, stdout).unwrap();
            std::fs::write(stderr_path, stderr).unwrap();
        }
        std::fs::write(&args[5], format!("profile={} case={} stdio={} result={result:?}\nreport={report:?}\nclocks={:?}\nFull={} wait_enter={} wait_exit={} wait_result={} before={} after={} callbacks={} rpc={} verified={}\n", args[0].to_string_lossy(), args[3].to_string_lossy(), args[2].to_string_lossy(), control.observations().clocks.each_ref().map(|value| value.load(Ordering::Acquire)), control.observations().full.load(Ordering::Acquire), control.observations().wait_enter.load(Ordering::Acquire), control.observations().wait_exit.load(Ordering::Acquire), control.observations().result.load(Ordering::Acquire) as i64, control.observations().before.load(Ordering::Acquire), control.observations().after.load(Ordering::Acquire), control.observations().callbacks.load(Ordering::Acquire), logged::GLOBAL_RPCS.load(Ordering::Acquire), control.observations().verified.load(Ordering::Acquire))).unwrap();
        let seen = control.observations();
        let install_result = seen.install_result.load(Ordering::Acquire);
        let error_length = seen.install_error_len.load(Ordering::Acquire);
        let error_bytes: Vec<_> = seen
            .install_error
            .iter()
            .take(error_length as usize)
            .map(|byte| byte.load(Ordering::Acquire))
            .collect();
        let mut evidence = std::fs::OpenOptions::new()
            .append(true)
            .open(&args[5])
            .unwrap();
        writeln!(
            evidence,
            "guest_reaped={} guest_wait_status_raw={}",
            seen.guest_reaped.load(Ordering::Acquire),
            seen.guest_wait_status.load(Ordering::Acquire)
        )
        .unwrap();
        writeln!(evidence, "ended_before_full={ended_before_full} timed_out={timed_out} drain_timed_out={drain_timed_out} gate_prefix={gate_prefix:?}\nstartup={:?} signal_policy={:?} install_result={install_result} install_error_len={error_length} install_error_truncated={} install_error_bytes={error_bytes:?} install_error_text={:?}\ngate_reached={} gate_wait_baseline={} invalid={}", seen.startup.each_ref().map(|value| value.load(Ordering::Acquire)), seen.signal_policy.each_ref().map(|value| value.load(Ordering::Acquire)), seen.install_error_truncated.load(Ordering::Acquire), String::from_utf8_lossy(&error_bytes), seen.gate_reached.load(Ordering::Acquire), seen.gate_wait_baseline.load(Ordering::Acquire), seen.invalid.load(Ordering::Acquire)).unwrap();
        drop(evidence);
        assert!(
            !timed_out,
            "bounded logged fixture timeout; evidence retained"
        );
        assert!(
            !ended_before_full,
            "launch ended before Full/wait; evidence retained"
        );
        if case <= 1 {
            assert_eq!(gate_prefix, Some(Some(logged::PREFIX.to_vec())));
        }
        if case == 1 {
            assert_eq!(report.run, RunState::Cancelled);
        }
        assert_eq!(install_result, 1);
        assert_eq!(
            seen.signal_policy[0].load(Ordering::Acquire),
            u64::from(logged::clocked())
        );
        assert_eq!(seen.signal_policy[1].load(Ordering::Acquire), 0);
        assert_eq!(seen.signal_policy[3].load(Ordering::Acquire), 0);
        assert!(report.terminal());
        assert!(
            report.streams[0].bytes.starts_with(logged::PREFIX),
            "{report:?}"
        );
        assert_eq!(control.observations().invalid.load(Ordering::Acquire), 0);
        for stage in &control.observations().startup {
            let bits = stage.load(Ordering::Acquire);
            assert_ne!(bits & 16, 0);
            if !logged::clocked() {
                assert_eq!(bits & 3, 0);
            }
        }
        if case == 0 {
            let (process, _) = result.unwrap().unwrap();
            assert!(process.status.success(), "{process:?}");
            assert!(report.qualifies(), "{report:?}");
            let mut expected = logged::PREFIX.to_vec();
            expected.extend(logged::BURST);
            for index in 2..logged::SAMPLES {
                expected.extend([0, index as u8, 0xff]);
            }
            assert_eq!(report.streams[0].bytes, expected);
            assert!(report.streams[0].finished);
            assert_eq!(report.streams[0].complete_records, logged::SAMPLES as u64);
            assert!(report.streams[0].fragment.is_empty());
            assert_eq!(
                control.observations().callbacks.load(Ordering::Acquire),
                logged::SAMPLES as u64
            );
            assert_eq!(
                logged::GLOBAL_RPCS.load(Ordering::Acquire),
                logged::SAMPLES as u64
            );
            assert_eq!(control.observations().verified.load(Ordering::Acquire), 1);
            if logged::clocked() {
                let clocks = control
                    .observations()
                    .clocks
                    .each_ref()
                    .map(|value| value.load(Ordering::Acquire));
                assert_eq!(clocks, [7, 8, 9, 10, 17, 18, 21, 21]);
            }
            if inherited {
                assert!(process.stdout.is_empty() && process.stderr.is_empty());
            } else {
                assert_eq!(process.stdout, b"OUT\0\xff");
                assert_eq!(process.stderr, b"ERR\0\xfe");
            }
            let seen = control.observations();
            assert!(seen.full.load(Ordering::Acquire) > 0);
            assert_eq!(
                seen.wait_enter.load(Ordering::Acquire),
                seen.wait_exit.load(Ordering::Acquire)
            );
            for bits in [&seen.before, &seen.after] {
                let bits = bits.load(Ordering::Acquire);
                if logged::clocked() {
                    assert_eq!(bits & 14, 14);
                } else {
                    assert_eq!(bits & 3, 0);
                }
            }
        } else {
            assert!(!report.qualifies());
            assert_eq!(report.streams[0].bytes, logged::PREFIX);
            assert!(!report.streams[0].finished);
            if case != 1 {
                assert!(result.unwrap().is_err());
            }
            if case == 2 {
                assert!(
                    !report.rpc_issues.is_empty(),
                    "original RPC failure missing"
                );
            }
            if case == 3 {
                assert!(
                    report.issues.iter().any(|issue| issue.kind
                        == reverie_rpc_transport::guest_log::IssueKind::Truncated)
                );
            }
        }
    }
}

#[cfg(any(feature = "logged-inactive", feature = "logged-clocked"))]
fn prepare<T: reverie::Tool + 'static>(
    command: reverie::process::Command,
    config: <T::GlobalState as reverie::GlobalTool>::Config,
    preload: &std::ffi::OsStr,
    data: Vec<u8>,
    sink: reverie_rpc_transport::guest_log::LogSink,
    mode: reverie_liteinst::run_evidence::StdioMode,
) -> (
    reverie_liteinst::run_evidence::RunObserver,
    impl std::future::Future<
        Output = Result<
            (std::process::Output, T::GlobalState),
            reverie_liteinst::LoggedRunError,
        >,
    >,
) {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let preload = std::fs::File::open(preload).expect("open owned preload");
    let fd = preload.as_raw_fd();
    let mut command = command.try_into_std().expect("convert owned command");
    command.env("LD_PRELOAD", format!("/proc/self/fd/{fd}"));
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let prepared = reverie_liteinst::PreparedCommand::new(command, preload);
    reverie_liteinst::LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<T, _>(
        prepared, config, data, sink, mode,
    )
}
