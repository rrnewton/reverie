/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Plain ptrace regression checks on real guests.
//!
//! Two fixtures run under plain ptrace with a recording Tool:
//!
//! - `tests/fixtures/plain_fork_vfork_exec.c` forks, vforks, raises a signal
//!   and execs itself. Its runs are checked for every stop class, for the
//!   PID-bearing return values the Tool sees, and for being repeatable PID
//!   for PID in a fresh PID namespace.
//! - `tests/fixtures/plain_guest_regression.c` runs one mode per test and
//!   reports its results, registers and signal frames to a file. Each test
//!   checks that report, the stop sequence, the signal-delivery stops and the
//!   Tool-visible events of the plain run.
//!
//! The stop sequence is the tracer's run-loop waits (the stops that
//! `record_wait` sees). Stops consumed inside `inject`, `tail_inject` or
//! single-step paths are not recorded.
//!
//! These are the plain ptrace arms of the deleted trap-only LiteInst parity
//! tests, which compared each of these runs with a run of the deleted
//! backend; the plain assertions are kept as they were.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::sync::Mutex;

use reverie::Guest;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use serde::Deserialize;
use serde::Serialize;

use super::*;
use crate::PtraceBackendStatsSnapshot;

fn tempfile_path(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Fixed width: a guest that opens this path executes a number of
    // branches that depends on its length.
    let path = std::env::temp_dir().join(format!(
        "reverie-{label}-{}-{serial:08}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[derive(Default)]
struct Log(Mutex<Vec<(Pid, String)>>);

#[reverie::global_tool]
impl GlobalTool for Log {
    /// `true` records every syscall's return value (through `inject`);
    /// `false` behaves as the default Tool (through `tail_inject`).
    type Config = bool;
    type Request = String;
    type Response = ();

    async fn receive_rpc(&self, from: Pid, event: String) {
        self.0.lock().unwrap().push((from, event));
    }
}

/// Records every Tool-visible event. With the default configuration it
/// otherwise behaves as the default Tool; with `true` it also records each
/// syscall's return value.
#[derive(Default)]
struct RecordTool;

#[reverie::tool]
impl Tool for RecordTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_config: &bool) -> Subscription {
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
        // exit and a successful execve never return a value to the caller.
        let no_return = matches!(
            call,
            Syscall::Exit(_) | Syscall::ExitGroup(_) | Syscall::Execve(_) | Syscall::Execveat(_)
        );
        if !*guest.config() || no_return {
            guest.send_rpc(format!("syscall {}", call.number())).await;
            guest.tail_inject(call).await
        } else {
            let result = guest.inject(call).await;
            let value = match result {
                Ok(value) => value,
                Err(errno) => -i64::from(errno.into_raw()),
            };
            guest
                .send_rpc(format!("syscall {} = {value}", call.number()))
                .await;
            Ok(result?)
        }
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

/// Names a prebuilt fork/vfork/exec guest for a re-executed namespace arm.
const FORK_VFORK_EXEC_GUEST_ENV: &str = "REVERIE_PLAIN_FORK_VFORK_EXEC_GUEST";

fn fork_vfork_exec_guest() -> &'static std::path::Path {
    static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
        if let Some(prebuilt) = std::env::var_os(FORK_VFORK_EXEC_GUEST_ENV) {
            return PathBuf::from(prebuilt);
        }
        let source = crate::tracer::tests::fixture("plain_fork_vfork_exec.c");
        // One fixture beside the test binary, inside the build tree, rather
        // than one leaked file per test process under /tmp. Each process
        // compiles its own copy and renames it into place, so the source is
        // never stale and a concurrent process still running the previous
        // copy keeps its inode.
        let directory = std::env::current_exe()
            .expect("locate the test binary")
            .parent()
            .expect("the test binary has a directory")
            .to_path_buf();
        let output = directory.join("reverie-plain-fork-vfork-exec");
        let staging = directory.join(format!(
            "reverie-plain-fork-vfork-exec.{}.tmp",
            std::process::id()
        ));
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g"])
            .arg(&source)
            .arg("-o")
            .arg(&staging)
            .status()
            .expect("invoke cc for the fork/vfork/exec fixture");
        assert!(status.success(), "compile {}", source.display());
        std::fs::rename(&staging, &output).expect("publish the fork/vfork/exec fixture");
        output
    });
    GUEST.as_path()
}

/// Syscalls whose positive return value is a PID or TID, by the name the
/// Tool records.
const PID_RETURNING: [&str; 8] = [
    "getpid",
    "gettid",
    "set_tid_address",
    "clone",
    "clone3",
    "fork",
    "vfork",
    "wait4",
];

/// How a trace names the tasks and PID values it contains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Identity {
    /// Run in the host PID namespace: PIDs differ from run to run, so each
    /// traced PID, wherever it appears, is renamed `task#N` in order of first
    /// appearance. The relation between tasks and PID values is kept.
    Renamed,
    /// Run in a fresh PID namespace: PIDs are compared exactly.
    Exact,
}

/// Renames the traced PIDs in a stop trace and a Tool event trace, and groups
/// both per task.
struct TaskNames(BTreeMap<Pid, String>);

impl TaskNames {
    fn new(identity: Identity, stops: &[(Pid, String)]) -> Self {
        let mut names = BTreeMap::new();
        for (pid, _) in stops {
            let next = names.len();
            names.entry(*pid).or_insert_with(|| match identity {
                Identity::Renamed => format!("task#{next}"),
                Identity::Exact => format!("pid {pid}"),
            });
        }
        Self(names)
    }

