/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Trap-only LiteInst with site patching on (P2b): every test runs one mode
//! of `tests/fixtures/trap_only_p2.c` under plain ptrace and under trap-only
//! with patching on, and requires the two runs to be equal: exit status, the
//! run-loop stop sequence, every Tool-visible event (with the registers the
//! Tool sees), the stop counts, and the guest's own report of its results,
//! registers and signal frames. Each test also checks that the trap-only run
//! really patched and hopped, so that equality is not vacuous.
//!
//! The fixture tags the calls it makes through its shared site with a magic
//! value in r9, which also tells the Tool how to handle the call and which
//! signals to send while the call is parked at its seccomp stop.

use super::*;
use crate::liteinst_trap_only::DisabledReason;
use crate::liteinst_trap_only::RetiredReason;
use crate::liteinst_trap_only::SiteState;
use crate::liteinst_trap_only::TableState;
use crate::task::step_count_for_test;

const TP_MAGIC: u64 = 0x7e57_0000_0000_0000;
const TP_MAGIC_MASK: u64 = 0xffff_0000_0000_0000;
const SHAPE_INJECT: u64 = 0;
const SHAPE_TAIL: u64 = 1;
const SHAPE_EMULATE: u64 = 2;
const SHAPE_PRIVATE: u64 = 3;
const SHAPE_TWO_INJECTS: u64 = 4;
const SHAPE_TWO_PRIVATE: u64 = 5;
const SEND_SIGUSR1: u64 = 0x100;
const SEND_SIGWINCH: u64 = 0x200;
const SEND_QUEUE: u64 = 0x400;
/// The tracer leaves SIGUSR1 pending for its final resume of the stop.
const SEND_RESUME: u64 = 0x800;

#[derive(Default)]
struct P2Log(Mutex<Vec<(Pid, String)>>);

#[reverie::global_tool]
impl GlobalTool for P2Log {
    /// `true` resumes untagged syscalls through `tail_inject`; `false` runs
    /// them through `inject` and records each return value.
    type Config = bool;
    type Request = String;
    type Response = ();

    async fn receive_rpc(&self, from: Pid, event: String) {
        self.0.lock().unwrap().push((from, event));
    }
}

/// Single-step count of each tid when its thread started.
static STEP_BASE: Mutex<BTreeMap<i32, u64>> = Mutex::new(BTreeMap::new());
/// The last tagged call (sequence number) each tid acted on, so that a call
/// the kernel restarts does not send its signals again.
static ACTED: Mutex<BTreeMap<i32, u64>> = Mutex::new(BTreeMap::new());

fn steps(tid: Pid) -> u64 {
    let base = STEP_BASE
        .lock()
        .unwrap()
        .get(&tid.as_raw())
        .copied()
        .unwrap_or(0);
    step_count_for_test(tid) - base
}

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

fn send_queued(pid: Pid, tid: Pid, signal: i32, value: i32, thread: bool) {
    // A kernel siginfo: signo at 0, code at 8 (SI_QUEUE), pid at 16, uid at
    // 20 and the value at 24.
    let mut info = [0u8; 128];
    info[0..4].copy_from_slice(&signal.to_ne_bytes());
    info[8..12].copy_from_slice(&libc::SI_QUEUE.to_ne_bytes());
    info[16..20].copy_from_slice(&std::process::id().to_ne_bytes());
    info[20..24].copy_from_slice(&unsafe { libc::getuid() }.to_ne_bytes());
    info[24..28].copy_from_slice(&value.to_ne_bytes());
    let result = unsafe {
        if thread {
            libc::syscall(
                libc::SYS_rt_tgsigqueueinfo,
                pid.as_raw(),
                tid.as_raw(),
                signal,
                info.as_ptr(),
            )
        } else {
            libc::syscall(
                libc::SYS_rt_sigqueueinfo,
                pid.as_raw(),
                signal,
                info.as_ptr(),
            )
        }
    };
    assert_eq!(result, 0, "queue signal {signal}");
}

fn tgkill(pid: Pid, tid: Pid, signal: i32) {
    let result = unsafe { libc::syscall(libc::SYS_tgkill, pid.as_raw(), tid.as_raw(), signal) };
    assert_eq!(result, 0, "tgkill {signal}");
}

