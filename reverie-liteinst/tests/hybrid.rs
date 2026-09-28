use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(target_arch = "x86_64")]
use reverie::CpuIdResult;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
#[cfg(target_arch = "x86_64")]
use reverie::Rdtsc;
#[cfg(target_arch = "x86_64")]
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Addr;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;
use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;

// Linux truncates PR_SET_NAME to 15 bytes.  Give every concurrently running
// fixture a distinct, exact-width marker so one test never mistakes another
// test's still-live process (or terminal zombie awaiting its own reaper) for a
// cleanup failure.  The cleanup assertions remain fail-closed: they still
// require zero processes carrying this test's marker.
static PROCESS_NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn unique_process_name() -> String {
    let sequence = PROCESS_NAME_SEQUENCE.fetch_add(1, Ordering::SeqCst) & 0x000f_ffff;
    let identity = ((std::process::id() as u64) << 20) | sequence;
    format!("li{:013x}", identity)
}

#[derive(Debug, Default)]
struct EventCounter {
    delivered: AtomicU64,
    task_creation_events: AtomicU64,
    cpuid_events: AtomicU64,
    cpuid_interception: AtomicU64,
    rdtsc_events: AtomicU64,
    last_getpid_rip: AtomicU64,
    last_getpid_r12: AtomicU64,
    helper_mprotect_callbacks: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for EventCounter {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, increment: u64) {
        if increment & (1_u64 << 63) != 0 {
            self.last_getpid_rip
                .store(increment & ((1_u64 << 62) - 1), Ordering::SeqCst);
            self.delivered.fetch_add(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 62) != 0 {
            self.last_getpid_r12
                .store(increment & ((1_u64 << 62) - 1), Ordering::SeqCst);
        } else if increment & (1_u64 << 61) != 0 {
            self.helper_mprotect_callbacks
                .fetch_add(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 60) != 0 {
            self.cpuid_events.fetch_add(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 59) != 0 {
            self.rdtsc_events.fetch_add(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 58) != 0 {
            self.cpuid_interception.store(1, Ordering::SeqCst);
        } else if increment & (1_u64 << 57) != 0 {
            self.task_creation_events.fetch_add(1, Ordering::SeqCst);
        } else {
            self.delivered.fetch_add(increment, Ordering::SeqCst);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[derive(Default)]
struct ActivationCpuEvents;

#[cfg(target_arch = "x86_64")]
#[reverie::tool]
impl Tool for ActivationCpuEvents {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        Subscription::all()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        Ok(guest.inject(syscall).await?)
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), reverie::Errno> {
        if guest.has_cpuid_interception() {
            guest.send_rpc(1_u64 << 58).await;
        }
        Ok(())
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        guest.send_rpc(1_u64 << 60).await;
        // Keep the loader's required x86-64 feature floor while proving that
        // the Tool, rather than the guest, decides the observed result.
        let native = std::arch::x86_64::__cpuid_count(eax, ecx);
        Ok(CpuIdResult {
            eax: native.eax,
            ebx: native.ebx,
            ecx: native.ecx,
            edx: native.edx,
        })
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, reverie::Errno> {
        guest.send_rpc(1_u64 << 59).await;
        Ok(RdtscResult {
            tsc: 0x1234_5678,
            aux: (request == Rdtsc::Tscp).then_some(0x42),
        })
    }
}

#[derive(Default)]
struct CountSyscalls;

#[derive(Debug, Default)]
struct ExecEvents {
    events: std::sync::Mutex<Vec<(u64, u64, u64)>>,
}

#[reverie::global_tool]
impl GlobalTool for ExecEvents {
    type Request = (u64, u64, u64);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, event: Self::Request) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Default)]
struct ExecTool;

#[reverie::tool]
impl Tool for ExecTool {
    type GlobalState = ExecEvents;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid, Sysno::execve, Sysno::execveat]
            .into_iter()
            .collect()
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), reverie::Errno> {
        // A successful exec has a new initial stack. In particular, a Tool
        // callback must not read registers from the replaced hook frame.
        let regs = guest.regs().await;
        let argc: u64 = guest
            .memory()
            .read_value(Addr::from_raw(regs.rsp as usize).unwrap())?;
        guest.send_rpc((0, argc, 0)).await;
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global_state: &G,
        exit_status: ExitStatus,
    ) -> Result<(), Error> {
        global_state
            .send_rpc((
                4,
                pid.as_raw() as u64,
                u64::from(exit_status == ExitStatus::Exited(0)),
            ))
            .await;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = syscall.into_parts();
        if nr == Sysno::getpid && args.arg0 == 0x6e786578 {
            guest
                .send_rpc((1, args.arg1 as u64, args.arg2 as u64))
                .await;
            return Ok(0x4242);
        }
        if matches!(nr, Sysno::execve | Sysno::execveat) {
            guest.send_rpc((2, nr as u64, 0)).await;
        }
        match guest.inject(syscall).await {
            Ok(value) => Ok(value),
            Err(error) => {
                guest
                    .send_rpc((3, nr as u64, error.into_raw() as u64))
                    .await;
                Err(error.into())
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_reactivates_after_exec() {
    let (_directory, guest) = compile_fixture("hybrid_exec_generation.c");
    for mode in ["cold", "hot", "execveat"] {
        let mut command = Command::new(&guest);
        command.arg(mode).arg("0");
        let (output, global) = tokio::time::timeout(
            Duration::from_secs(10),
            LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(
                command,
                (),
                preload_path(),
            ),
        )
        .await
        .expect("exec did not finish")
        .unwrap();
        assert!(output.status.success(), "mode={mode} output={output:?}");
        assert_eq!(output.stdout, b"exec-generations-finished\n", "mode={mode}");
        let events = global.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.0 == 0)
                .copied()
                .collect::<Vec<_>>(),
            vec![(0, 3, 0); 3],
            "mode={mode} events={events:?}"
        );
        let next_exec = if mode == "execveat" {
            Sysno::execveat
        } else {
            Sysno::execve
        };
        assert_eq!(
            events
                .iter()
                .filter(|e| e.0 == 2)
                .copied()
                .collect::<Vec<_>>(),
            vec![
                (2, Sysno::execve as u64, 0),
                (2, next_exec as u64, 0),
                (2, next_exec as u64, 0)
            ],
            "initial launch and two replacement execs: mode={mode} events={events:?}"
        );
        let stages = if mode == "cold" { 2..3 } else { 0..3 };
        let expected = stages
            .flat_map(|stage| (0..3).map(move |i| (1, stage, i)))
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.0 == 1)
                .copied()
                .collect::<Vec<_>>(),
            expected,
            "mode={mode} events={events:?}"
        );
        assert!(
            !events.iter().any(|e| e.0 == 3),
            "mode={mode} events={events:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_failed_exec_preserves_installed_site() {
    let (_directory, guest) = compile_fixture("hybrid_exec_generation.c");
    let mut command = Command::new(guest);
    command.arg("failed").arg("0");
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(command, (), preload_path()),
    )
    .await
    .expect("failed exec did not return")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"failed-exec-preserved\n");
    let events = global.events.lock().unwrap();
    assert_eq!(events.iter().filter(|e| e.0 == 0).count(), 1, "{events:?}");
    assert_eq!(events.iter().filter(|e| e.0 == 1).count(), 6, "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 3)
            .copied()
            .collect::<Vec<_>>(),
        vec![
            (3, Sysno::execve as u64, libc::ENOENT as u64),
            (3, Sysno::execveat as u64, libc::ENOENT as u64)
        ],
        "{events:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_worker_exec_is_refused_and_reaped() {
    for mode in ["worker", "worker-execveat"] {
        let (_directory, guest) = compile_fixture("hybrid_thread_exec.c");
        let files = tempfile::tempdir().unwrap();
        let ids = files.path().join("ids");
        let marker = files.path().join("entered");
        let mut command = Command::new(guest);
        command.arg(mode).arg(&ids).arg(&marker);
        let unrelated = UnrelatedStoppedProcess::spawn();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(
                command,
                (),
                preload_path(),
            ),
        )
        .await
        .expect("nonleader exec did not reach session cleanup");
        let error = result.expect_err("nonleader exec unexpectedly reported success");
        let ids = fs::read_to_string(ids).unwrap();
        let ids = ids
            .split_whitespace()
            .map(|id| id.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2, "{ids:?}");
        let (pid, former_tid) = (ids[0], ids[1]);
        assert_ne!(pid, former_tid, "the exec caller was not a worker");
        let text = error.to_string();
        assert_eq!(
            text,
            format!(
                "reject LiteInst post-start exec failed for tracee {pid}: exec requires the original thread-group leader (former tid {former_tid}, event tid {pid}, pid {pid})"
            ),
            "mode={mode}: nonleader exec did not retain its precise refusal"
        );
        assert!(!marker.exists(), "nonleader exec reached application entry");
        eprintln!("mode={mode}: {text}; identities={ids:?}");
        assert_pid_reaped(pid);
        assert_pid_reaped(former_tid);
        unrelated.assert_live_and_unreaped();
        unrelated.kill_and_reap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_worker_failed_exec_preserves_the_running_image() {
    let (_directory, guest) = compile_fixture("hybrid_thread_exec.c");
    let files = tempfile::tempdir().unwrap();
    let ids = files.path().join("ids");
    let marker = files.path().join("entered");
    let mut command = Command::new(guest);
    command.arg("failed").arg(&ids).arg(&marker);
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(3),
        LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(command, (), preload_path()),
    )
    .await
    .expect("worker failed exec did not continue")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"worker-failed-exec-preserved\n");
    assert!(!marker.exists(), "failed exec replaced the application");
    let events = global.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 0)
            .copied()
            .collect::<Vec<_>>(),
        vec![(0, 4, 0)],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 1)
            .copied()
            .collect::<Vec<_>>(),
        vec![(1, 4, 0), (1, 4, 1)],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 2)
            .copied()
            .collect::<Vec<_>>(),
        vec![
            (2, Sysno::execve as u64, 0),
            (2, Sysno::execve as u64, 0),
            (2, Sysno::execveat as u64, 0)
        ],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 3)
            .copied()
            .collect::<Vec<_>>(),
        vec![
            (3, Sysno::execve as u64, libc::ENOENT as u64),
            (3, Sysno::execveat as u64, libc::ENOENT as u64)
        ],
        "{events:?}"
    );
    let ids = fs::read_to_string(ids).unwrap();
    let ids = ids
        .split_whitespace()
        .map(|id| id.parse::<u32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    assert_pid_reaped(ids[0]);
    assert_pid_reaped(ids[1]);
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_exec_during_preinit_is_refused_and_reaped() {
    let (_directory, guest) = compile_fixture("hybrid_exec_during_preinit.c");
    let files = tempfile::tempdir().unwrap();
    let ids = files.path().join("pid");
    let marker = files.path().join("entered");
    let mut command = Command::new(guest);
    command.arg("start").arg(&ids).arg(&marker);
    let error = run_fail_closed_and_assert_reaped(command, &ids).await;
    let pid = fs::read_to_string(ids).unwrap();
    let pid = pid.trim().parse::<u32>().unwrap();
    assert!(
        error.to_string().contains(&format!(
            "reject LiteInst post-start exec failed for tracee {pid}: exec requires an activated thread-group leader (phase Waiting, tid {pid}, pid {pid})"
        )),
        "exec during real ELF preinit did not retain its phase refusal: {error}"
    );
    assert!(!marker.exists(), "preinit exec reached application entry");
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_leader_exec_with_a_live_sibling_reactivates() {
    let (_directory, guest) = compile_fixture("hybrid_thread_exec.c");
    let files = tempfile::tempdir().unwrap();
    let ids = files.path().join("ids");
    let marker = files.path().join("entered");
    let mut command = Command::new(guest);
    command.arg("leader").arg(&ids).arg(&marker);
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(3),
        LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(command, (), preload_path()),
    )
    .await
    .expect("leader exec with a live sibling did not complete")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"threaded-leader-exec-followed\n");
    assert_eq!(fs::read(marker).unwrap(), b"entered\n");
    let events = global.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 0)
            .copied()
            .collect::<Vec<_>>(),
        vec![(0, 4, 0); 2],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 1)
            .copied()
            .collect::<Vec<_>>(),
        vec![(1, 4, 0)],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.0 == 2)
            .copied()
            .collect::<Vec<_>>(),
        vec![(2, Sysno::execve as u64, 0); 2],
        "{events:?}"
    );
    assert!(!events.iter().any(|e| e.0 == 3), "{events:?}");
    let ids = fs::read_to_string(ids).unwrap();
    let ids = ids
        .split_whitespace()
        .map(|id| id.parse::<u32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    assert_pid_reaped(ids[0]);
    assert_pid_reaped(ids[1]);
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_exec_requires_preload_and_selector_before_entry() {
    let (_directory, guest) = compile_fixture("hybrid_exec_generation.c");
    let markers = tempfile::tempdir().unwrap();
    for mode in ["drop-preload", "drop-selector"] {
        let marker = markers.path().join(mode);
        let mut command = Command::new(&guest);
        command.arg(mode).arg(&marker);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(
                command,
                (),
                preload_path(),
            ),
        )
        .await
        .expect("missing-runtime exec did not reach the entry guard");
        let error = result.expect_err("exec without the required runtime reported success");
        assert!(
            error
                .to_string()
                .contains("verify LiteInst runtime before executable entry failed")
                && error
                    .to_string()
                    .contains("before the required preload handshake completed"),
            "mode={mode}: {error}"
        );
        assert!(
            !marker.exists(),
            "mode={mode} reached its first application side effect"
        );
        let pid = error
            .to_string()
            .split("tracee ")
            .nth(1)
            .and_then(|suffix| suffix.split(':').next())
            .and_then(|pid| pid.parse::<u32>().ok())
            .expect("entry guard omitted tracee identity");
        assert_pid_reaped(pid);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_hybrid_fork_child_exec_completes_before_and_after_root_exit() {
    const CHILD_ENV: &str = "REVERIE_LITEINST_FORK_EXEC_REAPER_TEST_CHILD";
    const TEST: &str = "host_hybrid_fork_child_exec_completes_before_and_after_root_exit";
    if std::env::var(CHILD_ENV).as_deref() != Ok(TEST) {
        use std::os::unix::process::CommandExt;

        // Own the orphan's real-parent wait as well as its ptrace wait. A
        // foreign init/subreaper may retain a released zero-exit zombie after
        // the backend returns. Isolate this process-wide setting from every
        // other test, and keep the immediate reaping assertions below intact.
        let mut child = ProcessCommand::new(std::env::current_exe().unwrap());
        child
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, TEST);
        // SAFETY: the post-fork callback makes only the Linux prctl syscall;
        // it does not allocate, lock, or alter the parent test process.
        unsafe {
            child.pre_exec(|| {
                if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let status = child.status().unwrap();
        assert!(
            status.success(),
            "isolated fork/exec reaping test failed: {status}"
        );
        return;
    }
    let mut subreaper: libc::c_int = 0;
    // SAFETY: PR_GET_CHILD_SUBREAPER writes one integer to this live pointer.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut subreaper, 0, 0, 0) },
        0
    );
    assert_eq!(subreaper, 1, "isolated test must own the real-parent reap");

    for fixture in ["hybrid_fork_exec.c", "hybrid_fork_exec_after_root_exit.c"] {
        let (_directory, guest) = compile_fixture(fixture);
        let name = unique_process_name();
        let pids = tempfile::tempdir().unwrap();
        let pid_file = pids.path().join("root.pid");
        let mut command = Command::new(guest);
        command.arg(&name).arg(&pid_file);
        let (output, global) = tokio::time::timeout(
            Duration::from_secs(10),
            LiteinstBackend::run_host_with_output_and_preload::<ExecTool>(
                command,
                (),
                preload_path(),
            ),
        )
        .await
        .expect("forked exec did not complete")
        .unwrap();
        assert_eq!(
            output.status,
            ExitStatus::Exited(0),
            "fixture={fixture} {output:?}"
        );
        assert_eq!(
            output.stdout, b"fork-exec-root-finished\n",
            "fixture={fixture} {output:?}"
        );
        let events = global.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.0 == 0)
                .copied()
                .collect::<Vec<_>>(),
            vec![(0, 3, 0), (0, 1, 0)],
            "fixture={fixture} {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.0 == 2)
                .copied()
                .collect::<Vec<_>>(),
            vec![(2, Sysno::execve as u64, 0); 2],
            "fixture={fixture} {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.0 == 3),
            "fixture={fixture} {events:?}"
        );
        let exits = events
            .iter()
            .filter(|e| e.0 == 4)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(exits.len(), 2, "fixture={fixture} {events:?}");
        assert!(
            exits.iter().all(|e| e.2 == 1),
            "fixture={fixture} {events:?}"
        );
        assert_ne!(exits[0].1, exits[1].1, "fixture={fixture} {events:?}");
        let root_pid: u32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            exits.iter().any(|e| e.1 == root_pid as u64),
            "fixture={fixture} {events:?}"
        );
        for exit in exits {
            assert_pid_reaped(exit.1 as u32);
        }
        assert_processes_named_eventually_reaped(&name, "completed exec left a process behind");
    }
}