    fn name(&self, pid: Pid) -> String {
        self.0
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| format!("untraced pid {pid}"))
    }

    fn value(&self, raw: &str) -> String {
        match raw.parse::<i32>() {
            Ok(value) if value > 0 => self
                .0
                .get(&Pid::from_raw(value))
                .cloned()
                .unwrap_or_else(|| raw.to_owned()),
            _ => raw.to_owned(),
        }
    }

    /// Replaces PID values inside one event. In [`Identity::Exact`] mode the
    /// names are the PIDs themselves, so only the spelling changes.
    fn event(&self, event: &str) -> String {
        if let Some((operation, pid)) = event
            .strip_prefix("new-child ")
            .and_then(|rest| rest.rsplit_once(' '))
        {
            return format!("new-child {operation} {}", self.value(pid));
        }
        if let Some((number, value)) = event
            .strip_prefix("syscall ")
            .and_then(|rest| rest.split_once(" = "))
            && PID_RETURNING.contains(&number)
        {
            return format!("syscall {number} = {}", self.value(value));
        }
        event.to_owned()
    }

    fn per_task(&self, trace: Vec<(Pid, String)>) -> BTreeMap<String, Vec<String>> {
        let mut tasks = BTreeMap::<String, Vec<String>>::new();
        for (pid, event) in trace {
            tasks
                .entry(self.name(pid))
                .or_default()
                .push(self.event(&event));
        }
        tasks
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Observed {
    status: ExitStatus,
    /// Name of the first task the tracer saw.
    root: String,
    tool_events: BTreeMap<String, Vec<String>>,
    stops: BTreeMap<String, Vec<String>>,
    counts: PtraceBackendStatsSnapshot,
}

async fn run_fork_vfork_exec_guest(record_values: bool, identity: Identity) -> Observed {
    let tracer = TracerBuilder::<RecordTool>::new(Command::new(fork_vfork_exec_guest()))
        .config(record_values)
        .backend_stats(BackendStatsRequest::ENABLED)
        .spawn()
        .await
        .expect("spawn the fork/vfork/exec guest");
    let stats = tracer.backend_stats().expect("stats were requested");
    let (status, log) = tokio::time::timeout(Duration::from_secs(20), tracer.wait())
        .await
        .expect("fork/vfork/exec guest timed out")
        .expect("fork/vfork/exec guest run failed");
    let stop_trace = stats.stop_trace();
    let names = TaskNames::new(identity, &stop_trace);
    let root = names.name(stop_trace.first().expect("no stop was recorded").0);
    Observed {
        status,
        root,
        tool_events: names.per_task(std::mem::take(&mut *log.0.lock().unwrap())),
        stops: names.per_task(stop_trace),
        counts: reverie::BackendStatsSource::backend_stats(&stats),
    }
}

/// Checks that a plain-ptrace run of the fork/vfork/exec guest has every
/// class of stop and Tool event the guest exercises, so that a comparison
/// against it is not vacuous.
///
/// With `record_values`, the Tool resumes syscalls through `inject`, which
/// consumes the fork and vfork event stops itself, so the run loop never sees
/// them; the children's identities are then required in the return values.
fn assert_baseline_is_not_vacuous(
    ptrace: &Observed,
    record_values: bool,
    root: &str,
    fork: &str,
    vfork: &str,
) {
    assert_eq!(ptrace.status, ExitStatus::Exited(0));
    assert_eq!(ptrace.root, root);
    let all_stops = ptrace.stops.values().flatten().collect::<Vec<_>>();
    let required_stops = if record_values {
        vec!["exec".to_owned(), "Signal(SIGUSR1)".to_owned()]
    } else {
        vec![
            "exec".to_owned(),
            format!("new-child Fork {fork}"),
            format!("new-child Vfork {vfork}"),
            // The parent is still inside the tail-injected vfork, with the
            // entry's -ENOSYS in rax.
            "VforkDone rax=-38".to_owned(),
            "Signal(SIGUSR1)".to_owned(),
        ]
    };
    for required in required_stops {
        assert!(
            all_stops.iter().any(|stop| **stop == required),
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
    // The fixture keeps SIGCHLD blocked from before each fork and vfork until
    // after the matching waitpid, so each child's SIGCHLD is delivered at a
    // fixed point: when the rt_sigprocmask that follows the parent's wait4
    // restores the old mask, never before the wait4.
    let root_stops = ptrace
        .stops
        .get(root)
        .unwrap_or_else(|| panic!("no stop sequence for root {root}"));
    let sigchld_stops = root_stops
        .iter()
        .filter(|stop| *stop == "Signal(SIGCHLD)")
        .count();
    let sigchld_after_wait4 = root_stops
        .windows(3)
        .filter(|stops| stops == &["seccomp 61", "seccomp 14", "Signal(SIGCHLD)"])
        .count();
    assert_eq!(
        (sigchld_stops, sigchld_after_wait4),
        (2, 2),
        "root's two SIGCHLD stops must each follow its wait4 and unblock stops: {root_stops:?}"
    );
    let mut expected_tasks = vec![root.to_owned(), fork.to_owned(), vfork.to_owned()];
    expected_tasks.sort();
    assert_eq!(
        ptrace.stops.keys().cloned().collect::<Vec<_>>(),
        expected_tasks,
        "root, fork child, and vfork child each own a stop sequence"
    );
    let all_events = ptrace.tool_events.values().flatten().collect::<Vec<_>>();
    assert!(all_events.iter().any(|event| *event == "post-exec"));
    assert!(all_events.iter().any(|event| *event == "signal SIGUSR1"));
    if record_values {
        let values = all_events
            .iter()
            .filter(|event| event.contains(" = "))
            .count();
        assert!(values > 50, "only {values} syscall return values recorded");
        let root_events = &ptrace.tool_events[root];
        for expected in [
            format!("syscall set_tid_address = {root}"),
            format!("syscall getpid = {root}"),
            format!("syscall gettid = {root}"),
            format!("syscall clone = {fork}"),
            format!("syscall wait4 = {fork}"),
            format!("syscall vfork = {vfork}"),
            format!("syscall wait4 = {vfork}"),
        ] {
            assert!(
                root_events.contains(&expected),
                "root lacks {expected}: {root_events:?}"
            );
        }
    }
}

/// A plain ptrace run of the fork/vfork/exec guest with the default
/// (tail-injecting) Tool has its fork, vfork and exec stops, the vfork-done
/// stop with the entry's -ENOSYS in rax, the delivered SIGUSR1, each child's
/// SIGCHLD right after the parent's wait4 and unblock, and a stop sequence
/// for each of its three tasks.
#[tokio::test(flavor = "current_thread")]
async fn plain_fork_vfork_exec_run_has_every_stop_class() {
    let ptrace = run_fork_vfork_exec_guest(false, Identity::Renamed).await;
    assert_baseline_is_not_vacuous(&ptrace, false, "task#0", "task#1", "task#2");
}

/// The same run with every syscall's return value recorded: the root's
/// set_tid_address, getpid, gettid, clone, vfork and wait4 return the PIDs of
/// the tasks the tracer saw. PID values are renamed consistently with task
/// identity, because host PIDs differ between runs.
#[tokio::test(flavor = "current_thread")]
async fn plain_fork_vfork_exec_run_returns_the_traced_pids() {
    let ptrace = run_fork_vfork_exec_guest(true, Identity::Renamed).await;
    assert_baseline_is_not_vacuous(&ptrace, true, "task#0", "task#1", "task#2");
}

/// Selects the arm a re-executed namespace child runs.
const NAMESPACE_ARM_ENV: &str = "REVERIE_PLAIN_NAMESPACE_ARM";
const NAMESPACE_MARK: &str = "@@plain-namespace-arm@@ ";

/// Runs one arm in a fresh user, PID and mount namespace with its own /proc,
/// the way Hermit runs the tracer, and returns the child's printed record.
fn run_arm_in_fresh_pid_namespace(test_name: &str, arm: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut child = std::process::Command::new("/usr/bin/unshare")
        .args([
            "--user",
            "--map-root-user",
            "--pid",
            "--fork",
            "--mount-proc",
            "--",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(NAMESPACE_ARM_ENV, arm)
        .env(FORK_VFORK_EXEC_GUEST_ENV, fork_vfork_exec_guest())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn /usr/bin/unshare");
    // Drain both pipes while the child runs. Once the user's pipe pages pass
    // fs.pipe-user-pages-soft, a new pipe holds only 8 KiB, and a child
    // blocked writing a longer record would never exit.
    fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).expect("read the arm's output");
            bytes
        })
    }
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{arm} arm in a fresh PID namespace exceeded 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        status.success(),
        "{arm} arm in a fresh PID namespace failed ({status}):\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&stderr)
    );
    let record = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(NAMESPACE_MARK))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!record.is_empty(), "{arm} arm printed no record:\n{stdout}");
    record
}