/// Sends the signals a tagged call asks for, while it is parked at its stop.
fn send_signals(pid: Pid, tid: Pid, action: u64) {
    if action & SEND_SIGUSR1 != 0 {
        tgkill(pid, tid, libc::SIGUSR1);
    }
    if action & SEND_SIGWINCH != 0 {
        tgkill(pid, tid, libc::SIGWINCH);
    }
    if action & SEND_QUEUE != 0 {
        // Standard signals only: the ptrace backend cannot handle a
        // real-time signal stop.
        send_queued(pid, tid, libc::SIGUSR1, 1, true);
        send_queued(pid, tid, libc::SIGUSR2, 2, false);
        send_queued(pid, tid, libc::SIGHUP, 3, false);
        tgkill(pid, tid, libc::SIGALRM);
        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGURG) }, 0);
        // Coalesces with the pending SIGUSR1.
        send_queued(pid, tid, libc::SIGUSR1, 5, true);
    }
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
struct P2Tool;

#[reverie::tool]
impl Tool for P2Tool {
    type GlobalState = P2Log;
    type ThreadState = ();

    fn subscriptions(_config: &bool) -> Subscription {
        Subscription::all()
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        let tid = guest.tid();
        STEP_BASE
            .lock()
            .unwrap()
            .insert(tid.as_raw(), step_count_for_test(tid));
        let regs = guest.regs().await;
        guest
            .send_rpc(format!(
                "thread-start rip={} rax={} rcx={} r11={:#x}",
                code(regs.rip),
                value(regs.rax, guest.pid()),
                code(regs.rcx),
                regs.r11
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
                "entry {name} steps={} rcx=rip:{} r11=rflags:{}",
                steps(tid),
                regs.rcx == regs.rip,
                regs.r11 == regs.eflags,
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
        } else if *guest.config() {
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
            if first {
                send_signals(pid, tid, regs.r9 & 0xff00);
            }
        }
        match shape {
            SHAPE_TAIL => guest.tail_inject(call).await,
            SHAPE_EMULATE => {
                guest.send_rpc(format!("syscall {name} = 4242")).await;
                Ok(4242)
            }
            SHAPE_PRIVATE => {
                let result = guest.inject(reverie::syscalls::Getpid::new()).await;
                guest
                    .send_rpc(format!("syscall {name} = {}", errno_value(&result)))
                    .await;
                Ok(result?)
            }
            SHAPE_TWO_INJECTS => {
                let first = guest.inject(call).await;
                let second = guest.inject(reverie::syscalls::Getpid::new()).await;
                guest
                    .send_rpc(format!(
                        "syscall {name} = {} then getpid {}",
                        errno_value(&first),
                        errno_value(&second)
                    ))
                    .await;
                Ok(first?)
            }
            SHAPE_TWO_PRIVATE => {
                let first = guest.inject(reverie::syscalls::Getpid::new()).await;
                let second = guest.inject(reverie::syscalls::Getpid::new()).await;
                guest
                    .send_rpc(format!(
                        "syscall {name} = {} then {}",
                        errno_value(&first),
                        errno_value(&second)
                    ))
                    .await;
                Ok(second?)
            }
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
                        "exit {name} rip={} rcx={} r11=rflags:{} steps={}",
                        code(after.rip),
                        code(after.rcx),
                        after.r11 == after.eflags,
                        steps(tid)
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
                "signal {signal:?} rip={} rax={} orig_rax={} rcx={} steps={}",
                code(regs.rip),
                value(regs.rax, guest.pid()),
                regs.orig_rax as i64,
                code(regs.rcx),
                steps(guest.tid()),
            ))
            .await;
        if signal == reverie::Signal::SIGWINCH {
            // Suppressed: the interrupted call restarts.
            Ok(None)
        } else {
            Ok(Some(signal))
        }
    }
}

