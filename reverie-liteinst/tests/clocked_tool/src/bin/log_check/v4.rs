use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use liteinst_clocked_tool_fixture::ClockTool;
use liteinst_clocked_tool_fixture::logged;
use logged::v4;
use reverie_liteinst::LiteinstBackend;
use reverie_rpc_transport::guest_log::ArtifactStability;
use reverie_rpc_transport::guest_log::CaptureDestination;
use reverie_rpc_transport::guest_log::CaptureLimits;
use reverie_rpc_transport::guest_log::CaptureOptions;
use reverie_rpc_transport::guest_log::CaptureTimeouts;
use reverie_rpc_transport::guest_log::DestinationProgress;
use reverie_rpc_transport::guest_log::GuestStopReason;
use reverie_rpc_transport::guest_log::Phase;
use reverie_rpc_transport::guest_log::RunState;
use reverie_rpc_transport::guest_log::fixture::Control;
use reverie_rpc_transport::guest_log::prepared_capture;

#[path = "v4/evidence.rs"]
mod evidence;

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn entered(&self) -> bool {
        self.state.lock().unwrap().0
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(50), |state| !state.1)
            .unwrap();
        assert!(
            !timeout.timed_out() && state.1,
            "fixture destination gate deadline"
        );
    }
}

struct Output {
    bytes: Arc<Mutex<Vec<u8>>>,
    progress: DestinationProgress,
    gate: Option<Arc<Gate>>,
    fail: bool,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        self.progress.acknowledged_data_bytes += bytes.len() as u64;
        if bytes == logged::PREFIX {
            if let Some(gate) = &self.gate {
                gate.wait();
            }
            if self.fail {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "V4 fixture failure after acknowledged prefix",
                ));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl CaptureDestination for Output {
    fn progress(&self) -> DestinationProgress {
        self.progress
    }
}

fn concatenate(records: &[(bool, Vec<u8>)]) -> Vec<u8> {
    records.iter().flat_map(|record| record.1.clone()).collect()
}

async fn until(mut condition: impl FnMut() -> bool) {
    while !condition() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

pub fn run() {
    let mut args: Vec<_> = std::env::args_os().skip(2).collect();
    let inner = args.first().is_some_and(|arg| arg == "--inner");
    if inner {
        args.remove(0);
    }
    assert_eq!(
        args.len(),
        7,
        "--v4 DSO GUEST captured|inherited success|cancel|rpc|quota|record-failed|sink-error slot|credit WORK REPORT"
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
        "record-failed" => 4,
        "sink-error" => 5,
        _ => panic!("case"),
    };
    let credit = match args[4].to_str().unwrap() {
        "slot" => false,
        "credit" => true,
        _ => panic!("pressure"),
    };
    assert!(case != 5 || credit, "sink-error uses the held-credit gate");
    let work: u64 = args[5].to_str().unwrap().parse().unwrap();
    if !inner {
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(b"IN\0\xfe!").unwrap();
        input.rewind().unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut errors = tempfile::tempfile().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--v4", "--inner"])
            .args(&args)
            .stdin(input)
            .stdout(output.try_clone().unwrap())
            .stderr(errors.try_clone().unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(75);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
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
        for (suffix, bytes) in [(".outer.stdout", &stdout), (".outer.stderr", &stderr)] {
            let mut path = args[6].clone();
            path.push(suffix);
            std::fs::write(path, bytes).unwrap();
        }
        assert!(
            status.success(),
            "V4 checker status={status} stdout={stdout:?} stderr={stderr:?}"
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
            "V4 fixture profile={} stdio={} case={} pressure={} work={work}; see retained scope and observations",
            args[0].to_string_lossy(),
            args[2].to_string_lossy(),
            args[3].to_string_lossy(),
            args[4].to_string_lossy()
        );
        return;
    }
    let control = Arc::new(Control::new().unwrap());
    let release = control.release_on_drop();
    let pressure_case = matches!(case, 0 | 1 | 5);
    control.observations().v4_pressure.store(
        if pressure_case {
            if credit { 2 } else { 1 }
        } else {
            0
        },
        Ordering::Release,
    );
    let gate = Arc::new(Gate::default());
    let output = Arc::new(Mutex::new(Vec::new()));
    let limits = CaptureLimits {
        producers: 2,
        slots_per_producer: 4,
        max_record_bytes: if case == 3 { 64 } else { logged::BURST.len() },
        host_pending_bytes: 100_000,
        guest_pending_bytes: 100_000,
        pending_records: if credit { 2 } else { 32 },
        diagnostic_bytes: 100_000,
    };
    let options = CaptureOptions {
        limits,
        timeouts: CaptureTimeouts {
            startup: Duration::from_secs(5),
            blocked_publication: Duration::from_secs(50),
            final_drain: Duration::from_secs(2),
        },
    };
    let (mut owner, sink, producer) = prepared_capture(
        options,
        Output {
            bytes: output.clone(),
            progress: DestinationProgress::default(),
            gate: (credit && pressure_case).then(|| gate.clone()),
            fail: case == 5,
        },
    )
    .unwrap();
    let handle = owner.handle();
    v4::configure(producer, handle.clone(), control.clone());
    let sink = sink.with_fixture_control(control.clone());
    let mut data = vec![0; 32];
    data[..8].copy_from_slice(b"LOGTEST4");
    data[8] = u8::from(logged::clocked());
    data[9] = case;
    data[10] = if credit { 2 } else { 1 };
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
    let (observer, future): (_, std::pin::Pin<Box<dyn std::future::Future<Output = _>>>) =
        if inherited {
            let (observer, future) =
                LiteinstBackend::prepare_with_inherited_stdio_and_preload_data_and_log_sink::<
                    ClockTool,
                >(command, (), &args[0], data, sink);
            (observer, Box::pin(future))
        } else {
            let (observer, future) =
                LiteinstBackend::prepare_with_output_and_preload_data_and_log_sink::<ClockTool>(
                    command,
                    (),
                    &args[0],
                    data,
                    sink,
                );
            (observer, Box::pin(future))
        };
    let prepared_evidence = observer.try_snapshot();
    let mut cancellation_evidence = None;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let expected = v4::records::expected(logged::PREFIX, &logged::BURST, logged::clocked(), case);
    let first_four = concatenate(&expected[..4]);
    let mut ended_before_gate = false;
    let mut timed_out = false;
    let mut drain_timed_out = false;
    let result = runtime.block_on(async {
        let mut future = future;
        let bounded = tokio::time::timeout(Duration::from_secs(45), async {
            if pressure_case || case == 3 {
                tokio::select! {
                    result = &mut future => { ended_before_gate = true; return Some(result); }
                    _ = until(|| {
                        let seen = control.observations();
                        seen.v4_gate.load(Ordering::Acquire) == if case == 3 { 2 } else { 1 }
                            && seen.v4_waits.load(Ordering::Acquire) > 0
                            && if credit && pressure_case { gate.entered() && handle.capture_snapshot().unwrap().publication.diagnostic_prefix == first_four } else { *output.lock().unwrap() == first_four }
                    }) => {}
                }
                if case == 1 {
                    until(|| observer.try_snapshot().is_ok_and(|snapshot| evidence::before_cancel(&snapshot, inherited).is_ok())).await;
                    cancellation_evidence = Some(observer.try_snapshot());
                    drop(future);
                    gate.release();
                    handle.guest_finished().await;
                    until(|| v4::DROPS.load(Ordering::Acquire) == 1).await;
                    return None;
                }
                if case == 5 {
                    gate.release();
                    let result = future.await;
                    return Some(result);
                }
                gate.release();
                control.release();
            }
            Some(future.await)
        }).await;
        timed_out = bounded.is_err();
        if timed_out {
            handle.request_guest_stop(GuestStopReason::Cancelled);
            gate.release(); control.release();
            drain_timed_out = tokio::time::timeout(Duration::from_secs(5), handle.guest_finished()).await.is_err();
        }
        bounded.ok().flatten()
    });
    gate.release();
    control.release();
    drop(release);
    let before_cleanup = handle.capture_snapshot().unwrap();
    let result_text = format!("{result:?}");
    let returned_stdio = result.as_ref().map(|result| match result {
        Ok((output, _)) => (output.stdout.clone(), output.stderr.clone()),
        Err(error) => (error.stdout.clone(), error.stderr.clone()),
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let run_evidence = observer.try_snapshot();
    let stdio = run_evidence
        .as_ref()
        .ok()
        .map(|snapshot| (snapshot.stdout.bytes(), snapshot.stderr.bytes()));
    let cleanup = v4::host_record(v4::records::CLEANUP);
    let mut process = None;
    let returned_global = match result {
        Some(Ok((output, global))) => {
            process = Some(output);
            let open = !handle.capture_snapshot().unwrap().host.closed;
            drop(global);
            open
        }
        _ => false,
    };
    let after_drop = v4::host_record(v4::records::AFTER);
    let before_close = handle.capture_snapshot().unwrap();
    let final_start = Instant::now();
    let report = owner.finish_until(final_start + Duration::from_secs(3));
    let final_elapsed = final_start.elapsed();
    let seen = control.observations();
    let bytes = output.lock().unwrap().clone();
    let host_records = v4::host().records.lock().unwrap();
    let mut evidence = std::fs::File::create(&args[6]).unwrap();
    writeln!(evidence, "V4 backend=LiteInst fixture Tool=ClockTool Global=ClockGlobal profile={} stdio={} case={} pressure={} work={work}\nresult={result_text}\nlimits={limits:?}\nbefore_cleanup={before_cleanup:?}\nbefore_close={before_close:?}\nreport={report:?}\nhost_records={host_records:?}\nreturned_global_open={returned_global} cleanup={cleanup:?} after_drop={after_drop:?} final_elapsed={final_elapsed:?}\nended_before_gate={ended_before_gate} timed_out={timed_out} drain_timed_out={drain_timed_out}", args[0].to_string_lossy(), args[2].to_string_lossy(), args[3].to_string_lossy(), args[4].to_string_lossy()).unwrap();
    let error_length = seen.install_error_len.load(Ordering::Acquire) as usize;
    let error_bytes: Vec<_> = seen
        .install_error
        .iter()
        .take(error_length)
        .map(|value| value.load(Ordering::Acquire))
        .collect();
    writeln!(evidence, "prepared_evidence={prepared_evidence:?}\ncancellation_evidence={cancellation_evidence:?}\npost_runtime_evidence={run_evidence:?}\nreturned_stdio={returned_stdio:?}").unwrap();
    writeln!(evidence, "startup={:?} signal_policy={:?} install_result={} install_error={error_bytes:?} install_error_truncated={}\nclocks={:?} gate_clocks={:?} gate={} gate_waits={} Full={} wait_enter={} wait_exit={} wait_result={} before={} after={} invalid={}\ncallbacks={} RPCs={} verified={} guest_orders={:?} record_failed={} root_reaped={} raw_wait_status={}\nstdio={stdio:?}",
        seen.startup.each_ref().map(|value| value.load(Ordering::Acquire)), seen.signal_policy.each_ref().map(|value| value.load(Ordering::Acquire)), seen.install_result.load(Ordering::Acquire), seen.install_error_truncated.load(Ordering::Acquire),
        seen.clocks.each_ref().map(|value| value.load(Ordering::Acquire)), seen.v4_gate_clocks.each_ref().map(|value| value.load(Ordering::Acquire)), seen.v4_gate.load(Ordering::Acquire), seen.v4_waits.load(Ordering::Acquire), seen.full.load(Ordering::Acquire), seen.wait_enter.load(Ordering::Acquire), seen.wait_exit.load(Ordering::Acquire), seen.result.load(Ordering::Acquire) as i64, seen.before.load(Ordering::Acquire), seen.after.load(Ordering::Acquire), seen.invalid.load(Ordering::Acquire),
        seen.callbacks.load(Ordering::Acquire), logged::GLOBAL_RPCS.load(Ordering::Acquire), seen.verified.load(Ordering::Acquire), seen.guest_orders.each_ref().map(|value| value.load(Ordering::Acquire)), seen.record_failed.load(Ordering::Acquire), seen.guest_reaped.load(Ordering::Acquire), seen.guest_wait_status.load(Ordering::Acquire)).unwrap();
    drop(evidence);
    for (suffix, value) in [
        (".combined", &bytes),
        (".diagnostic", &report.publication.diagnostic_prefix),
    ] {
        let mut path = args[6].clone();
        path.push(suffix);
        std::fs::write(path, value).unwrap();
    }
    if let Some((stdout, stderr)) = &stdio {
        for (suffix, value) in [(".guest.stdout", stdout), (".guest.stderr", stderr)] {
            let mut path = args[6].clone();
            path.push(suffix);
            std::fs::write(path, value).unwrap();
        }
    }
    assert!(
        !timed_out && !drain_timed_out && !ended_before_gate,
        "bounded V4 execution/gate failed; evidence retained"
    );
    assert!(final_elapsed < Duration::from_secs(4));
    evidence::prepared(
        prepared_evidence
            .as_ref()
            .expect("prepared snapshot unavailable"),
        inherited,
    )
    .unwrap();
    let run_evidence = run_evidence
        .as_ref()
        .expect("post-runtime snapshot unavailable");
    evidence::terminal(run_evidence, inherited, case).unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        run_evidence.wait_status.unwrap().into_raw(),
        seen.guest_wait_status.load(Ordering::Acquire) as i32
    );
    if let Some(returned_stdio) = &returned_stdio {
        assert_eq!(stdio.as_ref().unwrap(), returned_stdio);
    }
    if case == 1 {
        let before = cancellation_evidence
            .as_ref()
            .expect("cancellation snapshot missing")
            .as_ref()
            .expect("cancellation snapshot unavailable");
        evidence::before_cancel(before, inherited).unwrap();
        assert_eq!(run_evidence.stdout.bytes(), before.stdout.bytes());
        assert_eq!(run_evidence.stderr.bytes(), before.stderr.bytes());
        assert_eq!(run_evidence.pid, before.pid);
    }
    assert_eq!(seen.install_result.load(Ordering::Acquire), 1);
    assert_eq!(
        seen.signal_policy[0].load(Ordering::Acquire),
        u64::from(logged::clocked())
    );
    assert_eq!(seen.signal_policy[1].load(Ordering::Acquire), 0);
    assert_eq!(seen.signal_policy[3].load(Ordering::Acquire), 0);
    assert_eq!(seen.invalid.load(Ordering::Acquire), 0);
    for stage in &seen.startup {
        let bits = stage.load(Ordering::Acquire);
        assert_ne!(bits & 16, 0);
        if !logged::clocked() {
            assert_eq!(bits & 3, 0);
        }
    }
    assert!(report.guest.root_reaped && report.guest.peer_closed);
    assert_eq!(seen.guest_reaped.load(Ordering::Acquire), 1);
    assert_eq!(v4::DROPS.load(Ordering::Acquire), 1);
    assert!(!before_cleanup.host.closed && !before_close.host.closed);
    assert!(report.host.closed && report.host.entrants == 0);
    if case != 3 {
        assert_eq!(report.publication.stability, ArtifactStability::Stable);
    }
    let combined = concatenate(&expected);
    assert_eq!(report.publication.diagnostic_prefix, combined);
    assert_eq!(report.publication.omitted_bytes, 0);
    let mut observed_guest = Vec::new();
    let mut offset = 0;
    for (guest, record) in &expected {
        if *guest {
            observed_guest.extend_from_slice(
                &report.publication.diagnostic_prefix[offset..offset + record.len()],
            );
        }
        offset += record.len();
    }
    let mut expected_guest = logged::PREFIX.to_vec();
    if case == 0 {
        expected_guest.extend_from_slice(&logged::BURST);
        for index in 2..logged::SAMPLES {
            expected_guest.extend_from_slice(&[0, index as u8, 0xff]);
        }
    }
    assert_eq!(observed_guest, expected_guest);
    let mut guest_path = args[6].clone();
    guest_path.push(".guest.records");
    std::fs::write(guest_path, &observed_guest).unwrap();
    assert_eq!(
        bytes,
        if case == 5 {
            concatenate(&expected[..3])
        } else {
            combined
        }
    );
    let committed: Vec<_> = expected
        .iter()
        .enumerate()
        .filter(|(_, record)| !record.0)
        .collect();
    assert_eq!(
        host_records.len(),
        committed.len() + if case == 3 { 3 } else { 0 }
    );
    for (index, (host_bytes, order)) in host_records.iter().enumerate() {
        if case == 3 && index >= committed.len() {
            assert!(order.is_err());
        } else {
            assert_eq!(host_bytes, &committed[index].1.1);
            assert_eq!(*order, Ok(committed[index].0 as u64 + 1));
        }
    }
    if case != 3 {
        assert!(cleanup.is_ok() && after_drop.is_ok());
    }
    assert_eq!(
        report.publication.progress.acknowledged_data_bytes,
        bytes.len() as u64
    );
    assert_eq!(report.publication.progress.discarded_bytes, 0);
    if case == 5 {
        assert!(report.publication.error.is_some());
        assert_eq!(
            report.publication.unpublished_bytes,
            concatenate(&expected[3..]).len() as u64
        );
        assert_eq!(report.publication.first_unpublished_order, Some(4));
        let attempt = report.publication.attempt.unwrap();
        assert_eq!(
            (attempt.order, attempt.acknowledged, attempt.attempted_end),
            (3, logged::PREFIX.len(), logged::PREFIX.len())
        );
    } else {
        assert_eq!(report.publication.unpublished_bytes, 0);
        assert_eq!(report.publication.first_unpublished_order, None);
    }
    if pressure_case {
        assert_eq!(seen.v4_gate.load(Ordering::Acquire), 1);
        assert!(
            seen.full.load(Ordering::Acquire) > 0
                && seen.wait_exit.load(Ordering::Acquire) > 0
                && seen.v4_waits.load(Ordering::Acquire) > 0
        );
        assert_eq!(
            seen.v4_gate_clocks[0].load(Ordering::Acquire),
            if logged::clocked() { 8 } else { u64::MAX }
        );
    }
    if case == 0 {
        let process = process.unwrap();
        assert!(process.status.success());
        assert!(returned_global && report.qualifies(), "{report:?}");
        assert_eq!(report.guest.phase, Phase::Complete);
        assert!(report.streams[1].finished && report.streams[1].fragment.is_empty());
        assert_eq!(report.streams[1].complete_records, 7);
        assert_eq!(seen.callbacks.load(Ordering::Acquire), 7);
        assert_eq!(logged::GLOBAL_RPCS.load(Ordering::Acquire), 7);
        assert_eq!(seen.verified.load(Ordering::Acquire), 1);
        assert_eq!(
            seen.guest_orders
                .each_ref()
                .map(|value| value.load(Ordering::Acquire)),
            [3, 5, 7, 9, 11, 13, 15]
        );
        if logged::clocked() {
            assert_eq!(
                seen.clocks
                    .each_ref()
                    .map(|value| value.load(Ordering::Acquire)),
                [7, 8, 9, 10, 17, 18, 21, 21]
            );
        }
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
        assert_eq!(
            seen.v4_gate_clocks[0].load(Ordering::Acquire),
            seen.v4_gate_clocks[1].load(Ordering::Acquire)
        );
        if inherited {
            assert!(process.stdout.is_empty() && process.stderr.is_empty());
        } else {
            assert_eq!(process.stdout, b"OUT\0\xff");
            assert_eq!(process.stderr, b"ERR\0\xfe");
        }
    } else {
        assert!(!report.qualifies());
        assert!(!report.streams[1].finished);
        assert_eq!(report.streams[1].complete_records, 1);
        assert_eq!(seen.guest_orders[0].load(Ordering::Acquire), 3);
        assert!(
            seen.guest_orders[1..]
                .iter()
                .all(|order| order.load(Ordering::Acquire) == 0)
        );
        if case == 1 {
            assert_eq!(report.guest.run, RunState::Cancelled);
        } else {
            assert!(process.is_none() && result_text.starts_with("Some(Err("));
        }
        if case == 2 {
            assert!(!report.guest.rpc_issues.is_empty());
        }
        if case == 3 {
            assert!(report.error.is_some());
        }
        if case == 4 {
            assert_eq!(seen.record_failed.load(Ordering::Acquire), 1);
            assert!(
                report.guest.issues.iter().any(
                    |issue| issue.kind == reverie_rpc_transport::guest_log::IssueKind::Producer
                )
            );
        }
    }
}