/// Two plain ptrace runs of the fork/vfork/exec guest, each in its own fresh
/// PID namespace, are exactly equal: exit status, root, every Tool event and
/// return value (PIDs included, compared exactly), every stop, and the stop
/// counts. Each run also passes the baseline checks with its exact
/// namespace PIDs.
#[test]
fn plain_ptrace_repeats_pid_for_pid_in_a_fresh_pid_namespace() {
    if let Ok(arm) = std::env::var(NAMESPACE_ARM_ENV) {
        assert_eq!(arm, "ptrace", "unknown arm {arm}");
        let observed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_fork_vfork_exec_guest(true, Identity::Exact));
        // PIDs in a fresh namespace are small and increase without wrapping,
        // so the fork child precedes the vfork child.
        let mut children = observed
            .stops
            .keys()
            .filter(|task| **task != observed.root)
            .cloned()
            .collect::<Vec<_>>();
        children.sort_by_key(|task| task["pid ".len()..].parse::<i32>().unwrap());
        let [fork, vfork] = children.as_slice() else {
            panic!("expected exactly two children: {children:?}");
        };
        assert_baseline_is_not_vacuous(&observed, true, &observed.root, fork, vfork);
        // libtest has already printed "test <name> ... " without a newline.
        println!("\n{NAMESPACE_MARK}{observed:?}");
        return;
    }
    let module = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap();
    let test_name = format!("{module}::plain_ptrace_repeats_pid_for_pid_in_a_fresh_pid_namespace");
    let ptrace = run_arm_in_fresh_pid_namespace(&test_name, "ptrace");
    let ptrace_again = run_arm_in_fresh_pid_namespace(&test_name, "ptrace");
    assert!(
        ptrace.contains("root: \"pid ") && ptrace.contains("\"syscall clone = pid "),
        "record lacks PID-bearing values: {ptrace}"
    );
    assert_eq!(
        ptrace_again, ptrace,
        "plain ptrace is not repeatable in a fresh namespace"
    );
}

const TP_MAGIC: u64 = 0x7e57_0000_0000_0000;
const TP_MAGIC_MASK: u64 = 0xffff_0000_0000_0000;
const SHAPE_INJECT: u64 = 0;
const SHAPE_TAIL: u64 = 1;
/// Bits 32-36: a signal the parent sends when the Tool, at the call's seccomp
/// stop, notifies it; the Tool waits until the signal has arrived.
const TOOL_PARK_SHIFT: u32 = 32;
/// Bits 40-44: the same as the Tool's park signal, from the pre-syscall hook
/// (immediately before the call runs), at its `Early` point unless
/// `HOOK_LATE`.
const HOOK_PARK_SHIFT: u32 = 40;
/// Bit 45: the hook parks at its `Late` point.
const HOOK_LATE: u64 = 0x20 << HOOK_PARK_SHIFT;

/// The regression Tool's configuration.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct GuestConfig {
    /// Resumes untagged syscalls through `tail_inject`; otherwise runs them
    /// through `inject` and records each return value.
    tail: bool,
}

#[derive(Default)]
struct GuestLog(Mutex<Vec<(Pid, String)>>);

#[reverie::global_tool]
impl GlobalTool for GuestLog {
    type Config = GuestConfig;
    type Request = String;
    type Response = ();

    async fn receive_rpc(&self, from: Pid, event: String) {
        self.0.lock().unwrap().push((from, event));
    }
}

/// The last tagged call (sequence number) each tid acted on, so that a call
/// the kernel restarts does not park again.
static ACTED: Mutex<BTreeMap<i32, u64>> = Mutex::new(BTreeMap::new());