fn p2_guest() -> &'static std::path::Path {
    static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
        let source = crate::tracer::tests::fixture("trap_only_p2.c");
        let directory = std::env::current_exe()
            .expect("locate the test binary")
            .parent()
            .expect("the test binary has a directory")
            .to_path_buf();
        let output = directory.join("reverie-trap-only-p2");
        let staging = directory.join(format!("reverie-trap-only-p2.{}.tmp", std::process::id()));
        // -no-pie: the fixture's text addresses are the same in every run, so
        // the two backends' reports and register observations compare.
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g", "-Wall", "-no-pie"])
            .arg(&source)
            .arg("-o")
            .arg(&staging)
            .status()
            .expect("invoke cc for the trap-only P2 fixture");
        assert!(status.success(), "compile {}", source.display());
        std::fs::rename(&staging, &output).expect("publish the trap-only P2 fixture");
        output
    });
    GUEST.as_path()
}

#[derive(Debug)]
struct P2Run {
    status: ExitStatus,
    stops: BTreeMap<String, Vec<String>>,
    tool_events: BTreeMap<String, Vec<String>>,
    counts: PtraceBackendStatsSnapshot,
    report: String,
    /// Trap-only only: the root address space's table at the end of the run.
    patched_sites: usize,
    table_state: Option<TableState>,
    site_state: Option<SiteState>,
    site: u64,
}

impl P2Run {
    fn all_events(&self) -> Vec<&String> {
        self.tool_events.values().flatten().collect()
    }