#[reverie::tool]
impl Tool for CountSyscalls {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getrandom, Sysno::getpid, Sysno::mprotect]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert!(matches!(
            syscall.number(),
            Sysno::getrandom | Sysno::getpid | Sysno::mprotect
        ));
        if syscall.number() == Sysno::getpid {
            let regs = guest.regs().await;
            guest.send_rpc((1_u64 << 62) | regs.r12).await;
            guest.send_rpc((1_u64 << 63) | regs.rip).await;
        } else if syscall.number() == Sysno::mprotect {
            guest.send_rpc(1_u64 << 61).await;
        } else {
            guest.send_rpc(1).await;
        }
        Ok(guest.inject(syscall).await?)
    }
}

#[derive(Default)]
struct PassthroughGetpid;

#[reverie::tool]
impl Tool for PassthroughGetpid {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        guest.send_rpc(1).await;
        Ok(guest.inject(syscall).await?)
    }
}

#[derive(Default)]
struct PassthroughGetpidAndTaskCreation;

#[reverie::tool]
impl Tool for PassthroughGetpidAndTaskCreation {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [
            Sysno::getpid,
            Sysno::clone,
            Sysno::clone3,
            Sysno::fork,
            Sysno::vfork,
        ]
        .into_iter()
        .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() == Sysno::getpid {
            guest.send_rpc(1).await;
        } else {
            assert!(matches!(
                syscall.number(),
                Sysno::clone | Sysno::clone3 | Sysno::fork | Sysno::vfork
            ));
            guest.send_rpc(1_u64 << 57).await;
        }
        Ok(guest.inject(syscall).await?)
    }
}

#[derive(Default)]
struct ExitRecorder {
    path: PathBuf,
}

#[reverie::global_tool]
impl GlobalTool for ExitRecorder {
    type Request = i32;
    type Response = ();
    type Config = PathBuf;

    async fn init_global_state(path: &PathBuf) -> Self {
        Self { path: path.clone() }
    }

    async fn receive_rpc(&self, _from: Tid, pid: i32) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap();
        writeln!(file, "{pid}").unwrap();
    }
}

#[derive(Default)]
struct PassthroughTaskCreationAndRecordExits;

#[reverie::tool]
impl Tool for PassthroughTaskCreationAndRecordExits {
    type GlobalState = ExitRecorder;
    type ThreadState = ();

    fn subscriptions(_config: &PathBuf) -> Subscription {
        [
            Sysno::getpid,
            Sysno::clone,
            Sysno::clone3,
            Sysno::fork,
            Sysno::vfork,
        ]
        .into_iter()
        .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        Ok(guest.inject(syscall).await?)
    }

    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global_state: &G,
        _exit_status: ExitStatus,
    ) -> Result<(), Error> {
        global_state.send_rpc(pid.as_raw()).await;
        Ok(())
    }
}

#[derive(Default)]
struct PassthroughTaskCreationWithPendingProcessExit;

#[reverie::tool]
impl Tool for PassthroughTaskCreationWithPendingProcessExit {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [
            Sysno::getpid,
            Sysno::clone,
            Sysno::clone3,
            Sysno::fork,
            Sysno::vfork,
        ]
        .into_iter()
        .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        Ok(guest.inject(syscall).await?)
    }

    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        _pid: Pid,
        _global_state: &G,
        _exit_status: ExitStatus,
    ) -> Result<(), Error> {
        std::future::pending().await
    }
}

#[derive(Default)]
struct ObservePkey;

#[reverie::tool]
impl Tool for ObservePkey {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid, Sysno::pkey_mprotect].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.send_rpc(1).await;
        Ok(guest.inject(syscall).await?)
    }
}

#[derive(Default)]
struct ReplaceGetpid;