/// Names an address: the fixture is linked -no-pie, so its own text is at
/// the same address in every run, as is the private page. Library and stack
/// addresses are not, and are only reported as `other`.
fn code(address: u64) -> String {
    let page = crate::cp::PRIVATE_PAGE_OFFSET as u64;
    if (0x40_0000..0x100_0000).contains(&address) {
        format!("{address:#x}")
    } else if (page..page + 0x1000).contains(&address) {
        format!("page+{:#x}", address - page)
    } else {
        "other".to_owned()
    }
}

/// Renders a register value that may hold a return value.
fn value(raw: u64, pid: Pid) -> String {
    let signed = raw as i64;
    if signed == i64::from(pid.as_raw()) {
        "<pid>".to_owned()
    } else if signed.unsigned_abs() >= 1 << 32 {
        "pointer".to_owned()
    } else {
        signed.to_string()
    }
}

fn tgkill(pid: Pid, tid: Pid, signal: i32) {
    let result = unsafe { libc::syscall(libc::SYS_tgkill, pid.as_raw(), tid.as_raw(), signal) };
    assert_eq!(result, 0, "tgkill {signal}");
}

/// A field of `/proc/<tid>/status`, if the thread still exists.
fn proc_status_field(tid: Pid, field: &str) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{tid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(|value| value.trim().to_owned())
}

/// Notifies the (traced) parent of process `pid` with SIGUSR2.
fn notify_parent(pid: Pid) {
    let ppid: i32 = proc_status_field(pid, "PPid:")
        .expect("the parked guest exists")
        .parse()
        .expect("a PPid");
    let ppid = Pid::from_raw(ppid);
    tgkill(ppid, ppid, libc::SIGUSR2);
}

/// The private and shared pending signals of thread `tid`, from one read.
fn pending_masks(tid: Pid) -> (u64, u64) {
    let status = std::fs::read_to_string(format!("/proc/{tid}/status"))
        .unwrap_or_else(|error| panic!("read the status of parked thread {tid}: {error}"));
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            .unwrap_or_else(|| panic!("no {name} for {tid}"))
    };
    (field("SigPnd:"), field("ShdPnd:"))
}

/// Waits, without blocking the tracer's other tasks, until `arrived`.
async fn wait_until(what: &str, mut arrived: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !arrived() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Notifies the parent of the parked `tid` (of process `pid`), then waits,
/// without blocking the tracer's other tasks, until the signal the parent
/// sends in response is newly pending in either queue. The parked thread
/// dequeues nothing meanwhile, so a bit that becomes set is the parent's
/// signal; a test must not send one into a queue that already holds it (it
/// would coalesce, and never be seen to arrive).
async fn park_for(pid: Pid, tid: Pid, signal: i32) {
    let what = format!("signal {signal} did not arrive at parked thread {tid}");
    let bit = 1u64 << (signal - 1);
    let (private, shared) = pending_masks(tid);
    notify_parent(pid);
    wait_until(&what, || {
        let (now_private, now_shared) = pending_masks(tid);
        ((now_private & !private) | (now_shared & !shared)) & bit != 0
    })
    .await;
}

/// The pre-syscall hook: parks once on each call tagged with a hook park
/// signal, at the point the tag names (see `HOOK_PARK_SHIFT`).
fn park_hook() -> crate::task::PreSyscallForTest {
    type Fired = std::collections::BTreeSet<(i32, u64, crate::task::PreSyscallPoint)>;
    let fired = std::sync::Arc::new(Mutex::new(Fired::new()));
    std::sync::Arc::new(
        move |tid: Pid, regs: &libc::user_regs_struct, point: crate::task::PreSyscallPoint| {
            use crate::task::PreSyscallPoint::Early;
            use crate::task::PreSyscallPoint::Late;
            let signal = ((regs.r9 >> HOOK_PARK_SHIFT) & 0x1f) as i32;
            let park_point = if regs.r9 & HOOK_LATE != 0 {
                Late
            } else {
                Early
            };
            let sequence = (regs.r9 & !TP_MAGIC_MASK) >> 16;
            // The fixture parks only single-threaded children: pid == tid.
            let park = regs.r9 & TP_MAGIC_MASK == TP_MAGIC
                && signal != 0
                && point == park_point
                && fired
                    .lock()
                    .unwrap()
                    .insert((tid.as_raw(), sequence, point));
            Box::pin(async move {
                if park {
                    park_for(tid, tid, signal).await;
                }
            })
        },
    )
}

fn errno_value(result: &Result<i64, Errno>) -> i64 {
    match result {
        Ok(value) => *value,
        Err(errno) => -i64::from(errno.into_raw()),
    }
}

/// Records every Tool-visible event with the registers the Tool sees, and
/// handles each tagged call the way its tag asks.
#[derive(Default)]
struct GuestTool;

#[reverie::tool]
impl Tool for GuestTool {
    type GlobalState = GuestLog;
    type ThreadState = ();

    fn subscriptions(_config: &GuestConfig) -> Subscription {
        Subscription::all()
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        let regs = guest.regs().await;
        guest
            .send_rpc(format!(
                "thread-start rip={} rax={} rcx={}",
                code(regs.rip),
                value(regs.rax, guest.pid()),
                code(regs.rcx),
            ))
            .await;
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
        let tid = guest.tid();
        let pid = guest.pid();
        let regs = guest.regs().await;
        let name = call.number();
        guest
            .send_rpc(format!(
                "entry {name} rcx=rip:{} r11=rflags:{} rip={} orig_rax={}",
                regs.rcx == regs.rip,
                regs.r11 == regs.eflags,
                code(regs.rip),
                regs.orig_rax as i64,
            ))
            .await;
        let tagged = regs.r9 & TP_MAGIC_MASK == TP_MAGIC;
        let no_return = matches!(
            call,
            Syscall::Exit(_) | Syscall::ExitGroup(_) | Syscall::Execve(_) | Syscall::Execveat(_)
        );
        let shape = if no_return {
            SHAPE_TAIL
        } else if tagged {
            regs.r9 & 0xff
        } else if guest.config().tail {
            SHAPE_TAIL
        } else {
            SHAPE_INJECT
        };
        if tagged {
            let sequence = (regs.r9 & !TP_MAGIC_MASK) >> 16;
            guest
                .send_rpc(format!(
                    "tagged {name} rip={} orig_rax={} action={:#x}",
                    code(regs.rip),
                    regs.orig_rax as i64,
                    regs.r9 & 0xffff
                ))
                .await;
            let first = ACTED.lock().unwrap().insert(tid.as_raw(), sequence) != Some(sequence);
            let park = ((regs.r9 >> TOOL_PARK_SHIFT) & 0x1f) as i32;
            if first && park != 0 {
                park_for(pid, tid, park).await;
            }
        }
        match shape {
            SHAPE_TAIL => guest.tail_inject(call).await,
            _ => {
                assert_eq!(shape, SHAPE_INJECT, "unknown shape");
                let result = guest.inject(call).await;
                let rendered = match result {
                    // The guest's parent is not traced, and differs per run.
                    Ok(value) if name == reverie::syscalls::Sysno::getppid && value > 0 => {
                        "<ppid>".to_owned()
                    }
                    _ => errno_value(&result).to_string(),
                };
                guest.send_rpc(format!("syscall {name} = {rendered}")).await;
                let after = guest.regs().await;
                guest
                    .send_rpc(format!(
                        "exit {name} rip={} rax={} rcx={} r11=rflags:{}",
                        code(after.rip),
                        value(after.rax, pid),
                        code(after.rcx),
                        after.r11 == after.eflags,
                    ))
                    .await;
                Ok(result?)
            }
        }
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: reverie::Signal,
    ) -> Result<Option<reverie::Signal>, Errno> {
        let regs = guest.regs().await;
        guest
            .send_rpc(format!(
                "signal {signal:?} rip={} rax={} orig_rax={} rcx={}",
                code(regs.rip),
                value(regs.rax, guest.pid()),
                regs.orig_rax as i64,
                code(regs.rcx),
            ))
            .await;
        Ok(Some(signal))
    }
}

fn regression_guest() -> &'static std::path::Path {
    static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
        let source = crate::tracer::tests::fixture("plain_guest_regression.c");
        let directory = std::env::current_exe()
            .expect("locate the test binary")
            .parent()
            .expect("the test binary has a directory")
            .to_path_buf();
        // Every test binary in this directory shares it, and another checkout
        // or commit may build into the same target: key the published name
        // by the source (FNV-1a, fixed width, so argv length never changes).
        let text = std::fs::read(&source).expect("read the regression fixture source");
        let hash = text.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        let output = directory.join(format!("reverie-plain-guest-regression-{hash:016x}"));
        let staging = directory.join(format!(
            "reverie-plain-guest-regression-{hash:016x}.{}.tmp",
            std::process::id()
        ));
        // -no-pie: the fixture's text addresses are the same in every run.
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g", "-Wall", "-no-pie"])
            .arg(&source)
            .arg("-o")
            .arg(&staging)
            .status()
            .expect("invoke cc for the regression fixture");
        assert!(status.success(), "compile {}", source.display());
        std::fs::rename(&staging, &output).expect("publish the regression fixture");
        output
    });
    GUEST.as_path()
}