    fn count_events(&self, prefix: &str) -> usize {
        self.all_events()
            .iter()
            .filter(|event| event.starts_with(prefix))
            .count()
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

async fn run_p2(mode: &str, patching: Option<SitePatching>, tail: bool) -> P2Run {
    run_p2_with(mode, patching, tail, false)
        .await
        .expect("P2 fixture run failed")
}

async fn run_p2_with(
    mode: &str,
    patching: Option<SitePatching>,
    tail: bool,
    skip_patch_write: bool,
) -> Result<P2Run, Error> {
    let report_path = tempfile_path(&format!("trap-only-p2-{mode}"));
    let mut command = Command::new(p2_guest());
    command.arg(mode).arg(&report_path);
    let mut builder = TracerBuilder::<P2Tool>::new(command)
        .config(tail)
        .backend_stats(BackendStatsRequest::ENABLED)
        .final_resume_signal_for_test(resume_signal_hook());
    if let Some(patching) = patching {
        builder = builder.liteinst_trap_only(patching);
        if skip_patch_write {
            builder = builder.liteinst_trap_only_skip_patch_write_for_test();
        }
    }
    let tracer = builder.spawn().await?;
    let stats = tracer.backend_stats().expect("stats were requested");
    let handle = tracer.liteinst_trap_only();
    let result = tokio::time::timeout(Duration::from_secs(60), tracer.wait())
        .await
        .unwrap_or_else(|_| panic!("P2 fixture mode {mode} timed out"));
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
    Ok(P2Run {
        status,
        stops: names.per_task(stop_trace),
        tool_events,
        counts: reverie::BackendStatsSource::backend_stats(&stats),
        report,
        patched_sites: handle.as_ref().map_or(0, |handle| handle.patched_sites()),
        table_state: handle.as_ref().map(|handle| handle.table_state()),
        site_state: handle.as_ref().and_then(|handle| handle.site_state(site)),
        site,
    })
}

/// Leaves SIGUSR1 pending for the final resume of each `SEND_RESUME` call,
/// once per call.
fn resume_signal_hook() -> crate::task::FinalResumeSignalForTest {
    let fired = std::sync::Arc::new(Mutex::new(std::collections::BTreeSet::<(i32, u64)>::new()));
    std::sync::Arc::new(move |tid: Pid, regs: &libc::user_regs_struct| {
        if regs.r9 & TP_MAGIC_MASK != TP_MAGIC || regs.r9 & SEND_RESUME == 0 {
            return None;
        }
        let sequence = (regs.r9 & !TP_MAGIC_MASK) >> 16;
        fired
            .lock()
            .unwrap()
            .insert((tid.as_raw(), sequence))
            .then_some(nix::sys::signal::Signal::SIGUSR1)
    })
}

/// Describes where two per-task traces first differ.
fn first_divergence(
    trap_only: &BTreeMap<String, Vec<String>>,
    ptrace: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut out = String::new();
    for task in ptrace.keys().chain(trap_only.keys()) {
        let (left, right) = (
            trap_only.get(task).cloned().unwrap_or_default(),
            ptrace.get(task).cloned().unwrap_or_default(),
        );
        if left == right {
            continue;
        }
        let index = left
            .iter()
            .zip(&right)
            .position(|(l, r)| l != r)
            .unwrap_or(left.len().min(right.len()));
        let from = index.saturating_sub(3);
        out += &format!(
            "{task}: first difference at {index} (lengths {} vs {})\n  trap-only: {:#?}\n  ptrace: {:#?}\n",
            left.len(),
            right.len(),
            &left[from..(index + 4).min(left.len())],
            &right[from..(index + 4).min(right.len())],
        );
        break;
    }
    out
}

fn assert_equal_runs(trap_only: &P2Run, ptrace: &P2Run) {
    assert_eq!(trap_only.status, ptrace.status, "exit status diverged");
    assert_eq!(trap_only.report, ptrace.report, "guest reports diverged");
    assert!(
        trap_only.tool_events == ptrace.tool_events,
        "Tool-visible events diverged: {}",
        first_divergence(&trap_only.tool_events, &ptrace.tool_events)
    );
    assert!(
        trap_only.stops == ptrace.stops,
        "stop sequences diverged: {}",
        first_divergence(&trap_only.stops, &ptrace.stops)
    );
    assert_eq!(trap_only.counts, ptrace.counts, "stop counts diverged");
}

/// The trap-only run patched the shared site, and its tagged calls reached
/// the Tool from the patched site.
fn assert_patched(trap_only: &P2Run, expected_site: SiteState) {
    assert_eq!(
        trap_only.site_state,
        Some(expected_site),
        "shared site {:#x} was not patched as expected",
        trap_only.site
    );
    assert!(trap_only.patched_sites > 0 || expected_site != SiteState::Live);
}

/// Runs `mode` under ptrace and under trap-only with patching on, in both
/// Tool configurations, and requires equal runs. Returns the four runs as
/// (ptrace inject, trap-only inject, ptrace tail, trap-only tail).
async fn compare_mode(mode: &str) -> [P2Run; 4] {
    let ptrace_inject = run_p2(mode, None, false).await;
    let trap_only_inject = run_p2(mode, Some(SitePatching::On), false).await;
    assert_equal_runs(&trap_only_inject, &ptrace_inject);
    let ptrace_tail = run_p2(mode, None, true).await;
    let trap_only_tail = run_p2(mode, Some(SitePatching::On), true).await;
    assert_equal_runs(&trap_only_tail, &ptrace_tail);
    for run in [&ptrace_inject, &ptrace_tail] {
        assert_eq!(run.status, ExitStatus::Exited(0), "{}", run.report);
        assert!(run.report.ends_with("done\n"), "{}", run.report);
    }
    [ptrace_inject, trap_only_inject, ptrace_tail, trap_only_tail]
}

fn assert_report_has(run: &P2Run, lines: &[&str]) {
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

fn assert_events_have(run: &P2Run, events: &[&str]) {
    let all = run.all_events();
    for event in events {
        assert!(
            all.iter().any(|candidate| candidate.starts_with(event)),
            "no Tool event starting {event:?} in {all:#?}"
        );
    }
}

/// T1a: a signal sent while a patched getpid is parked is delivered after
/// getpid returned, at S+2, exactly as under ptrace.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t1a_signal_pending_at_a_patched_stop() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("sig_pending").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    let s2 = format!("{:#x}", ptrace.site + 2);
    assert_report_has(
        &ptrace,
        &[
            "getpid returned pid=1",
            "sig_pending 0: sig=10 code=-6 value=-1 rip=tp_site_end rax=<pid> rcx=tp_site_end r11=0x246 ",
            "sig_pending_tail 0: sig=10 code=-6 value=-1 rip=tp_site_end rax=<pid> rcx=tp_site_end r11=0x246 ",
        ],
    );
    assert_events_have(
        &ptrace,
        &[&format!(
            "signal SIGUSR1 rip={s2} rax=<pid> orig_rax=39 rcx={s2} steps="
        )],
    );
}

/// T1c: queued thread- and process-directed signals are delivered in the
/// same order, with the same values, as under ptrace.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t1c_queued_signals_keep_order_and_values() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("rt_queue").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    let queued = ptrace
        .report
        .lines()
        .filter(|line| line.starts_with("queue "))
        .count();
    assert_eq!(
        queued, 5,
        "five distinct signals delivered:\n{}",
        ptrace.report
    );
    assert!(
        ptrace.report.contains("sig=10 code=-1 value=1"),
        "{}",
        ptrace.report
    );
}

/// T1f: signals raised by the patched syscall itself are delivered at S+2
/// with the same registers as under ptrace.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t1f_self_raised_signals() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("self_raise").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    assert_report_has(
        &ptrace,
        &[
            "kill 0: sig=10 code=0 value=-1 rip=tp_site_end rax=0 rcx=tp_site_end r11=0x246 ",
            "sigpipe 0: sig=13 code=0 value=-1 rip=tp_site_end rax=-32 rcx=tp_site_end r11=0x246 ",
            "unblock 0: sig=12 code=-6 value=-1 rip=tp_site_end rax=0 rcx=tp_site_end r11=0x246 ",
        ],
    );
}

