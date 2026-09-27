/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Trap-only LiteInst with patching off must be observably plain ptrace.

use std::collections::BTreeMap;
use std::sync::Mutex;

use reverie::Guest;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;
use crate::Ia32EmulationProbe;
use crate::Ia32EmulationUnavailable;
use crate::PtraceBackendStatsSnapshot;

#[derive(Default)]
struct Log(Mutex<Vec<(Pid, String)>>);

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = ();
    type Request = String;
    type Response = ();

    async fn receive_rpc(&self, from: Pid, event: String) {
        self.0.lock().unwrap().push((from, event));
    }
}

/// Records every Tool-visible event and otherwise behaves as the default Tool.
#[derive(Default)]
struct RecordTool;

#[reverie::tool]
impl Tool for RecordTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        Subscription::all()
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        guest.send_rpc("thread-start".to_owned()).await;
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        guest.send_rpc("post-exec".to_owned()).await;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        guest.send_rpc(format!("syscall {}", call.number())).await;
        guest.tail_inject(call).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        guest.send_rpc(format!("signal {signal:?}")).await;
        Ok(Some(signal))
    }
}

fn parity_guest() -> &'static std::path::Path {
    static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trap_only_parity.c");
        let output =
            std::env::temp_dir().join(format!("reverie-trap-only-parity-{}", std::process::id()));
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g"])
            .arg(&source)
            .arg("-o")
            .arg(&output)
            .status()
            .expect("invoke cc for the trap-only parity fixture");
        assert!(status.success(), "compile {}", source.display());
        output
    });
    GUEST.as_path()
}

/// Groups a (pid, event) trace per task. Each task's own sequence is
/// deterministic; the interleaving of a parent and child is not.
fn per_task(trace: Vec<(Pid, String)>) -> Vec<Vec<String>> {
    let mut tasks = BTreeMap::<Pid, Vec<String>>::new();
    for (pid, event) in trace {
        tasks.entry(pid).or_default().push(event);
    }
    let mut sequences = tasks.into_values().collect::<Vec<_>>();
    sequences.sort();
    sequences
}

struct Observed {
    status: ExitStatus,
    tool_events: Vec<Vec<String>>,
    stops: Vec<Vec<String>>,
    counts: PtraceBackendStatsSnapshot,
}