#[derive(Debug)]
struct GuestRun {
    status: ExitStatus,
    stops: BTreeMap<String, Vec<String>>,
    tool_events: BTreeMap<String, Vec<String>>,
    counts: PtraceBackendStatsSnapshot,
    /// The registers at every final resume of a Tool-visible syscall stop:
    /// rip, rax (the result, unless the Tool tail-injected) and orig_rax.
    resumes: BTreeMap<String, Vec<String>>,
    report: String,
    site: u64,
    /// Every signal-delivery stop the run loop handled: the signal, its
    /// siginfo and where it took effect.
    signals: BTreeMap<String, Vec<String>>,
}

impl GuestRun {
    fn all_events(&self) -> Vec<&String> {
        self.tool_events.values().flatten().collect()
    }
}

/// Renames traced PIDs in a Tool event with the task names of the run: any
/// token that is, or ends in `=` followed by, a traced PID.
fn rename_event(names: &TaskNames, event: &str) -> String {
    let event = names.event(event);
    event
        .split(' ')
        .map(|token| match token.rsplit_once('=') {
            Some((key, number)) if !key.is_empty() => format!("{key}={}", names.value(number)),
            _ => names.value(token),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn run_guest(mode: &str, tail: bool) -> GuestRun {
    run_guest_with(mode, tail)
        .await
        .unwrap_or_else(|error| panic!("regression fixture run {mode} tail={tail} failed: {error}"))
}

async fn run_guest_with(mode: &str, tail: bool) -> Result<GuestRun, Error> {
    let report_path = tempfile_path(&format!("plain-guest-{mode}"));
    let mut command = Command::new(regression_guest());
    command.arg(mode).arg(&report_path);
    let resumes = std::sync::Arc::new(Mutex::new(Vec::new()));
    let tracer = TracerBuilder::<GuestTool>::new(command)
        .config(GuestConfig { tail })
        .backend_stats(BackendStatsRequest::ENABLED)
        .final_resume_signal_for_test(resume_recorder(resumes.clone()))
        .pre_syscall_for_test(park_hook())
        .spawn()
        .await?;
    let stats = tracer.backend_stats().expect("stats were requested");
    let result = tokio::time::timeout(Duration::from_secs(60), finish_run(tracer, &stats))
        .await
        .unwrap_or_else(|_| panic!("regression fixture mode {mode} timed out"));
    let report = std::fs::read_to_string(&report_path).unwrap_or_default();
    let _ = std::fs::remove_file(&report_path);
    let (status, log) = result?;
    let site = report
        .lines()
        .next()
        .and_then(|line| line.split_once(" site=0x"))
        .and_then(|(_, hex)| u64::from_str_radix(hex, 16).ok())
        .unwrap_or_else(|| panic!("no site line in the report: {report}"));
    let stop_trace = stats.stop_trace();
    let names = TaskNames::new(Identity::Renamed, &stop_trace);
    let mut tool_events = BTreeMap::<String, Vec<String>>::new();
    for (pid, event) in std::mem::take(&mut *log.0.lock().unwrap()) {
        tool_events
            .entry(names.name(pid))
            .or_default()
            .push(rename_event(&names, &event));
    }
    let mut resume_events = BTreeMap::<String, Vec<String>>::new();
    for (pid, event) in std::mem::take(&mut *resumes.lock().unwrap()) {
        resume_events
            .entry(names.name(pid))
            .or_default()
            .push(rename_event(&names, &event));
    }
    let mut signals = BTreeMap::<String, Vec<String>>::new();
    for (pid, event) in stats.signal_trace() {
        // The tracer's own traps: a kernel SIGSEGV (si_code SI_KERNEL) is its
        // rdtsc or cpuid trap, whose rax holds whatever the guest computed
        // last, and SIGSTKFLT is its timer's perf overflow, which lands
        // wherever the counter's skid left it. Their registers are not
        // recorded.
        let tracer_trap =
            event.starts_with("SIGSEGV signo=11 code=128 ") || event.starts_with("SIGSTKFLT ");
        let event = event
            .split(' ')
            .filter(|token| {
                !(tracer_trap && (token.starts_with("rax=") || token.starts_with("rip=")))
            })
            .map(|token| match token.strip_prefix("rip=0x") {
                Some(hex) => format!(
                    "rip={}",
                    code(u64::from_str_radix(hex, 16).expect("a hex rip"))
                ),
                // A signal the tracer (this process) sent.
                None if token == format!("pid={}", std::process::id()) => "pid=tracer".to_owned(),
                None => token.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        signals
            .entry(names.name(pid))
            .or_default()
            .push(rename_event(&names, &event));
    }
    Ok(GuestRun {
        status,
        stops: names.per_task(stop_trace),
        tool_events,
        counts: reverie::BackendStatsSource::backend_stats(&stats),
        resumes: resume_events,
        report,
        site,
        signals,
    })
}

/// Waits for a run like `Tracer::wait`, except that a run whose cleanup is
/// left pending is terminated, its root is killed, and the same cleanup is
/// resumed once; if it is still pending, every tracee of the run that this
/// process still traces is killed (and named on stderr) and the owner is
/// dropped, rather than quarantined. `Tracer::wait` (and `quarantine`) take a
/// process-wide quarantine permit that is never released, so one failed run
/// would refuse every later spawn in this test binary and hide which test
/// failed. The run still fails, with its original cause.
async fn finish_run(
    tracer: Tracer<GuestLog>,
    stats: &crate::PtraceBackendStatsSource,
) -> Result<(ExitStatus, GuestLog), Error> {
    let Some(termination) = tracer.termination_handle() else {
        return tracer.wait().await;
    };
    // SAFETY: pidfd_open takes no pointers; a failure leaves -1.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, tracer.guest_pid().as_raw(), 0) };
    // SAFETY: a non-negative result is a new descriptor owned here.
    let root = (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw as RawFd) });
    match tracer.wait_completion().await {
        ToolRunOutcome::Complete(completion) => completion
            .result
            .map(|status| (status, completion.global_state))
            .map_err(crate::PtraceRunFailure::into_legacy_error),
        ToolRunOutcome::CleanupPending(pending) => {
            let cause = pending.failure().to_string();
            termination.terminate(Error::Tool(anyhow::anyhow!(
                "regression rescue after: {cause}"
            )));
            if let Some(root) = &root {
                // SAFETY: a valid pidfd, and a null siginfo.
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        root.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                };
            }
            match tokio::time::timeout(Duration::from_secs(5), pending.resume_cleanup()).await {
                Ok(ToolRunOutcome::Complete(_)) => Err(Error::Tool(anyhow::anyhow!(
                    "{cause} (cleanup completed after the rescue)"
                ))),
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    kill_remaining_tracees(stats);
                    drop(pending);
                    Err(Error::Tool(anyhow::anyhow!(
                        "{cause} (cleanup still pending after the rescue)"
                    )))
                }
                Ok(ToolRunOutcome::UnsupportedBackend(_)) => {
                    unreachable!("a pending ordinary cleanup resumes on the same backend")
                }
                Err(_) => Err(Error::Tool(anyhow::anyhow!(
                    "{cause} (cleanup rescue timed out)"
                ))),
            }
        }
        ToolRunOutcome::UnsupportedBackend(tracer) => tracer.wait().await,
    }
}

/// Kills, with SIGKILL, each thread group of a tracee in `stats`' stop trace
/// that a thread of this process still traces, and names each on stderr, so
/// that a run whose cleanup stayed pending leaves no tracee behind.
/// `TracerPid` is checked first, so a reused PID of an unrelated process is
/// not signalled.
fn kill_remaining_tracees(stats: &crate::PtraceBackendStatsSource) {
    let tids: std::collections::BTreeSet<i32> = stats
        .stop_trace()
        .into_iter()
        .map(|(pid, _)| pid.as_raw())
        .collect();
    for tid in tids {
        let Ok(status) = std::fs::read_to_string(format!("/proc/{tid}/status")) else {
            continue;
        };
        // The tracer is a thread (TracerPid is its TID) of this process.
        let tracer = status.lines().find_map(|line| {
            line.strip_prefix("TracerPid:")
                .and_then(|value| value.trim().parse::<i32>().ok())
        });
        let traced_here = tracer.is_some_and(|tracer| {
            tracer != 0 && std::path::Path::new(&format!("/proc/self/task/{tracer}")).exists()
        });
        if traced_here {
            // SAFETY: kill takes no pointers.
            let rc = unsafe { libc::kill(tid, libc::SIGKILL) };
            eprintln!("regression rescue: killed remaining tracee {tid} (rc={rc})");
        }
    }
}

/// Records the registers at every final resume of a Tool-visible syscall
/// stop in `log`. It leaves no signal pending.
fn resume_recorder(
    log: std::sync::Arc<Mutex<Vec<(Pid, String)>>>,
) -> crate::task::FinalResumeSignalForTest {
    std::sync::Arc::new(move |tid: Pid, regs: &libc::user_regs_struct| {
        // The guest's pid is the thread-group id, which the hook does not
        // see; a result equal to the tid is the common case (getpid in the
        // root thread) and is rendered as `<pid>` too.
        log.lock().unwrap().push((
            tid,
            format!(
                "resume rip={} rax={} orig_rax={} rcx={} r11=rflags:{}",
                code(regs.rip),
                value(regs.rax, tid),
                regs.orig_rax as i64,
                code(regs.rcx),
                // The raw flags depend on guest arithmetic on its own pid,
                // which differs per run.
                regs.r11 == regs.eflags
            ),
        ));
        None
    })
}

/// Runs `mode` in both Tool configurations, and requires each run to exit 0
/// with a complete report. Returns the two runs as (inject, tail).
async fn run_both_configurations(mode: &str) -> [GuestRun; 2] {
    let inject = run_guest(mode, false).await;
    let tail = run_guest(mode, true).await;
    for run in [&inject, &tail] {
        assert_eq!(run.status, ExitStatus::Exited(0), "{}", run.report);
        assert!(run.report.ends_with("done\n"), "{}", run.report);
    }
    [inject, tail]
}

fn assert_report_has(run: &GuestRun, lines: &[&str]) {
    for line in lines {
        assert!(
            run.report
                .lines()
                .any(|candidate| candidate.starts_with(line)),
            "report lacks a line starting {line:?}:\n{}",
            run.report
        );
    }
}

/// The signal-delivery stop of `signal` sent by `sender` with `code`, where
/// the call at the shared site (orig_rax `nr`) returned `rax`.
fn delivery_after_the_call(
    run: &GuestRun,
    signal: &str,
    code: i32,
    sender: &str,
    rax: &str,
    nr: i64,
) -> String {
    let number = match signal {
        "SIGCONT" => libc::SIGCONT,
        "SIGTSTP" => libc::SIGTSTP,
        "SIGTTIN" => libc::SIGTTIN,
        "SIGTTOU" => libc::SIGTTOU,
        _ => unreachable!("{signal}"),
    };
    format!(
        "{signal} signo={number} code={code} errno=0 pid={sender} uid={} rip={:#x} rax={rax} orig_rax={nr}",
        // SAFETY: getuid cannot fail.
        unsafe { libc::getuid() },
        run.site + 2
    )
}

/// Syscall user dispatch with the allowed region excluding the shared site:
/// the dispatched SIGSYS reports the x86_64 `getpid` at the site's end, the
/// handler's result (1234) is the call's, and after dispatch is turned off
/// the parent and a forked child run getpid normally.
#[tokio::test(flavor = "current_thread")]
async fn syscall_user_dispatch_sees_the_original_site() {
    let [inject, _] = run_both_configurations("sud").await;
    assert_report_has(
        &inject,
        &[
            "sud on ret=0",
            "after sud site bytes 0f 05",
            "dispatched getpid ret=1234 count=1 syscall=39 arch=0xc000003e call=tp_site_end",
            "sud off ret=0",
            "getpid 2 pid=1",
            "child getpid 2 pid=1",
            "child exited=1 code=0",
            "sud handled=1",
        ],
    );
}

/// A CLONE_UNTRACED thread gets no new-child stop: it executes the original
/// `syscall` at the shared site, whose getpid the inherited filter refuses
/// with ENOSYS (no tracer), and whose rcx is the site's end. Cloned from
/// libc and through the shared site itself.
#[tokio::test(flavor = "current_thread")]
async fn untraced_thread_runs_the_original_syscall() {
    for mode in ["untraced_thread", "untraced_thread_site"] {
        let [inject, _] = run_both_configurations(mode).await;
        assert_report_has(
            &inject,
            &[
                "untraced clone ok=1",
                "untraced thread getpid ret=-38 rcx=tp_site_end",
                "after untraced site bytes 0f 05",
            ],
        );
    }
}

/// A fork-like CLONE_UNTRACED child (without CLONE_VM), cloned through the
/// shared site, gets no new-child stop and its own copy of the text: it
/// starts after the guest's own `syscall` (rcx at the site's end, r11 0x246),
/// executes the original instruction there (getpid refused with ENOSYS by the
/// inherited filter), and is killed by its parent.
#[tokio::test(flavor = "current_thread")]
async fn untraced_fork_runs_the_original_syscall() {
    let [inject, tail] = run_both_configurations("untraced_fork").await;
    assert_report_has(
        &inject,
        &[
            "untraced fork clone ok=1",
            "untraced fork child done=1 clone rcx=tp_site_end r11=0x246 \
             getpid ret=-38 rcx=tp_site_end bytes 0f 05",
            // CLD_KILLED by the parent's SIGKILL.
            "untraced fork child code=2 status=9",
            "after untraced fork site bytes 0f 05",
        ],
    );
    // The child is untraced: no new-child stop.
    assert_eq!(tail.counts.fork_stops(), 0);
    assert_eq!(tail.counts.clone_stops(), 0);
}

/// The guest's own seccomp filter returns `SECCOMP_RET_TRACE` for number 500,
/// which the syscall table does not know (the tracer's filter does not trace
/// it). The seccomp stop's number cannot be decoded: `get_syscall` used to
/// panic in `Sysno::from` here, and now ends the run with an ENOSYS error
/// whose context names the stop. Returning -ENOSYS to the guest instead is a
/// tracked follow-up.
#[tokio::test(flavor = "current_thread")]
async fn guest_trace_of_an_unknown_number_ends_the_run() {
    let error = run_guest_with("guest_trace_unknown", false)
        .await
        .expect_err("an undecodable seccomp stop must end the run");
    let text = format!("{error:#} {error:?}");
    eprintln!("{text}");
    assert!(text.contains("read registers at seccomp stop"), "{text}");
    assert!(text.contains("ENOSYS"), "{text}");
}

/// A parent stops its child (SIGSTOP) while the child's write at the shared
/// site is parked at its seccomp stop, then continues it (SIGCONT)
/// immediately before the write runs. The SIGCONT discards the pending
/// SIGSTOP: the write returns 4, the guest's SIGCONT handler runs once at the
/// site's end, the run loop sees exactly one signal-delivery stop (the
/// SIGCONT, after the write) and the Tool one SIGCONT event, and the parent
/// sees no stop.
#[tokio::test(flavor = "current_thread")]
async fn sigcont_before_the_call_discards_the_pending_sigstop() {
    for tail in [false, true] {
        let ptrace = run_guest("sigstop_cont_late", tail).await;
        assert_eq!(ptrace.status, ExitStatus::Exited(0), "{}", ptrace.report);
        let cont = delivery_after_the_call(&ptrace, "SIGCONT", libc::SI_USER, "task#0", "4", 1);
        for tag in ["tail", "inject"] {
            let common = [
                format!("{tag} child call ret=4 byte=0"),
                format!("{tag} parent notified=2 data=4 after=5 restarted=-1 waitpid exited:7"),
                format!("{tag} parent SIGCHLD code=1 status=7 from-child=1"),
            ];
            let common: Vec<&str> = common.iter().map(String::as_str).collect();
            assert_report_has(&ptrace, &common);
            let handled =
                format!("{tag} child signal 0: sig=18 code=0 from-parent=1 rip=tp_site_end rax=4");
            assert_report_has(&ptrace, &[&handled]);
        }
        for child in ["task#1", "task#2"] {
            assert_eq!(ptrace.signals[child], std::slice::from_ref(&cont));
            let sigcont_events = ptrace.tool_events[child]
                .iter()
                .filter(|event| event.starts_with("signal SIGCONT "))
                .count();
            assert_eq!(sigcont_events, 1, "{:#?}", ptrace.tool_events);
        }
    }
}

/// A SIGSTOP sent while the child's write is parked at its seccomp stop, then
/// a SIGCONT and a SIGTSTP, SIGTTIN or SIGTTOU (handled) immediately before
/// the write runs. The stop signal discards the SIGCONT, which had discarded
/// the SIGSTOP: only the stop signal is delivered, after the write, its
/// handler runs once at the site's end, and there is no SIGSTOP delivery
/// stop and no SIGCONT.
#[tokio::test(flavor = "current_thread")]
async fn sigcont_then_a_stop_signal_delivers_only_the_stop_signal() {
    for (mode, tag, name, signal) in [
        ("sigstop_cont_tstp", "tstp", "SIGTSTP", libc::SIGTSTP),
        ("sigstop_cont_ttin", "ttin", "SIGTTIN", libc::SIGTTIN),
        ("sigstop_cont_ttou", "ttou", "SIGTTOU", libc::SIGTTOU),
    ] {
        let ptrace = run_guest(mode, false).await;
        assert_eq!(ptrace.status, ExitStatus::Exited(0), "{}", ptrace.report);
        assert_report_has(
            &ptrace,
            &[
                &format!("{tag} child call ret=4 byte=0"),
                &format!(
                    "{tag} child signal 0: sig={signal} code=0 from-parent=1 rip=tp_site_end rax=4"
                ),
                &format!("{tag} parent notified=2 data=4 after=5 restarted=-1 waitpid exited:7"),
                &format!("{tag} parent SIGCHLD code=1 status=7 from-child=1"),
            ],
        );
        assert!(
            !ptrace.report.contains(&format!("{tag} child signal 1:")),
            "{}",
            ptrace.report
        );
        let only = delivery_after_the_call(&ptrace, name, libc::SI_USER, "task#0", "4", 1);
        assert_eq!(
            ptrace.signals["task#1"],
            std::slice::from_ref(&only),
            "{mode}"
        );
    }
}

/// Unknown and out-of-range syscall numbers through generic sites return
/// -ENOSYS (rcx the next instruction, r11 0x246) without any Tool event, and
/// a number with high bits set runs as its low 32 bits: high-getpid returns
/// the pid, and the Tool, the stop trace and the final resume each see its
/// full orig_rax exactly once.
#[tokio::test(flavor = "current_thread")]
async fn unknown_numbers_return_enosys_without_a_tool_event() {
    let high = 0x1_0000_0027u64;
    for tail in [false, true] {
        let ptrace = run_guest("unknown", tail).await;
        eprintln!("unknown report:\n{}", ptrace.report);
        let count = |events: &BTreeMap<String, Vec<String>>, needle: &str| {
            events
                .values()
                .flatten()
                .filter(|event| event.contains(needle))
                .count()
        };
        assert_eq!(
            count(&ptrace.tool_events, &format!("orig_rax={high}")),
            1,
            "one Tool entry shows the full number"
        );
        assert_eq!(
            count(&ptrace.stops, &format!("seccomp {high}")),
            1,
            "one stop shows the full number"
        );
        assert_eq!(
            count(&ptrace.resumes, &format!("orig_rax={high}")),
            1,
            "one resume shows the full number"
        );
        assert_eq!(ptrace.status, ExitStatus::Exited(0), "{}", ptrace.report);
        assert_report_has(
            &ptrace,
            &[
                "nr500 ret=-38 rcx-next=1 r11=0x246",
                "nr500 getpid after=1",
                "nr500 bytes after 0f 05",
                "nr-1 ret=-38 rcx-next=1 r11=0x246",
                "nr-1 bytes after 0f 05",
                "gap337 ret=-38 rcx-next=1 r11=0x246",
                "gap337 bytes after 0f 05",
                "high-getpid ret=<pid> rcx-next=1 r11=0x246",
                "high-getpid getpid after=1",
                "high500 ret=-38 rcx-next=1 r11=0x246",
                "high500 bytes after 0f 05",
            ],
        );
        assert!(
            !ptrace
                .all_events()
                .iter()
                .any(|event| event.contains("orig_rax=500") || event.contains("orig_rax=-1")),
            "an unknown number reached the Tool: {:#?}",
            ptrace.all_events()
        );
    }
}