/// T2: each Tool handler shape at a patched site single-steps exactly as
/// often as under ptrace, so SIGTRAP disposition, mask and trapno are equal.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t2_forced_sigtrap_profile_per_shape() {
    let [ptrace, trap_only, _, _] = compare_mode("sigtrap_profile").await;
    assert_patched(&trap_only, SiteState::Live);
    // Order of the fixture's shapes: tail, exact inject, emulate, private
    // inject, two injects, two private injects.
    let expected = [0, 0, 1, 2, 1, 3];
    let ptrace_steps = steps_per_shape(&ptrace);
    eprintln!("ptrace steps per shape: {ptrace_steps:?}");
    assert_eq!(ptrace_steps, expected, "ptrace step profile");
    assert_eq!(
        steps_per_shape(&trap_only),
        expected,
        "trap-only step profile"
    );
}

/// Steps taken by each tagged call: the cumulative count at the next
/// syscall entry minus the count at the call's entry.
fn steps_per_shape(run: &P2Run) -> Vec<u64> {
    let root = &run.tool_events["task#0"];
    let entry_steps = |event: &String| -> u64 {
        event
            .split_once(" steps=")
            .and_then(|(_, rest)| rest.split(' ').next())
            .and_then(|value| value.parse().ok())
            .unwrap()
    };
    let mut per_shape = Vec::new();
    for (index, event) in root.iter().enumerate() {
        if !event.starts_with("tagged getppid") {
            continue;
        }
        let before = entry_steps(&root[index - 1]);
        let after = root[index + 1..]
            .iter()
            .find(|event| event.starts_with("entry "))
            .map(entry_steps)
            .unwrap();
        per_shape.push(after - before);
    }
    per_shape
}

/// T3: interrupted and restarted calls at patched sites return the same
/// values, restart through the same stops (including restart_syscall) and
/// show the same frames as under ptrace.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t3_restart_matrix() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("restart").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    assert_report_has(
        &ptrace,
        &[
            "nanosleep-suppressed ret=0 errno=0 rcx=tp_site_end r11=0x246",
            // The signal is pending when each sleep starts, so every
            // interrupted sleep (restarted or not) reports about the whole
            // request as its remaining time (the fixture's rem_class).
            "nanosleep-suppressed rem=whole",
            "nanosleep-handled rem=whole",
            "clock_nanosleep-suppressed rem=whole",
            "clock_nanosleep-handled rem=whole",
            "nanosleep-suppressed-tail rem=whole",
            "read-eintr ret=-4 errno=4 rcx=tp_site_end r11=0x246",
            "read-restart ret=1 errno=0 rcx=tp_site_end r11=0x246",
            "futex-restart ret=-11 errno=11 rcx=tp_site_end r11=0x246",
            "futex-eintr ret=-4 errno=4 rcx=tp_site_end r11=0x246",
            "ppoll ret=-4 errno=4 rcx=tp_site_end r11=0x246",
            "epoll_pwait ret=-4 errno=4 rcx=tp_site_end r11=0x246",
            "pause ret=-4 errno=4 rcx=tp_site_end r11=0x246",
        ],
    );
    assert!(
        ptrace.count_events("tagged restart_syscall") >= 3,
        "restart_syscall re-entries: {:#?}",
        ptrace.all_events()
    );
    assert!(
        ptrace
            .report
            .contains("read-restart 0: sig=10 code=-6 value=-1 rip=tp_site rax=0 "),
        "SA_RESTART frame shows rip == S:\n{}",
        ptrace.report
    );
}