async fn run_parity_guest(trap_only: bool) -> Observed {
    let mut builder = TracerBuilder::<RecordTool>::new(Command::new(parity_guest()))
        .backend_stats(BackendStatsRequest::ENABLED);
    if trap_only {
        builder = builder.liteinst_trap_only(SitePatching::Off);
    }
    let tracer = builder.spawn().await.expect("spawn parity guest");
    let stats = tracer.backend_stats().expect("stats were requested");
    let handle = tracer.liteinst_trap_only();
    assert_eq!(handle.is_some(), trap_only);
    assert!(
        tracer.liteinst_instrumentation_stats().is_none(),
        "neither mode requested LiteInst patch statistics"
    );
    let (status, log) = tokio::time::timeout(Duration::from_secs(20), tracer.wait())
        .await
        .expect("parity guest timed out")
        .expect("parity guest run failed");
    if let Some(handle) = handle {
        assert_eq!(handle.patching(), SitePatching::Off);
        assert_eq!(handle.patched_sites(), 0, "patching off wrote a site");
    }
    let tool_events = per_task(std::mem::take(&mut *log.0.lock().unwrap()));
    Observed {
        status,
        tool_events,
        stops: per_task(stats.stop_trace()),
        counts: reverie::BackendStatsSource::backend_stats(&stats),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn trap_only_with_patching_off_matches_plain_ptrace_stop_for_stop() {
    let ptrace = run_parity_guest(false).await;
    let trap_only = run_parity_guest(true).await;

    // The comparison must cover the classes the preload hybrid refuses.
    assert_eq!(ptrace.status, ExitStatus::Exited(0));
    let all_stops = ptrace.stops.concat();
    for required in [
        "exec",
        "new-child Fork",
        "new-child Vfork",
        "VforkDone",
        "Signal(SIGUSR1)",
    ] {
        assert!(
            all_stops.iter().any(|stop| stop == required),
            "plain-ptrace baseline lacks a {required} stop: {all_stops:?}"
        );
    }
    let seccomp_stops = all_stops
        .iter()
        .filter(|stop| stop.starts_with("seccomp "))
        .count();
    assert!(
        seccomp_stops > 50,
        "baseline has only {seccomp_stops} seccomp stops"
    );
    assert_eq!(
        ptrace.stops.len(),
        3,
        "root, fork child, and vfork child each own a stop sequence"
    );
    let all_events = ptrace.tool_events.concat();
    assert!(all_events.iter().any(|event| event == "post-exec"));
    assert!(all_events.iter().any(|event| event == "signal SIGUSR1"));

    assert_eq!(trap_only.status, ptrace.status);
    assert_eq!(trap_only.stops, ptrace.stops, "stop sequences diverged");
    assert_eq!(
        trap_only.tool_events, ptrace.tool_events,
        "Tool-visible events diverged"
    );
    assert_eq!(trap_only.counts, ptrace.counts, "stop counts diverged");
}

#[tokio::test(flavor = "current_thread")]
async fn trap_only_refuses_launch_without_ia32_emulation() {
    let marker = tempfile_path("trap-only-refused");
    let mut command = Command::new(parity_guest());
    command.arg("touch").arg(&marker);
    let result = TracerBuilder::<RecordTool>::new(command)
        .liteinst_trap_only(SitePatching::Off)
        .liteinst_trap_only_ia32_probe_for_test(Ia32EmulationProbe::Unavailable(
            "forced unavailable by the test".into(),
        ))
        .spawn()
        .await;
    let error = match result {
        Ok(_) => panic!("trap-only launched without IA-32 syscall emulation"),
        Err(error) => error,
    };
    let Error::Tool(tool_error) = &error else {
        panic!("refusal must be a named Tool error, got {error:?}");
    };
    let refusal = tool_error
        .downcast_ref::<Ia32EmulationUnavailable>()
        .unwrap_or_else(|| panic!("refusal is not Ia32EmulationUnavailable: {error}"));
    assert_eq!(refusal.observation, "forced unavailable by the test");
    assert!(
        error
            .to_string()
            .contains("LiteInst trap-only launch refused"),
        "{error}"
    );
    assert!(
        !marker.exists(),
        "the guest ran even though the launch was refused"
    );

    // The same launch with an available probe runs the guest: the refusal
    // above is caused by the probe result, not by the command.
    let mut command = Command::new(parity_guest());
    command.arg("touch").arg(&marker);
    let (status, _) = TracerBuilder::<RecordTool>::new(command)
        .liteinst_trap_only(SitePatching::Off)
        .liteinst_trap_only_ia32_probe_for_test(Ia32EmulationProbe::Available)
        .spawn()
        .await
        .expect("available probe admits the launch")
        .wait()
        .await
        .expect("witness guest run");
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(marker.exists(), "the admitted guest did not run");
    std::fs::remove_file(&marker).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn trap_only_and_the_preload_runtime_are_mutually_exclusive() {
    let marker = tempfile_path("trap-only-exclusive");
    let mut command = Command::new(parity_guest());
    command.arg("touch").arg(&marker);
    let result = TracerBuilder::<RecordTool>::new(command)
        .liteinst_runtime("/nonexistent/preload.so", 1, 2, 3, 4)
        .liteinst_trap_only(SitePatching::Off)
        .spawn()
        .await;
    let error = match result {
        Ok(_) => panic!("both LiteInst modes were accepted together"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("mutually exclusive"), "{error}");
    assert!(!marker.exists(), "the guest ran despite the refusal");
}

#[test]
fn host_services_int_0x80() {
    // Trap-only parity depends on the real probe. A host without the IA-32
    // entry must fail this test loudly rather than skip the parity test.
    assert_eq!(crate::probe_ia32_emulation(), Ia32EmulationProbe::Available);
}

fn tempfile_path(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("reverie-{label}-{}-{serial}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}