#[reverie::tool]
impl Tool for ReplaceGetpid {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        guest.send_rpc(1).await;
        let replacement = Syscall::from_raw(Sysno::getppid, SyscallArgs::new(0, 0, 0, 0, 0, 0));
        Ok(guest.inject(replacement).await?)
    }
}

#[derive(Default)]
struct DoubleInjectGetpid;

#[reverie::tool]
impl Tool for DoubleInjectGetpid {
    type GlobalState = EventCounter;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        guest.send_rpc(1).await;
        let _ = guest.inject(syscall).await?;
        Ok(guest.inject(syscall).await?)
    }
}

/// What `SteppedHookTool` tells its global state.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
enum SteppedHookNote {
    /// The guest's RCB clock at a getpid.
    Getpid(u64),
    TimerEvent,
}

#[derive(Debug, Default)]
struct SteppedHookEvents {
    /// The guest's RCB clock at each getpid, in order.
    getpid_clocks: std::sync::Mutex<Vec<u64>>,
    timer_events: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for SteppedHookEvents {
    type Request = SteppedHookNote;
    type Response = ();
    /// The timer's RCBs from each getpid to its target.
    type Config = u64;

    async fn receive_rpc(&self, _from: Tid, note: SteppedHookNote) {
        match note {
            SteppedHookNote::Getpid(clock) => self.getpid_clocks.lock().unwrap().push(clock),
            SteppedHookNote::TimerEvent => {
                self.timer_events.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// Answers every getpid itself, with 0x4242, and requests a precise timer
/// there.
#[derive(Default)]
struct SteppedHookTool;

#[reverie::tool]
impl Tool for SteppedHookTool {
    type GlobalState = SteppedHookEvents;
    type ThreadState = ();

    fn subscriptions(_config: &u64) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        let clock = guest.read_clock()?;
        guest.send_rpc(SteppedHookNote::Getpid(clock)).await;
        let rcbs = *guest.config();
        guest.set_timer_precise(TimerSchedule::Rcbs(rcbs))?;
        Ok(0x4242)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        guest.send_rpc(SteppedHookNote::TimerEvent).await;
    }
}

fn preload_path() -> PathBuf {
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
    let target = launcher.parent().unwrap();
    [
        target.join("libreverie_liteinst.so"),
        target.join("deps/libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("cargo did not build the LiteInst preload cdylib")
}

fn compile_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = directory.path().join(name.trim_end_matches(".c"));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = ProcessCommand::new(compiler)
        .args(["-std=gnu11", "-O0", "-fno-pie", "-no-pie"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

fn compile_static_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = directory.path().join("li-static-exit");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = ProcessCommand::new(compiler)
        .args([
            "-std=gnu11",
            "-O0",
            "-nostdlib",
            "-static",
            "-fno-stack-protector",
            "-fno-pie",
            "-no-pie",
            "-Wl,--build-id=none",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

fn symbol_address(binary: &std::path::Path, symbol: &str) -> u64 {
    let output = ProcessCommand::new("nm").arg(binary).output().unwrap();
    assert!(output.status.success(), "nm failed: {output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?;
            let _kind = fields.next()?;
            (fields.next()? == symbol).then(|| u64::from_str_radix(address, 16).unwrap())
        })
        .unwrap_or_else(|| panic!("missing symbol {symbol}"))
}

fn processes_named(name: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap() {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if comm.trim() == name {
            found.push(pid);
        }
    }
    found
}

fn assert_processes_named_eventually_reaped(name: &str, context: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = processes_named(name);
        if remaining.is_empty() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            let remaining_status = remaining
                .iter()
                .map(|pid| fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default())
                .collect::<Vec<_>>();
            panic!("{context}: {remaining:?} {remaining_status:?}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn assert_pid_reaped(pid: u32) {
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "failed LiteInst process {pid} remains stopped or unreaped"
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
        -1,
        "failed LiteInst process {pid} still has a waitable state"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "failed LiteInst process {pid} was not fully reaped"
    );
}

struct UnrelatedStoppedProcess {
    child: Option<std::process::Child>,
}

impl UnrelatedStoppedProcess {
    fn spawn() -> Self {
        let child = ProcessCommand::new("/bin/sleep")
            .arg("300")
            .spawn()
            .expect("spawn unrelated process");
        let pid = child.id() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) },
            pid,
            "wait for unrelated process to stop"
        );
        assert!(
            libc::WIFSTOPPED(status),
            "unrelated process did not enter a stopped state: {status}"
        );
        Self { child: Some(child) }
    }

    fn assert_live_and_unreaped(&self) {
        let pid = self.child.as_ref().unwrap().id() as i32;
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "cleanup signaled an unrelated process"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            0,
            "cleanup made an unrelated process waitable"
        );
    }

    fn kill_and_reap(mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().expect("kill unrelated process after bracket");
        child.wait().expect("reap unrelated process after bracket");
    }
}

impl Drop for UnrelatedStoppedProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn is_original_liteinst_session_refusal(text: &str, operation: &str) -> bool {
    text.contains("LiteInst session failed closed in a non-root task")
        && text.contains(&format!("{operation} failed for tracee "))
        && !text.contains("LiteInst tracee cleanup failed")
        && !text.contains("notifier did not acknowledge terminal cleanup")
}

fn assert_original_liteinst_session_refusal(error: &Error, operation: &str) {
    let text = error.to_string();
    assert!(
        is_original_liteinst_session_refusal(&text, operation),
        "session failure did not retain the original refused {operation}, or cleanup replaced it: {text}"
    );
}

#[test]
fn session_refusal_assertion_rejects_terminal_cleanup_wrapper() {
    let original = "LiteInst session failed closed in a non-root task: refuse vfork under the LiteInst hybrid failed for tracee 42: vfork child refused";
    let wrapped = format!(
        "LiteInst tracee cleanup failed after {original}: notifier did not acknowledge terminal cleanup"
    );
    assert!(is_original_liteinst_session_refusal(
        original,
        "refuse vfork under the LiteInst hybrid"
    ));
    assert!(!is_original_liteinst_session_refusal(
        &wrapped,
        "refuse vfork under the LiteInst hybrid"
    ));
    let entry = "LiteInst session failed closed in a non-root task: verify LiteInst runtime before executable entry failed for tracee 42: before the required preload handshake completed";
    assert!(is_original_liteinst_session_refusal(
        entry,
        "verify LiteInst runtime before executable entry"
    ));
    assert!(!is_original_liteinst_session_refusal(entry, "exec"));
    assert!(!is_original_liteinst_session_refusal(
        entry,
        "reject LiteInst post-start exec"
    ));
}

async fn wait_for_pid_file(pid_file: &std::path::Path) -> u32 {
    loop {
        if let Ok(contents) = fs::read_to_string(pid_file)
            && let Ok(pid) = contents.trim().parse::<u32>()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn run_fail_closed_and_assert_reaped(command: Command, pid_file: &std::path::Path) -> Error {
    let mut run = Box::pin(LiteinstBackend::run_host_with_output_and_preload::<
        PassthroughGetpid,
    >(command, (), preload_path()));
    let mut early_result = None;
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            result = &mut run => {
                early_result = Some(result);
                wait_for_pid_file(pid_file).await
            }
            pid = wait_for_pid_file(pid_file) => pid,
        }
    })
    .await
    .expect("fail-closed fixture did not publish its pid");
    let result = if let Some(result) = early_result {
        result
    } else {
        match tokio::time::timeout(Duration::from_secs(3), &mut run).await {
            Ok(result) => result,
            Err(_) => {
                drop(run);
                assert_pid_reaped(pid);
                panic!("fail-closed fixture hung; cancellation cleanup reaped pid {pid}");
            }
        }
    };
    let error = match result {
        Ok(_) => panic!("required LiteInst runtime unexpectedly remained active"),
        Err(error) => error,
    };
    assert_pid_reaped(pid);
    error
}

#[tokio::test(flavor = "current_thread")]
async fn initial_dynamic_preload_handshake_activates_host_lifecycle() {
    let (_directory, guest) = compile_fixture("allocator_getrandom.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<CountSyscalls>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        global.delivered.load(Ordering::SeqCst),
        3,
        "host lifecycle missed allocator/pre-constructor entropy: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn initial_static_image_without_the_runtime_fails_closed() {
    let (_directory, guest) = compile_static_fixture("hybrid_static_exit.c");
    assert!(processes_named("li-static-exit").is_empty());
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("guest.pid");
    let mut command = Command::new(guest);
    command.arg(&pid_file);

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
            command,
            (),
            preload_path(),
        ),
    )
    .await
    .expect("static image did not reach the entry guard");
    let error = result.expect_err("static image unexpectedly passed the entry guard");
    assert!(
        !pid_file.exists(),
        "static image executed its first side-effecting syscall before failing closed"
    );
    assert!(
        error
            .to_string()
            .contains("verify LiteInst runtime before executable entry failed")
            && error
                .to_string()
                .contains("before the required preload handshake completed"),
        "static image did not report the guarded entry boundary: {error}"
    );
    let pid = error
        .to_string()
        .split("tracee ")
        .nth(1)
        .and_then(|suffix| suffix.split(':').next())
        .and_then(|pid| pid.parse::<u32>().ok())
        .expect("entry-guard error did not identify the exact tracee");
    assert_pid_reaped(pid);
    assert!(
        processes_named("li-static-exit").is_empty(),
        "failed static image remains stopped or unreaped"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn valid_dynamic_run_observes_restored_executable_entry() {
    let (_directory, guest) = compile_fixture("hybrid_entry_guard.c");
    let (output, _global) = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(output.stdout, b"entry-int3=0\n", "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

#[cfg(target_arch = "x86_64")]
#[tokio::test(flavor = "current_thread")]
async fn loader_cpu_events_are_determinized_before_ready_and_entry_is_restored() {
    let (_directory, guest) = compile_fixture("hybrid_pre_ready_cpu.c");
    let (output, global) =
        LiteinstBackend::run_host_with_output_and_preload::<ActivationCpuEvents>(
            Command::new(guest),
            (),
            preload_path(),
        )
        .await
        .unwrap();

    assert_eq!(output.stdout, b"entry-int3=0 probe=1\n", "{output:?}");
    assert!(output.status.success(), "{output:?}");
    if global.cpuid_interception.load(Ordering::SeqCst) == 1 {
        assert!(
            global.cpuid_events.load(Ordering::SeqCst) >= 1,
            "the pre-constructor IFUNC CPUID did not reach the Tool despite verified kernel interception"
        );
    } else {
        assert_eq!(
            global.cpuid_events.load(Ordering::SeqCst),
            0,
            "CPUID reached the Tool after the kernel reported interception unavailable"
        );
    }
    assert!(
        global.rdtsc_events.load(Ordering::SeqCst) >= 1,
        "the pre-constructor IFUNC RDTSC did not reach the Tool"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exec_without_preload_reaches_application_entry_guard() {
    let (_directory, guest) = compile_fixture("hybrid_exec_drop_preload.c");
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("guest.pid");
    let mut command = Command::new(guest);
    command.arg(&pid_file);

    let error = run_fail_closed_and_assert_reaped(command, &pid_file).await;
    assert!(
        error
            .to_string()
            .contains("verify LiteInst runtime before executable entry failed")
            && error
                .to_string()
                .contains("before the required preload handshake completed"),
        "post-start exec did not report the lost runtime boundary: {error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn first_site_is_installed_once_and_hot_calls_use_liteinst() {
    let (_baseline_directory, baseline_guest) = compile_fixture("allocator_getrandom.c");
    let (baseline_output, baseline_global) = LiteinstBackend::run_host_with_output_and_preload::<
        CountSyscalls,
    >(Command::new(baseline_guest), (), preload_path())
    .await
    .unwrap();
    assert!(baseline_output.status.success(), "{baseline_output:?}");

    let (_directory, guest) = compile_fixture("hybrid_hot_site.c");
    let site = symbol_address(&guest, "reverie_liteinst_hybrid_getpid_site");
    let (output, global, stats) = LiteinstBackend::run_host_with_output_and_preload_and_stats::<
        CountSyscalls,
    >(Command::new(guest), (), preload_path())
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"calls=32 traps=1 hooks=31 ac=0 simd=1 spoofs=3\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 33, "{output:?}");
    assert_eq!(
        global.last_getpid_rip.load(Ordering::SeqCst),
        site + 2,
        "the host Tool must see the original logical post-syscall RIP"
    );
    assert_eq!(
        global.last_getpid_r12.load(Ordering::SeqCst),
        0x0012_3456_789a_bcde,
        "logical guest R12 must remain distinct from the controller HookContext base"
    );
    assert_eq!(
        global.helper_mprotect_callbacks.load(Ordering::SeqCst)
            - baseline_global
                .helper_mprotect_callbacks
                .load(Ordering::SeqCst),
        0,
        "the patch helper must add zero mprotect Tool callbacks above the loader baseline"
    );
    let decisions = stats.decision_counts();
    assert_eq!(
        decisions.into_iter().sum::<usize>(),
        stats.patch_candidates()
    );
    assert_eq!(stats.distinct_rips(), decisions[0] + decisions[1]);
    assert!(
        decisions[1] >= 1,
        "the known hot site must use a relocated patch: {stats}"
    );
    assert!(
        stats.instruction_length_counts()[3] >= 1,
        "the known hot site begins with a two-byte syscall: {stats}"
    );
    assert_eq!(
        stats.instruction_length_counts().into_iter().sum::<usize>(),
        stats.classified_candidates()
    );
    assert_eq!(
        stats.non_straddling() + stats.cacheline_straddlers(),
        stats.classified_candidates()
    );
    assert_eq!(
        stats.straddle_prefix_counts().into_iter().sum::<usize>(),
        stats.cacheline_straddlers()
    );
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn cacheline_straddler_uses_quiescent_patch_and_is_counted() {
    let (_directory, guest) = compile_fixture("hybrid_straddler_site.c");
    let site = symbol_address(&guest, "reverie_liteinst_straddler_site");
    assert_eq!(site % 64, 63, "fixture syscall must straddle a cache line");

    let (output, global, stats) = LiteinstBackend::run_host_with_output_and_preload_and_stats::<
        PassthroughGetpid,
    >(Command::new(guest), (), preload_path())
    .await
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"straddler-quiescent-patch-ok\n");
    assert_eq!(global.delivered.load(Ordering::SeqCst), 8);
    assert_eq!(stats.distinct_rips(), 1);
    assert_eq!(stats.patch_candidates(), 1);
    assert_eq!(stats.decision_counts(), [0, 1, 0, 0]);
    assert_eq!(stats.classified_candidates(), 1);
    assert_eq!(stats.cacheline_straddlers(), 1);
    assert_eq!(stats.non_straddling(), 0);
    assert_eq!(stats.instruction_length_counts(), [0, 0, 0, 1, 0]);
    assert_eq!(stats.straddle_prefix_counts(), [1, 0, 0, 0]);
}

#[tokio::test(flavor = "current_thread")]
async fn quiescent_helper_patches_every_cache_line_split_without_calibration() {
    let (_directory, guest) = compile_fixture("hybrid_straddler_sites.c");
    let mut command = Command::new(guest);
    command.env_remove(STRADDLER_STALENESS_TICKS_ENV);
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        command,
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"offsets=57..63 calls=14 traps=7 hooks=7\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 14, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

async fn run_cpuid_policy_mode(
    mode: Option<&str>,
) -> Option<(reverie::process::Output, EventCounter)> {
    let (_directory, guest) = compile_fixture("hybrid_cpuid_policy.c");
    let mut command = Command::new(guest);
    if let Some(mode) = mode {
        command.arg(mode);
    }
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        command,
        (),
        preload_path(),
    )
    .await
    .unwrap();
    if output.status.code() == Some(77) {
        eprintln!("skipping: this host does not support ARCH_GET_CPUID");
        return None;
    }
    Some((output, global))
}

#[tokio::test(flavor = "current_thread")]
async fn patch_helper_restores_disabled_cpuid_after_installing_a_site() {
    let Some((output, global)) = run_cpuid_policy_mode(None).await else {
        return;
    };
    assert_eq!(
        output.stdout, b"mode=active calls=32 traps=1 hooks=31 cpuid=0\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 32, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn patch_helper_restores_disabled_cpuid_after_fallback() {
    let Some((output, global)) = run_cpuid_policy_mode(Some("fallback")).await else {
        return;
    };
    assert_eq!(
        output.stdout, b"mode=fallback calls=2 traps=1 hooks=0 cpuid=0\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 2, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

async fn run_tsc_policy_mode(
    mode: Option<&str>,
) -> Option<(reverie::process::Output, EventCounter)> {
    let (_directory, guest) = compile_fixture("hybrid_tsc_policy.c");
    let mut command = Command::new(guest);
    if let Some(mode) = mode {
        command.arg(mode);
    }
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        command,
        (),
        preload_path(),
    )
    .await
    .unwrap();
    if output.status.code() == Some(77) {
        eprintln!("skipping: this host does not support PR_GET_TSC/PR_SET_TSC");
        return None;
    }
    Some((output, global))
}

#[tokio::test(flavor = "current_thread")]
async fn patch_helper_restores_faulting_tsc_after_installing_a_site() {
    let Some((output, global)) = run_tsc_policy_mode(None).await else {
        return;
    };
    assert_eq!(
        output.stdout, b"mode=active calls=32 traps=1 hooks=31 tsc=2\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 32, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn patch_helper_restores_faulting_tsc_after_fallback() {
    let Some((output, global)) = run_tsc_policy_mode(Some("fallback")).await else {
        return;
    };
    assert_eq!(
        output.stdout, b"mode=fallback calls=2 traps=1 hooks=0 tsc=2\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 2, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn first_discovery_event_can_replace_the_syscall() {
    let (_directory, guest) = compile_fixture("hybrid_hot_site.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<ReplaceGetpid>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"calls=32 traps=1 hooks=31 ac=0 simd=1 spoofs=3\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 32, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn first_discovery_event_can_inject_more_than_once() {
    let (_directory, guest) = compile_fixture("hybrid_hot_site.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<DoubleInjectGetpid>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"calls=32 traps=1 hooks=31 ac=0 simd=1 spoofs=3\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 32, "{output:?}");
    assert!(output.status.success(), "{output:?}");
}

/// Runs a multi-task fixture that is expected to complete, and requires the
/// guest's own end-of-run marker.
///
/// Exit status alone is not enough: every fixture here prints its marker only
/// after the task it creates has been created, run and reaped, so a regression
/// that skips the task cannot satisfy the assertion by exiting zero.
async fn run_multi_task_fixture<T>(fixture: &str, marker_line: &str) -> EventCounter
where
    T: Tool<GlobalState = EventCounter, ThreadState = ()> + 'static,
{
    let (_directory, guest) = compile_fixture(fixture);
    let name = unique_process_name();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("root.pid");
    let mut command = Command::new(guest);
    command.arg(&name).arg(&pid_file);
    let (output, global) =
        LiteinstBackend::run_host_with_output_and_preload::<T>(command, (), preload_path())
            .await
            .unwrap_or_else(|error| panic!("hybrid refused to follow {fixture}: {error}"));

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        marker_line.as_bytes(),
        "guest did not reach the end of {fixture}: {output:?}"
    );
    let root_pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_pid_reaped(root_pid);
    assert!(
        processes_named(&name).is_empty(),
        "LiteInst root/child remains stopped or as a zombie"
    );
    global
}

/// The hybrid follows a forked child instead of refusing at the clone boundary.
///
/// This exercises the supported fork path end-to-end. It does not independently
/// isolate the root-TID identity and root-stop lease re-arm mechanisms.
#[tokio::test(flavor = "current_thread")]
async fn hybrid_follows_a_forked_child() {
    let global = run_multi_task_fixture::<PassthroughGetpidAndTaskCreation>(
        "hybrid_fork.c",
        "fork-followed\n",
    )
    .await;
    assert!(
        global.task_creation_events.load(Ordering::SeqCst) > 0,
        "the task-subscribing tool did not observe the fork lifecycle"
    );
}

/// The hybrid follows a second thread created with `clone3(CLONE_THREAD)`.
///
/// This exercises the supported thread-creation path end-to-end. It does not
/// independently isolate the task-creating-site patch guard.
#[tokio::test(flavor = "current_thread")]
async fn hybrid_follows_a_created_thread() {
    let global = run_multi_task_fixture::<PassthroughGetpidAndTaskCreation>(
        "hybrid_thread.c",
        "thread-followed\n",
    )
    .await;
    assert!(
        global.task_creation_events.load(Ordering::SeqCst) > 0,
        "the task-subscribing tool did not observe the clone lifecycle"
    );
}

/// The task-subscribing tool remains active without manufacturing a task event
/// when the guest makes subscribed `getpid` calls but creates no task.
#[tokio::test(flavor = "current_thread")]
async fn task_subscriber_does_not_report_task_creation_without_one() {
    let (_directory, guest) = compile_fixture("hybrid_hot_site.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<
        PassthroughGetpidAndTaskCreation,
    >(Command::new(guest), (), preload_path())
    .await
    .unwrap();

    assert_eq!(
        output.stdout, b"calls=32 traps=1 hooks=31 ac=0 simd=1 spoofs=3\n",
        "{output:?}"
    );
    assert_eq!(global.delivered.load(Ordering::SeqCst), 32, "{output:?}");
    assert_eq!(
        global.task_creation_events.load(Ordering::SeqCst),
        0,
        "the tool reported task creation for a single-task fixture: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
}

/// KNOWN GAP, committed as a reproducer rather than described: two generations
/// of children do not reliably complete under this harness.
///
/// The grandchild's new-task event belongs to a NON-root parent, which is the
/// case the cleanup guard's newborn registration has to cover -- scoping that
/// registration to the root leaves the grandchild unregistered and
/// `handle_new_task` aborts on `stored child event ownership must remain
/// registered`. That much is fixed and this fixture does reach
/// `fork-tree-followed`: it passed once here, and Hermit's
/// `determinism-stress-c/fork-tree` reaches canonical L2 under the real Detcore
/// tool, which sequentializes the guest.
///
/// It is `ignore`d because it is NOT reliable here: after that single pass it
/// wedged with no forward progress on three consecutive runs, under both this
/// tool and a variant that also subscribes to the task-creating syscalls. A
/// flaky hang is worse than no test, so it does not run by default. Do not
/// treat the fix it covers as verified until this is diagnosed and the `ignore`
/// removed.
#[tokio::test(flavor = "current_thread")]
#[ignore = "known gap: second-generation fork does not reliably complete in this harness"]
async fn hybrid_follows_a_grandchild() {
    run_multi_task_fixture::<PassthroughGetpid>("hybrid_fork_tree.c", "fork-tree-followed\n").await;
}

/// A child that removes the required preload before exec fails the whole
/// session; the root must not report the success it would otherwise reach.
/// The original session failure and pending-exit cleanup controls remain
/// necessary now that exec with an inherited preload is supported.
#[tokio::test(flavor = "current_thread")]
async fn a_child_missing_preload_fails_the_session_instead_of_reporting_success() {
    let (_directory, guest) = compile_fixture("hybrid_fork_exec.c");
    let name = unique_process_name();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("root.pid");
    let exit_file = pid_directory.path().join("process-exits");
    let mut command = Command::new(guest);
    command.arg(&name).arg(&pid_file).arg("drop-preload");
    let result =
        tokio::time::timeout(
            Duration::from_secs(60),
            LiteinstBackend::run_host_with_output_and_preload::<
                PassthroughTaskCreationAndRecordExits,
            >(command, exit_file.clone(), preload_path()),
        )
        .await
        .expect("a failed non-root task was never released from the tool: the session hung");

    let error = match result {
        Ok((output, _global)) => panic!(
            "SILENT GREEN: the session reported success over a child that could not be \
             followed through exec: {output:?}"
        ),
        Err(error) => error,
    };
    assert_original_liteinst_session_refusal(
        &error,
        "verify LiteInst runtime before executable entry",
    );
    assert!(
        error
            .to_string()
            .contains("before the required preload handshake completed")
    );
    let root_pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let exits = fs::read_to_string(&exit_file).unwrap_or_default();
    let non_root_exits = exits
        .lines()
        .map(|pid| pid.parse::<i32>().unwrap())
        .filter(|pid| *pid != root_pid as i32)
        .count();
    assert_eq!(
        non_root_exits, 1,
        "the failed non-root process did not run its Tool exit callback: root={root_pid} exits={exits:?}"
    );
    assert_pid_reaped(root_pid);
    assert_processes_named_eventually_reaped(
        &name,
        "failed LiteInst root/child remains stopped or as a zombie",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn missing_preload_entry_guard_reaches_cleanup_while_process_exit_is_pending() {
    let (_directory, guest) = compile_fixture("hybrid_fork_exec.c");
    let name = unique_process_name();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("root.pid");
    let mut command = Command::new(guest);
    command.arg(&name).arg(&pid_file).arg("drop-preload");

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        LiteinstBackend::run_host_with_output_and_preload::<
            PassthroughTaskCreationWithPendingProcessExit,
        >(command, (), preload_path()),
    )
    .await
    .expect("missing-preload entry guard never reached session cleanup");
    let error = result.expect_err("missing-preload entry guard reported success");
    assert_original_liteinst_session_refusal(
        &error,
        "verify LiteInst runtime before executable entry",
    );
    assert!(
        error
            .to_string()
            .contains("before the required preload handshake completed")
    );

    let root_pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_pid_reaped(root_pid);
    assert_processes_named_eventually_reaped(
        &name,
        "missing-preload entry failure left a LiteInst process behind",
    );
}

/// A root that has already exited must stop joining its child when that child
/// reaches the missing-preload entry guard, even if its Tool exit callback is pending.
#[tokio::test(flavor = "current_thread")]
async fn missing_preload_entry_guard_cancels_root_join_while_process_exit_is_pending() {
    let (_directory, guest) = compile_fixture("hybrid_fork_exec_after_root_exit.c");
    let name = unique_process_name();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("root.pid");
    let mut command = Command::new(guest);
    command.arg(&name).arg(&pid_file).arg("drop-preload");
    let unrelated = UnrelatedStoppedProcess::spawn();

    let result = tokio::time::timeout(
        Duration::from_secs(3),
        LiteinstBackend::run_host_with_output_and_preload::<
            PassthroughTaskCreationWithPendingProcessExit,
        >(command, (), preload_path()),
    )
    .await
    .expect("missing-preload entry guard did not cancel the root's child join");
    let error = result.expect_err("missing-preload entry guard reported success");
    assert_original_liteinst_session_refusal(
        &error,
        "verify LiteInst runtime before executable entry",
    );
    assert!(
        error
            .to_string()
            .contains("before the required preload handshake completed")
    );

    let root_pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_pid_reaped(root_pid);
    assert_processes_named_eventually_reaped(
        &name,
        "missing-preload entry failure left a LiteInst process behind",
    );
    unrelated.assert_live_and_unreaped();
    unrelated.kill_and_reap();
}

/// A vfork refusal in a non-root task fails the whole session even when it is
/// recorded only after the root has reached its own clean exit.
#[tokio::test(flavor = "current_thread")]
async fn vfork_in_a_forked_child_fails_the_session_after_root_exit() {
    let (_directory, guest) = compile_fixture("hybrid_fork_vfork.c");
    let name = unique_process_name();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("root.pid");
    let mut command = Command::new(guest);
    command.arg(&name).arg(&pid_file);
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
            command,
            (),
            preload_path(),
        ),
    )
    .await
    .expect("a refused non-root vfork was never released from the tool: the session hung");

    let error = match result {
        Ok((output, _global)) => panic!(
            "SILENT GREEN: the session reported success before a non-root vfork refusal: {output:?}"
        ),
        Err(error) => error,
    };
    assert_original_liteinst_session_refusal(&error, "refuse vfork under the LiteInst hybrid");
    let root_pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_pid_reaped(root_pid);
    assert_processes_named_eventually_reaped(
        &name,
        "failed LiteInst root/child remains stopped or as a zombie",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reused_mapping_invalidates_and_rediscovers_the_syscall_site() {
    let (_directory, guest) = compile_fixture("hybrid_mapping_churn.c");
    let (output, global) = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        Command::new(guest),
        (),
        preload_path(),
    )
    .await
    .unwrap();

    assert_eq!(output.stdout, b"reuse traps=2 hooks=0\n", "{output:?}");
    assert_eq!(
        global.delivered.load(Ordering::SeqCst),
        4,
        "both generations must use the correct ptrace fallback: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn moving_a_patched_mapping_rejects_stale_hot_provenance() {
    let (_directory, guest) = compile_fixture("hybrid_mremap_patched.c");
    let command = Command::new(guest);
    let error = LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        command,
        (),
        preload_path(),
    )
    .await
    .expect_err("mremap unexpectedly moved a live patched site");

    assert!(
        error
            .to_string()
            .contains("mremap overlaps an active LiteInst hook footprint"),
        "mapping was not rejected by the pre-mutation provenance check: {error}"
    );
}

async fn run_active_footprint_mode(
    mode: &str,
) -> Result<(reverie::process::Output, EventCounter), Error> {
    let (_directory, guest) = compile_fixture("hybrid_active_footprint.c");
    let mut command = Command::new(guest);
    command.arg(mode);
    LiteinstBackend::run_host_with_output_and_preload::<PassthroughGetpid>(
        command,
        (),
        preload_path(),
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn active_hook_noop_mprotect_preserves_the_hook() {
    for (mode, stdout) in [
        ("noop", "active no-op protection preserved\n"),
        ("short-noop", "active short no-op protection preserved\n"),
    ] {
        let (output, global) = run_active_footprint_mode(mode).await.unwrap();
        assert_eq!(output.stdout, stdout.as_bytes());
        assert_eq!(global.delivered.load(Ordering::SeqCst), 3);
        assert!(output.status.success(), "{output:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn active_hook_mapping_footprints_reject_mprotect_before_mutation() {
    for (mode, syscall) in [
        ("site", "mprotect"),
        ("trampoline", "mprotect"),
        ("arena-rw", "mprotect"),
        ("short-site", "mprotect"),
        ("short-trampoline", "mprotect"),
        ("short-arena-rw", "mprotect"),
        ("short-munmap", "munmap"),
        ("short-map-fixed", "mmap"),
        ("short-mremap", "mremap"),
        ("short-mremap-fixed", "mremap"),
        ("zero-old-mremap-fixed", "mremap"),
    ] {
        let error = run_active_footprint_mode(mode)
            .await
            .expect_err("active footprint mutation unexpectedly completed");
        assert!(
            error.to_string().contains(&format!(
                "{syscall} overlaps an active LiteInst hook footprint"
            )),
            "{mode} footprint was not rejected before mutation: {error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pkey_mprotect_is_controller_owned_unless_subscribed() {
    let (_directory, guest) = compile_fixture("hybrid_active_footprint.c");
    let mut command = Command::new(&guest);
    command.arg("pkey-noop");
    let (unsubscribed_output, unsubscribed) = LiteinstBackend::run_host_with_output_and_preload::<
        PassthroughGetpid,
    >(command, (), preload_path())
    .await
    .unwrap();

    let mut command = Command::new(guest);
    command.arg("pkey-noop");
    let (subscribed_output, subscribed) = LiteinstBackend::run_host_with_output_and_preload::<
        ObservePkey,
    >(command, (), preload_path())
    .await
    .unwrap();
    assert_eq!(unsubscribed_output.stdout, subscribed_output.stdout);
    assert_eq!(
        subscribed.delivered.load(Ordering::SeqCst),
        unsubscribed.delivered.load(Ordering::SeqCst) + 1,
        "pkey_mprotect must reach the Tool exactly when subscribed"
    );

    let error = run_active_footprint_mode("pkey-site")
        .await
        .expect_err("destructive pkey_mprotect unexpectedly completed");
    assert!(
        error
            .to_string()
            .contains("pkey_mprotect overlaps an active LiteInst hook footprint"),
        "pkey_mprotect was not rejected before mutation: {error}"
    );
}

async fn cancel_host_wait(capture_output: bool) {
    let (_directory, guest) = compile_fixture("hybrid_wait_cancel.c");
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("guest.pid");
    let mut command = Command::new(guest);
    command.arg(&pid_file);

    let mut run = if capture_output {
        Box::pin(LiteinstBackend::run_host_with_output_and_preload::<
            PassthroughGetpid,
        >(command, (), preload_path()))
            as std::pin::Pin<Box<dyn std::future::Future<Output = _>>>
    } else {
        Box::pin(async move {
            LiteinstBackend::run_host_with_preload::<PassthroughGetpid>(command, (), preload_path())
                .await
                .map(|(status, global)| {
                    (
                        reverie::process::Output {
                            status,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                        global,
                    )
                })
        })
    };
    let wait_for_pid = async {
        loop {
            if let Ok(contents) = fs::read_to_string(&pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let pid = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            result = &mut run => panic!("wait completed before cancellation: {result:?}"),
            pid = wait_for_pid => pid,
        }
    })
    .await
    .expect("guest did not enter the wait phase");
    drop(run);
    assert_pid_reaped(pid);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_wait_reaps_and_unregisters_liteinst_root() {
    cancel_host_wait(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_wait_with_output_reaps_and_unregisters_liteinst_root() {
    cancel_host_wait(true).await;
}

/// Branches from a timer request to its PMU notification. The fixture
/// `hybrid_timer_signal_at_helper.c` retires about this many between each
/// request and the syscall that follows it.
const HELPER_NOTIFICATION_RCBS: u64 = 9_000;

/// The skid margin differs between processors, so the interval is set from
/// it: the notification then comes HELPER_NOTIFICATION_RCBS branches after
/// the request.
static HELPER_REQUEST_RCBS: LazyLock<u64> =
    LazyLock::new(|| reverie_ptrace::PmuConfig::new().skid_margin() + HELPER_NOTIFICATION_RCBS);

#[derive(Debug, Default)]
struct TimerEvents {
    fired: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for TimerEvents {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, _fired: ()) {
        self.fired.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct RequestAtClockGetres;

#[reverie::tool]
impl Tool for RequestAtClockGetres {
    type GlobalState = TimerEvents;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::clock_getres, Sysno::getppid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() == Sysno::clock_getres {
            guest.set_timer_precise(TimerSchedule::Rcbs(*HELPER_REQUEST_RCBS))?;
        }
        guest.tail_inject(syscall).await
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        guest.send_rpc(()).await;
    }
}

/// The timer's counter also counts the branches of the LiteInst patch
/// helper, which runs in the guest at a site's first seccomp stop, so a
/// precise timer's overflow can be raised while the helper runs. That signal
/// is the timer's: it must neither fail the helper nor reach the guest,
/// which has no handler for it.
#[tokio::test(flavor = "current_thread")]
async fn timer_overflow_in_the_patch_helper_is_the_timers() {
    reverie_ptrace::ret_without_perf!();
    let helper_before = reverie_ptrace::testing::liteinst_helper_timer_signals_discarded();
    let injection_before = reverie_ptrace::testing::late_timer_signals_discarded();
    let skid = reverie_ptrace::PmuConfig::new().skid_margin();
    let (_directory, guest) = compile_fixture("hybrid_timer_signal_at_helper.c");
    let mut command = Command::new(guest);
    command.arg(skid.to_string());
    let (output, global) =
        LiteinstBackend::run_host_with_output_and_preload::<RequestAtClockGetres>(
            command,
            (),
            preload_path(),
        )
        .await
        .unwrap();

    assert_eq!(output.stdout, b"rounds=32\n", "{output:?}");
    assert!(output.status.success(), "{output:?}");
    // Each request's target lies past the getppid that follows it. Its
    // signal is discarded before any stop can fire it, or, if it is
    // delivered before getppid, stepping to the target is cut short by
    // getppid's seccomp stop.
    assert_eq!(global.fired.load(Ordering::SeqCst), 0, "{output:?}");
    // In the 4 of every 16 rounds that retire fewer than
    // HELPER_NOTIFICATION_RCBS branches before getppid, the counter reaches
    // its threshold in getppid's helper. In the other rounds it reaches it
    // before getppid. When the processor raises each signal depends on its
    // interrupt latency: a helper round's signal can come after the helper
    // returns, and another round's can come once the helper has started. So
    // the count is not required to be exactly 8. A signal already pending at
    // the getppid stop is taken during the CPUID policy injection instead.
    let helper = reverie_ptrace::testing::liteinst_helper_timer_signals_discarded() - helper_before;
    let injection = reverie_ptrace::testing::late_timer_signals_discarded() - injection_before;
    eprintln!(
        "timer signals discarded in the patch helper: {helper}, at injected syscalls: {injection}"
    );
    assert!(
        helper > 0,
        "no helper run had the timer's signal: {output:?}"
    );
}

/// Runs `hybrid_stepped_hook.c` with a precise timer `rcbs` RCBs past each
/// getpid. Returns the guest's output, the number of timer events, and the RCBs
/// from each getpid to the next.
async fn run_stepped_hook(rcbs: u64) -> (String, u64, Vec<u64>) {
    let (_directory, guest) = compile_fixture("hybrid_stepped_hook.c");
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(60),
        LiteinstBackend::run_host_with_output_and_preload::<SteppedHookTool>(
            Command::new(guest),
            rcbs,
            preload_path(),
        ),
    )
    .await
    .expect("the stepped hook guest did not complete")
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    let clocks = global.getpid_clocks.into_inner().unwrap();
    let distances = clocks.windows(2).map(|pair| pair[1] - pair[0]).collect();
    (
        String::from_utf8(output.stdout).unwrap(),
        global.timer_events.load(Ordering::SeqCst),
        distances,
    )
}

// A precise timer single-steps the guest toward its target, and the steps must
// pass a LiteInst hook's `int3` on to Reverie. If they took the trap's stop for
// a step's, the hook's syscall would not reach the Tool: the getpid would
// return its syscall number, 39, and the timer would fire as if no hook had run.
//
// Each getpid's timer is 200 RCBs away, and the next getpid's hook traps 111
// RCBs later in a debug build on an AMD EPYC 9D85. 200 RCBs is within the skid
// margin of every AMD processor in Reverie's PMU table, so there the timer is
// delivered with an artificial signal, the steps start at the getpid, and they
// run the next hook's trap. On Intel the timer uses the PMU, and the steps
// start at most 100 or 125 RCBs before the target.
#[tokio::test(flavor = "current_thread")]
async fn a_hook_trap_in_the_timer_steps_reaches_the_tool() {
    reverie_ptrace::ret_without_perf!();
    let rcbs = 200;
    let (stdout, timer_events, distances) = run_stepped_hook(rcbs).await;
    assert_eq!(
        stdout, "calls=64 wrong=0 last_wrong=0\n",
        "timer_events={timer_events} distances={distances:?}"
    );
    assert_eq!(distances.len(), 63);
    assert!(
        distances.iter().all(|&distance| distance < rcbs),
        "each hook must trap before the previous getpid's timer target: {distances:?}"
    );
    // Each hook's trap cancels the previous getpid's timer, and only the last
    // getpid's timer fires, after the loop.
    assert_eq!(timer_events, 1);
}

// The control: each getpid's timer is 50 RCBs away, before the next hook's
// trap, so every timer fires. 50 RCBs is within every skid margin in the table,
// so the steps start at the getpid on every host.
#[tokio::test(flavor = "current_thread")]
async fn a_timer_before_the_next_hook_trap_fires() {
    reverie_ptrace::ret_without_perf!();
    let rcbs = 50;
    let (stdout, timer_events, distances) = run_stepped_hook(rcbs).await;
    assert_eq!(
        stdout, "calls=64 wrong=0 last_wrong=0\n",
        "timer_events={timer_events} distances={distances:?}"
    );
    assert_eq!(distances.len(), 63);
    assert!(
        distances.iter().all(|&distance| distance > rcbs),
        "each getpid's timer target must come before the next hook's trap: {distances:?}"
    );
    assert_eq!(timer_events, 64);
}

/// What `UnsubscribedHookTimerTool` tells its global state.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
enum UnsubscribedHookTimerNote {
    Getpid,
    /// The guest's RCBs from the latest getpid to a getppid, when the Tool
    /// subscribes it.
    Getppid(u64),
    /// The guest's RCBs from the latest getpid to a timer event.
    TimerEvent(u64),
}

#[derive(Debug, Default)]
struct UnsubscribedHookTimerEvents {
    getpids: AtomicU64,
    getppids: std::sync::Mutex<Vec<u64>>,
    timer_events: std::sync::Mutex<Vec<u64>>,
}

/// The configuration of `UnsubscribedHookTimerTool`.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct UnsubscribedHookTimerConfig {
    /// The timer's RCBs from each getpid to its target.
    rcbs: u64,
    /// Whether the Tool also subscribes getppid, to measure where it traps.
    getppid: bool,
}

#[reverie::global_tool]
impl GlobalTool for UnsubscribedHookTimerEvents {
    type Request = UnsubscribedHookTimerNote;
    type Response = ();
    type Config = UnsubscribedHookTimerConfig;

    async fn receive_rpc(&self, _from: Tid, note: UnsubscribedHookTimerNote) {
        match note {
            UnsubscribedHookTimerNote::Getpid => {
                self.getpids.fetch_add(1, Ordering::SeqCst);
            }
            UnsubscribedHookTimerNote::Getppid(rcbs) => self.getppids.lock().unwrap().push(rcbs),
            UnsubscribedHookTimerNote::TimerEvent(rcbs) => {
                self.timer_events.lock().unwrap().push(rcbs)
            }
        }
    }
}

/// Answers every getpid itself, with 0x4242, and requests a precise timer
/// there. It subscribes nothing else, unless configured to measure getppid.
#[derive(Default)]
struct UnsubscribedHookTimerTool;

#[reverie::tool]
impl Tool for UnsubscribedHookTimerTool {
    type GlobalState = UnsubscribedHookTimerEvents;
    /// The guest's RCB clock at the latest getpid.
    type ThreadState = u64;

    fn subscriptions(config: &UnsubscribedHookTimerConfig) -> Subscription {
        if config.getppid {
            [Sysno::getpid, Sysno::getppid].into_iter().collect()
        } else {
            [Sysno::getpid].into_iter().collect()
        }
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::getpid => {
                *guest.thread_state_mut() = guest.read_clock()?;
                guest.send_rpc(UnsubscribedHookTimerNote::Getpid).await;
                guest.set_timer_precise(TimerSchedule::Rcbs(guest.config().rcbs))?;
                Ok(0x4242)
            }
            Sysno::getppid => {
                let rcbs = guest.read_clock()? - *guest.thread_state();
                guest
                    .send_rpc(UnsubscribedHookTimerNote::Getppid(rcbs))
                    .await;
                guest.tail_inject(syscall).await
            }
            other => panic!("unsubscribed {other}"),
        }
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let rcbs = guest.read_clock().unwrap() - *guest.thread_state();
        guest
            .send_rpc(UnsubscribedHookTimerNote::TimerEvent(rcbs))
            .await;
    }
}

/// Checks that each of a run's precise timer events that a PMU notification
/// delivered fired at its target, `rcbs` RCBs past its request, except where
/// Reverie witnessed a skid overshoot: `witnesses` is the change in
/// `reverie::take_skid_overshoot_count` over the run.
///
/// A precise event is delivered by single steps that start when its PMU
/// notification arrives, a skid margin before the target. The processor's
/// interrupt latency occasionally exceeds the margin, and then the guest has
/// passed the target when the notification arrives. Reverie delivers the
/// event late and counts it as a skid overshoot, and prints the
/// `HERMIT_SKID_OVERSHOOT` marker, by which Hermit refuses such a run as
/// nondeterministic. With a margin of 1000 RCBs on AMD EPYC 9D85, a scratch
/// probe of the rt_sigreturn run with its trap 5000 RCBs past the request
/// measured this in 2 of 768 rounds, at 897 and 1846 RCBs past the target,
/// both delivered by the notification in the normal signal path with no
/// LiteInst trap between the request and the target.
///
/// So an event may fire past its target only if Reverie witnessed it, once:
/// the number of events past the target must equal `witnesses`. The caller
/// must also check that no event was cancelled past its target, which would
/// be witnessed too. Never before the target.
///
/// The count is process global. The workspace runs its tests with
/// `--test-threads=1`, which these checks require.
fn assert_at_target_unless_witnessed(events: &[u64], rcbs: u64, witnesses: u64) {
    assert!(
        events.iter().all(|&event| event >= rcbs),
        "no event may fire before its target {rcbs}: {events:?}"
    );
    let late = events.iter().filter(|&&event| event > rcbs).count() as u64;
    assert_eq!(
        late, witnesses,
        "every event past its target {rcbs}, and nothing else, must be a witnessed skid \
         overshoot: {events:?}"
    );
    if late > 0 {
        let overshoots: Vec<u64> = events
            .iter()
            .filter(|&&event| event > rcbs)
            .map(|&event| event - rcbs)
            .collect();
        eprintln!(
            "{late} of {} events fired past the target {rcbs} with a witnessed skid \
             overshoot of {overshoots:?}",
            events.len()
        );
    }
}

/// Runs `hybrid_unsubscribed_hook_timer.c` with a precise timer `rcbs` RCBs
/// past each getpid, which the guest follows with `before + i % leads`
/// branches in round `i`, a getppid through the same patched site, and
/// `after` branches. Returns the RCBs from the getpid to each timer event,
/// and, if the Tool subscribes `getppid`, to each getppid.
async fn run_unsubscribed_hook_timer(
    config: UnsubscribedHookTimerConfig,
    before: u64,
    leads: u64,
    rounds: u64,
    after: u64,
) -> (Vec<u64>, Vec<u64>) {
    let (_directory, guest) = compile_fixture("hybrid_unsubscribed_hook_timer.c");
    let mut command = Command::new(guest);
    command.args([
        before.to_string(),
        leads.to_string(),
        rounds.to_string(),
        after.to_string(),
    ]);
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(120),
        LiteinstBackend::run_host_with_output_and_preload::<UnsubscribedHookTimerTool>(
            command,
            config,
            preload_path(),
        ),
    )
    .await
    .expect("the unsubscribed hook guest did not complete")
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rounds={rounds} wrong=0\n")
    );
    assert_eq!(global.getpids.load(Ordering::SeqCst), rounds + 1);
    (
        global.timer_events.into_inner().unwrap(),
        global.getppids.into_inner().unwrap(),
    )
}

/// Runs the guest with no timer target in reach, `rcbs` RCBs past each
/// getpid, `before` branches from each getpid to its getppid, and the Tool
/// subscribed to getppid.
async fn run_unsubscribed_hook_timer_steps(rcbs: u64, before: u64) -> Vec<u64> {
    let config = UnsubscribedHookTimerConfig {
        rcbs,
        getppid: false,
    };
    let rounds = 16;
    let (events, getppids) = run_unsubscribed_hook_timer(config, before, 1, rounds, 2 * rcbs).await;
    assert!(getppids.is_empty());
    assert_eq!(events.len(), rounds as usize + 1, "{events:?}");
    events
}

// A getppid through a patched site is a LiteInst hook trap that the Tool does
// not observe, because it subscribes only getpid. Without LiteInst the
// getppid would make no stop at all, so the trap must not change the timer
// event that the getpid before it requested: every round's event fires once,
// at its target, in the branches after the getppid.
//
// Each target is 200 RCBs past its getpid, and the getppid's trap 100 RCBs
// and the hook's own branches past it. 200 RCBs is within the skid margin of
// every AMD processor in Reverie's PMU table, so there the timer is delivered
// with an artificial signal, the steps start at the getpid, and the trap
// interrupts them. On Intel the steps start at most 100 or 125 RCBs before
// the target.
#[tokio::test(flavor = "current_thread")]
async fn an_unsubscribed_hook_trap_in_the_timer_steps_keeps_the_event() {
    reverie_ptrace::ret_without_perf!();
    let rcbs = 200;
    let events = run_unsubscribed_hook_timer_steps(rcbs, 100).await;
    assert!(events.iter().all(|&event| event == rcbs), "{events:?}");
}

// The control: the target is 50 RCBs past each getpid, before the getppid, so
// the steps reach it before the trap.
#[tokio::test(flavor = "current_thread")]
async fn a_timer_before_an_unsubscribed_hook_trap_fires() {
    reverie_ptrace::ret_without_perf!();
    let rcbs = 50;
    let events = run_unsubscribed_hook_timer_steps(rcbs, 100).await;
    assert!(events.iter().all(|&event| event == rcbs), "{events:?}");
}

// The timer's PMU notification can be pending at the unsubscribed hook trap,
// when the counter overflows a few branches before it and the processor's
// interrupt latency lands the signal after the trap. The notification then
// reaches the syscall that Reverie injects for the hook, before any stop has
// decided the event. It is the event's own: the event must fire at its
// target once the syscall returns, and not be taken for a late notification
// of a cancelled event and discarded.
//
// A first run, with the Tool subscribed to getppid, measures how many
// branches past the guest's loop the getppid traps. In the second run each
// round's counter then overflows one of `leads` distances before the trap, as
// in reverie-ptrace/tests/late_timer_signal.rs. Whichever side of the trap
// the signal comes, the event must fire at its target.
#[tokio::test(flavor = "current_thread")]
async fn a_timer_notification_at_an_unsubscribed_hook_syscall_fires_the_event() {
    reverie_ptrace::ret_without_perf!();
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    let period = 10_000;
    let leads = 80.min(margin / 4);
    let rounds = 200;
    let rcbs = period + margin;
    let measure = UnsubscribedHookTimerConfig {
        rcbs: 4 * period,
        getppid: true,
    };
    let (_, getppids) = run_unsubscribed_hook_timer(measure, period, 1, 4, 1).await;
    // The guest's first getppid precedes every getpid, and the site's first
    // getpid reaches the Tool through seccomp rather than the hook.
    assert_eq!(getppids.len(), 6, "{getppids:?}");
    let hook = getppids[2] - period;
    assert!(
        getppids[2..].iter().all(|&rcbs| rcbs == period + hook),
        "the getppid must trap at one distance past the loop: {getppids:?}"
    );
    assert!(
        hook + leads < margin,
        "the trap must come before the target: {hook} branches past the loop"
    );
    eprintln!("the getppid traps {hook} branches past the loop");
    let config = UnsubscribedHookTimerConfig {
        rcbs,
        getppid: false,
    };
    // These counts are process global, so the deltas are this run's only
    // with `--test-threads=1`, as the workspace runs its tests.
    let live_before = reverie_ptrace::testing::live_timer_signals_taken();
    let late_before = reverie_ptrace::testing::late_timer_signals_discarded();
    let _ = reverie::take_skid_overshoot_count();
    let (events, _) =
        run_unsubscribed_hook_timer(config, period - hook - leads / 2, leads, rounds, 2 * margin)
            .await;
    let witnesses = reverie::take_skid_overshoot_count();
    let live = reverie_ptrace::testing::live_timer_signals_taken() - live_before;
    let late = reverie_ptrace::testing::late_timer_signals_discarded() - late_before;
    eprintln!("notifications taken at injected syscalls: {live}, discarded as late: {late}");
    // Every round's event fires, none is lost or cancelled, and each at its
    // target unless its notification came too late (see
    // `assert_at_target_unless_witnessed`).
    assert_eq!(events.len(), rounds as usize + 1, "{events:?}");
    assert_at_target_unless_witnessed(&events, rcbs, witnesses);
    assert_eq!(late, 0, "no notification here is late");
    // Otherwise no round tested the notification at the injection.
    assert!(live > 0, "no notification reached an injected syscall");
}

#[derive(Debug, Default)]
struct SigreturnHookTimerEvents {
    requests: AtomicU64,
    /// The round of each timer event's request, and the guest's RCBs from
    /// the request to the event.
    timer_events: std::sync::Mutex<Vec<(u64, u64)>>,
}

#[reverie::global_tool]
impl GlobalTool for SigreturnHookTimerEvents {
    /// A timer event's round and RCBs from its request, or `None` for a
    /// request.
    type Request = Option<(u64, u64)>;
    type Response = ();
    /// The timer's RCBs from each request to its target.
    type Config = u64;

    async fn receive_rpc(&self, _from: Tid, note: Option<(u64, u64)>) {
        match note {
            None => {
                self.requests.fetch_add(1, Ordering::SeqCst);
            }
            Some(event) => self.timer_events.lock().unwrap().push(event),
        }
    }
}

/// Answers every getpid itself, with 0x4242, and requests a precise timer at
/// a getpid whose first argument is 1, for the round in its second. It
/// subscribes nothing else.
#[derive(Default)]
struct SigreturnHookTimerTool;

#[reverie::tool]
impl Tool for SigreturnHookTimerTool {
    type GlobalState = SigreturnHookTimerEvents;
    /// The guest's RCB clock at the latest request, and its round.
    type ThreadState = (u64, u64);

    fn subscriptions(_config: &u64) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        let (_, args) = syscall.into_parts();
        if args.arg0 == 1 {
            *guest.thread_state_mut() = (guest.read_clock()?, args.arg1 as u64);
            guest.send_rpc(None).await;
            guest.set_timer_precise(TimerSchedule::Rcbs(*guest.config()))?;
        }
        Ok(0x4242)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let (clock, round) = *guest.thread_state();
        let rcbs = guest.read_clock().unwrap() - clock;
        guest.send_rpc(Some((round, rcbs))).await;
    }
}

/// Runs `hybrid_sigreturn_hook_timer.c` for `rounds` signals, with a precise
/// timer `rcbs` RCBs past the request in each handler, which returns
/// `before + (i % leads) * stride` branches after it in round `i`, and
/// `after` branches after each signal. Returns each timer event's round and
/// RCBs from its request, and the skid overshoots Reverie witnessed.
async fn run_sigreturn_hook_timer(
    rcbs: u64,
    before: u64,
    leads: u64,
    stride: u64,
    rounds: u64,
    after: u64,
) -> (Vec<(u64, u64)>, u64) {
    let (_directory, guest) = compile_fixture("hybrid_sigreturn_hook_timer.c");
    let mut command = Command::new(guest);
    command.args([
        before.to_string(),
        rounds.to_string(),
        after.to_string(),
        leads.to_string(),
        stride.to_string(),
    ]);
    // Process global; see `assert_at_target_unless_witnessed`.
    let _ = reverie::take_skid_overshoot_count();
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(120),
        LiteinstBackend::run_host_with_output_and_preload::<SigreturnHookTimerTool>(
            command,
            rcbs,
            preload_path(),
        ),
    )
    .await
    .expect("the rt_sigreturn hook guest did not complete")
    .unwrap();
    let witnesses = reverie::take_skid_overshoot_count();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rounds={rounds} handled={rounds} wrong=0\n")
    );
    assert_eq!(global.requests.load(Ordering::SeqCst), rounds);
    let events = global.timer_events.into_inner().unwrap();
    // One event per request at most, in order.
    assert!(
        events.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "{events:?}"
    );
    (events, witnesses)
}

// A signal handler's restorer that makes its rt_sigreturn through a patched
// site traps into LiteInst, and Reverie makes the rt_sigreturn for the hook
// without a Tool callback. Without LiteInst the rt_sigreturn would make no
// stop, so the trap must not change the timer event that the handler
// requested: each handler requests an event whose PMU notification comes
// after the rt_sigreturn, and it must fire at its target after the handler
// has returned.
#[tokio::test(flavor = "current_thread")]
async fn an_rt_sigreturn_hook_trap_keeps_the_timer_event() {
    reverie_ptrace::ret_without_perf!();
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    let period = 10_000;
    let rcbs = period + margin;
    let rounds = 16;
    let (events, witnesses) =
        run_sigreturn_hook_timer(rcbs, period / 2, 1, 0, rounds, 2 * rcbs).await;
    assert_eq!(
        events.iter().map(|&(round, _)| round).collect::<Vec<_>>(),
        (0..rounds).collect::<Vec<_>>(),
        "every round's event must fire: {events:?}"
    );
    let clocks: Vec<u64> = events.iter().map(|&(_, rcbs)| rcbs).collect();
    assert_at_target_unless_witnessed(&clocks, rcbs, witnesses);
}

/// A bound on the branches from the return of `hybrid_sigreturn_hook_timer.c`'s
/// handler to the rt_sigreturn hook's trap. They are those of the handler's
/// epilogue, the restorer and the hook's trampoline. On devbig014, an AMD
/// EPYC 9D85, the event is cancelled from 109 branches before the period.
const SIGRETURN_TRAP_BRANCHES: u64 = 200;

/// Sweeps the rt_sigreturn hook's trap across `leads` distances from the
/// handler's request, `before + j * stride` branches for `j` in `0..leads`,
/// `repeats` times each, with the timer's target past the last trap.
///
/// Nothing of a timer event is continued across the context switch of an
/// rt_sigreturn hook trap: an event whose programming has passed its period
/// at the trap is cancelled, and one whose programming has not is kept, to be
/// delivered by its notification after the handler returns. Whether the
/// notification of a programming past its period has arrived at the trap,
/// and whether its single steps have begun, depends on the processor's
/// interrupt latency, so the rule must not: the guest's clock at the trap
/// alone decides the event's fate.
///
/// So every repeat of a distance must meet one fate, and the fate must change
/// exactly once, from firing to cancelled, where the trap reaches the period.
/// Every trap is short of the target, so no cancellation is a witnessed skid
/// overshoot, and every fired event is at its target unless witnessed (see
/// `assert_at_target_unless_witnessed`). Returns the last distance at which
/// the event fired.
async fn sweep_sigreturn_hook_trap(before: u64, leads: u64, stride: u64, repeats: u64) -> u64 {
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    let period = 10_000;
    let rcbs = period + margin;
    assert!(
        before < period && before + (leads - 1) * stride + SIGRETURN_TRAP_BRANCHES < rcbs,
        "the sweep must start before the period, and every trap come before the target"
    );
    let rounds = leads * repeats;
    let (events, witnesses) =
        run_sigreturn_hook_timer(rcbs, before, leads, stride, rounds, 2 * rcbs).await;
    let fired: std::collections::BTreeMap<u64, u64> = events.iter().copied().collect();
    let fates: Vec<bool> = (0..leads)
        .map(|j| {
            let fate = fired.contains_key(&j);
            for repeat in 1..repeats {
                assert_eq!(
                    fired.contains_key(&(j + repeat * leads)),
                    fate,
                    "every trap {} branches past the handler's request must meet one fate: \
                     fired in rounds {:?}",
                    before + j * stride,
                    fired.keys().collect::<Vec<_>>()
                );
            }
            fate
        })
        .collect();
    let boundary = fates.iter().position(|&fate| !fate).unwrap_or(fates.len());
    assert!(
        boundary > 0 && boundary < fates.len(),
        "the sweep must cross the period: {fates:?}"
    );
    assert!(
        fates[boundary..].iter().all(|&fate| !fate),
        "a trap past the period must cancel the event, and one before it keep it: {fates:?}"
    );
    let last_fired = before + (boundary as u64 - 1) * stride;
    eprintln!(
        "the event fired with the handler returning up to {last_fired} branches after its \
         request, and was cancelled from {}",
        last_fired + stride
    );
    let clocks: Vec<u64> = events.iter().map(|&(_, rcbs)| rcbs).collect();
    assert_at_target_unless_witnessed(&clocks, rcbs, witnesses);
    last_fired
}

// The trap one branch apart across the period, where interrupt latency
// decides whether its notification has arrived at the trap.
#[tokio::test(flavor = "current_thread")]
async fn an_rt_sigreturn_hook_trap_decides_the_timer_by_the_guest_clock() {
    reverie_ptrace::ret_without_perf!();
    let before = 10_000 - 256;
    sweep_sigreturn_hook_trap(before, 288, 1, 2).await;
}

// The trap from a skid margin before the period up to the target, where the
// notification may have arrived and its single steps begun.
#[tokio::test(flavor = "current_thread")]
async fn an_rt_sigreturn_hook_trap_up_to_the_target_cancels_the_timer() {
    reverie_ptrace::ret_without_perf!();
    const LEADS: u64 = 64;
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    let period = 10_000;
    let before = period - margin.min(period / 2);
    let stride = (period + margin - SIGRETURN_TRAP_BRANCHES - before) / LEADS;
    sweep_sigreturn_hook_trap(before, LEADS, stride, 4).await;
}

// A LiteInst thread whose timer event is overtaken, and which then exits with
// the event undecided. The guest blocks the timer's signal, requests the
// event at getpid, runs twice its RCBs, and makes a getppid and an exit_group
// through the patched site. Each is a hook trap that the Tool does not see,
// so neither decides the event, and the thread's exit ends it. The event was
// due before the first trap, so Reverie must witness one skid overshoot, as
// it does when a stop the Tool sees overtakes a due event, and nothing must
// fire.
#[tokio::test(flavor = "current_thread")]
async fn an_exit_through_a_hook_trap_past_the_target_is_witnessed() {
    reverie_ptrace::ret_without_perf!();
    let rcbs = 10_000 + reverie_ptrace::PmuConfig::new().skid_margin();
    let (_directory, guest) = compile_fixture("hybrid_exit_after_hook_trap.c");
    let mut command = Command::new(guest);
    command.arg((2 * rcbs).to_string());
    // Process global; see `assert_at_target_unless_witnessed`.
    let _ = reverie::take_skid_overshoot_count();
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(60),
        LiteinstBackend::run_host_with_output_and_preload::<SigreturnHookTimerTool>(
            command,
            rcbs,
            preload_path(),
        ),
    )
    .await
    .expect("the exiting guest did not complete")
    .unwrap();
    let witnesses = reverie::take_skid_overshoot_count();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(global.requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        global.timer_events.into_inner().unwrap(),
        Vec::<(u64, u64)>::new(),
        "no notification can deliver the event"
    );
    assert_eq!(
        witnesses, 1,
        "the exit must settle the overtaken event, witnessed once"
    );
}