/// T6a: fork, vfork, clone3 and a thread created through a patched site;
/// the thread restores the site and disables patching.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t6a_fork_family_through_a_patched_site() {
    let ptrace = run_p2("fork_family", None, false).await;
    let trap_only = run_p2("fork_family", Some(SitePatching::On), false).await;
    let ptrace_tail = run_p2("fork_family", None, true).await;
    let trap_only_tail = run_p2("fork_family", Some(SitePatching::On), true).await;
    for (trap_only, ptrace) in [(&trap_only, &ptrace), (&trap_only_tail, &ptrace_tail)] {
        // The site bytes before the thread are the disclosed text residual:
        // patched under trap-only, original under ptrace.
        assert_report_has(ptrace, &["before thread site bytes 0f 05"]);
        assert_report_has(trap_only, &["before thread site bytes cd 80"]);
        let masked = P2Run {
            report: trap_only.report.replace(
                "before thread site bytes cd 80",
                "before thread site bytes 0f 05",
            ),
            ..clone_run(trap_only)
        };
        assert_equal_runs(&masked, ptrace);
        assert_eq!(ptrace.status, ExitStatus::Exited(0));
        assert_report_has(
            ptrace,
            &[
                "fork child rcx=tp_site_end r11=0x246",
                "fork child status exited=1 code=7",
                "vfork child status exited=1 code=7",
                "clone3 child status exited=1 code=7",
                "after thread site bytes 0f 05",
                "getpid after thread pid=1",
                "done",
            ],
        );
        assert_eq!(
            trap_only.table_state,
            Some(TableState::Disabled(DisabledReason::MultiTask))
        );
        assert_eq!(
            trap_only.site_state,
            Some(SiteState::Retired(RetiredReason::MultiTask))
        );
        assert_eq!(trap_only.patched_sites, 0);
    }
    assert_eq!(ptrace.counts.fork_stops() + ptrace.counts.clone_stops(), 0);
    assert_eq!(
        ptrace_tail.counts.fork_stops(),
        3,
        "fork, grandchild, clone3"
    );
    assert_eq!(ptrace_tail.counts.vfork_stops(), 1);
    assert_eq!(ptrace_tail.counts.clone_stops(), 1);
}

fn clone_run(run: &P2Run) -> P2Run {
    P2Run {
        status: run.status,
        stops: run.stops.clone(),
        tool_events: run.tool_events.clone(),
        counts: run.counts.clone(),
        report: run.report.clone(),
        patched_sites: run.patched_sites,
        table_state: run.table_state,
        site_state: run.site_state,
        site: run.site,
    }
}

/// T7c: a real IA-32 `int 0x80` (not at a patched site) kills the process
/// with SIGSYS as plain ptrace's filter does, without any Tool event.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t7c_foreign_int_0x80_dies_of_sigsys() {
    let ptrace = run_p2("foreign_int80", None, false).await;
    let trap_only = run_p2("foreign_int80", Some(SitePatching::On), false).await;
    assert_eq!(trap_only.status, ptrace.status);
    assert_eq!(trap_only.report, ptrace.report);
    assert!(
        trap_only.tool_events == ptrace.tool_events,
        "{}",
        first_divergence(&trap_only.tool_events, &ptrace.tool_events)
    );
    assert!(
        ptrace
            .report
            .contains("child signaled=1 termsig=31 coredump="),
        "{}",
        ptrace.report
    );
    assert!(!ptrace.report.contains("int80 returned"));
    // The one difference: trap-only's filter reports the int 0x80 as a
    // seccomp stop, which plain ptrace's filter never does.
    let mut stops = trap_only.stops.clone();
    let mut removed = 0;
    for events in stops.values_mut() {
        if let Some(index) = events.iter().position(|event| event == "seccomp 20") {
            events.remove(index);
            removed += 1;
        }
    }
    assert_eq!(removed, 1, "{:#?}", trap_only.stops);
    assert_eq!(stops, ptrace.stops);
    let (t, p) = (&trap_only.counts, &ptrace.counts);
    assert_eq!(t.seccomp_stops(), p.seccomp_stops() + 1);
    assert_eq!(t.stop_events(), p.stop_events() + 1);
    assert_eq!(
        (
            t.tracees_started(),
            t.exited_tracees(),
            t.signal_stops(),
            t.exec_stops(),
            t.fork_stops(),
            t.vfork_stops(),
            t.clone_stops(),
            t.vfork_done_stops()
        ),
        (
            p.tracees_started(),
            p.exited_tracees(),
            p.signal_stops(),
            p.exec_stops(),
            p.fork_stops(),
            p.vfork_stops(),
            p.clone_stops(),
            p.vfork_done_stops()
        )
    );
    assert!(
        !trap_only
            .all_events()
            .iter()
            .any(|event| event.contains("SIGSYS")),
        "the Tool saw the SIGSYS"
    );
}

/// T8: rcx and r11 after a patched site, in a signal frame and in a fork
/// child equal ptrace's, including with DF and AC set.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_t8_rcx_r11_view() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("rcx_r11").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    assert_report_has(
        &ptrace,
        &[
            "t8 0 pid=1 rcx=t8_site_end r11=0x293",
            "t8 3 pid=1 rcx=t8_site_end r11=0x293",
            "t8 df pid=1 rcx=t8_site_end r11=0x697",
            "t8 ac pid=1 rcx=t8_site_end r11=0x40246",
            "fork child rcx=tp_site_end r11=0x246",
        ],
    );
    assert!(
        ptrace.report.contains("read 0: sig=10") && ptrace.report.contains("rcx=tp_site_end r11="),
        "{}",
        ptrace.report
    );
}

/// A patch whose readback does not show `int 0x80` fails the run closed
/// with `TrapOnlyPatchReadback`.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_patch_readback_mismatch_fails_closed() {
    let error = run_p2_with("sig_pending", Some(SitePatching::On), false, true)
        .await
        .expect_err("a failed patch readback must end the run");
    let text = format!("{error:#} {error:?}");
    assert!(text.contains("TrapOnlyPatchReadback"), "{text}");
}

/// A signal left pending for the tracer's final resume of a patched stop is
/// handled as under ptrace. After an in-place inject or an emulation the
/// resume is from a syscall-exit stop, where the kernel sends it; after a
/// tail inject ptrace resumes the seccomp stop, where the kernel ignores it,
/// so the trap-only tail hop must drop it rather than pass it at its own
/// syscall-exit stop.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_resume_signal_matches_ptrace() {
    let [ptrace, trap_only, _, trap_only_tail] = compare_mode("resume_signal").await;
    assert_patched(&trap_only, SiteState::Live);
    assert_patched(&trap_only_tail, SiteState::Live);
    assert_report_has(
        &ptrace,
        &[
            "getpid returned pid=1",
            "resume_inject 0: sig=10 code=128 value=-1 rip=tp_site_end rax=<pid> ",
            "emulated getpid returned 4242",
            "resume_emulate 0: sig=10 code=0 value=-1 rip=tp_site_end rax=4242 ",
            "tail getpid returned pid=1",
        ],
    );
    assert!(
        !ptrace.report.contains("resume_tail 0:"),
        "ptrace drops a signal passed on resume from the seccomp stop:\n{}",
        ptrace.report
    );
}

/// Guest code that runs the private page's traced slot outside any hop fails
/// the run closed with `TrapOnlyStraySlotStop`.
#[tokio::test(flavor = "current_thread")]
async fn trap_only_p2_stray_slot_stop_fails_closed() {
    let error = run_p2_with("stray_slot", Some(SitePatching::On), false, false)
        .await
        .expect_err("a slot stop outside a hop must end the run");
    let text = format!("{error:#} {error:?}");
    assert!(text.contains("TrapOnlyStraySlotStop"), "{text}");
}
