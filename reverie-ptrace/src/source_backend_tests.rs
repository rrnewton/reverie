/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Real ordinary Command controls. No synthetic source receipt/epoch is issued.
use std::sync::atomic::AtomicUsize;

use reverie::Guest;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;

use super::*;

const SOURCE_GUEST: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>
static int pipefd[2];
static int child_mode;
static char *source_page;
static void child_done(void) {
    if (child_mode == 9) {
        // This operation is in the NEW observation tier, not SourceTool's
        // original write subscription. It must retain ordinary Linux semantics.
        errno = 0;
        long advised = syscall(SYS_madvise, source_page, 4096, MADV_NORMAL);
        int advised_errno = errno; // save the CHILD's errno before any diagnostic
        int preserved = memcmp(source_page, "ABCD", 4) == 0;
        char diagnostic[160];
        int length = snprintf(diagnostic, sizeof(diagnostic),
            "source child mode=9 madvise_result=%ld madvise_errno=%d bytes_preserved=%d\n",
            advised, advised_errno, preserved);
        if (length < 0 || (size_t)length >= sizeof(diagnostic)) _exit(97);
        struct iovec record = {.iov_base=diagnostic, .iov_len=(size_t)length};
        if (syscall(SYS_writev, STDERR_FILENO, &record, 1) != length) _exit(97);
        if (advised != 0) _exit(95);
        if (!preserved) _exit(96);
    }
    struct iovec effect = {.iov_base="X", .iov_len=1};
    if (syscall(SYS_writev, pipefd[1], &effect, 1) != 1) _exit(80);
    _exit(0);
}
static void ill(int sig, siginfo_t *si, void *context) {
    ucontext_t *u = context;
    if ((uintptr_t)u->uc_mcontext.gregs[REG_RIP] != 0x71000002) _exit(81);
    if (!u->uc_mcontext.gregs[REG_RAX]) child_done();
    // A guest call to the real private stub returns through its original stack.
    uintptr_t *sp = (void *)u->uc_mcontext.gregs[REG_RSP];
    u->uc_mcontext.gregs[REG_RIP] = *sp;
    u->uc_mcontext.gregs[REG_RSP] += sizeof(uintptr_t);
}
static long private_clone(void) {
    register long rax __asm__("rax") = SYS_clone;
    register long r10 __asm__("r10") = 0;
    register long r8 __asm__("r8") = 0;
    __asm__ volatile("call *%[stub]" : "+a"(rax)
        : [stub]"r"((void *)0x71000000), "D"(0x00800000L | SIGCHLD),
          "S"(0L), "d"(0L), "r"(r10), "r"(r8)
        : "rcx", "r11", "memory");
    return rax;
}
int main(int argc, char **argv) {
    if (argc != 2) return 82;
    int mode = atoi(argv[1]);
    char *p = mmap(0, 4096, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return 83;
    memcpy(p, "ABCD", 4);
    child_mode = mode;
    source_page = p;
    if ((mode >= 1 && mode <= 3) || mode == 9 || mode == 10) {
        if (pipe2(pipefd, 0)) return 84;
        if (mode == 10) {
            // An exited child must remain a zombie while this single-threaded
            // parent is held. Do not inherit SIG_IGN or SA_NOCLDWAIT silently.
            struct sigaction keep_child = {.sa_handler=SIG_DFL, .sa_flags=0};
            struct sigaction actual_child;
            if (sigemptyset(&keep_child.sa_mask) ||
                sigaction(SIGCHLD, &keep_child, 0) ||
                sigaction(SIGCHLD, 0, &actual_child)) return 98;
            if (actual_child.sa_handler != SIG_DFL ||
                (actual_child.sa_flags & SA_NOCLDWAIT)) return 98;
            // Cancellation at the actual pre-effect stop must leave captured
            // stdout empty; an accidentally executed child writes its X here.
            close(pipefd[1]);
            pipefd[1] = STDOUT_FILENO;
        }
        struct sigaction sa = {.sa_sigaction=ill, .sa_flags=SA_SIGINFO};
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGILL, &sa, 0)) return 85;
        long pid;
        if (mode == 1) pid = syscall(SYS_clone, SIGCHLD, 0, 0, 0, 0);
        else if (mode == 2 || mode == 9) pid = private_clone();
        else pid = syscall(SYS_write, 777, 0, 0); // Tool's real private injection
        if (!pid) child_done();
        if (pid < 0) return 86;
        int status = 0;
        pid_t waited = waitpid(pid, &status, 0);
        if (waited != pid || !WIFEXITED(status) || WEXITSTATUS(status)) {
            fprintf(stderr, "source fixture mode=%d child=%ld waited=%ld raw_status=%#x errno=%d\n",
                    mode, pid, (long)waited, status, errno);
            return 87;
        }
        close(pipefd[1]);
        char got[2];
        if (read(pipefd[0], got, 2) != 1 || got[0] != 'X') return 88;
        if (read(pipefd[0], got, 2) != 0) return 89;
        if (waitpid(-1, &status, WNOHANG) != -1 || errno != ECHILD) return 90;
    }
    if (mode == 4 && madvise(p, 4096, MADV_FREE)) return 91;
    if (mode == 7) {
        long actual = syscall(0x40000027); // actual x32 getpid or native ENOSYS
        if (actual != getpid() && !(actual == -1 && errno == ENOSYS)) return 93;
    }
    if (mode == 8 && syscall(SYS_write, 779, 0, 0) != 0) return 94;
    if (syscall(SYS_write, 778, p, 4) != 4) return 92;
    return 0;
}
"#;

fn command(mode: u8) -> Command {
    static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
        let path =
            std::env::temp_dir().join(format!("reverie-source-epoch-{}", std::process::id()));
        let source = path.with_extension("c");
        std::fs::write(&source, SOURCE_GUEST).unwrap();
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g"])
            .arg(&source)
            .arg("-o")
            .arg(&path)
            .status()
            .expect("compile source epoch fixture");
        assert!(status.success(), "source epoch fixture must compile");
        path
    });
    let mut command = Command::new(GUEST.as_path());
    command.arg(mode.to_string());
    if mode == 6 {
        // An actual inherited second filter. It happens to Allow everything,
        // but unknown ancestry cannot be guessed safe merely from this fixture.
        unsafe {
            command.pre_exec(|| {
                let mut insn = libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x7fff0000,
                };
                let program = libc::sock_fprog {
                    len: 1,
                    filter: &mut insn,
                };
                Errno::result(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
                Errno::result(libc::syscall(libc::SYS_seccomp, 1, 0, &program))?;
                Ok(())
            });
        }
    }
    command
}

#[derive(Default)]
struct SourceLog {
    observations: StdMutex<Vec<(bool, Vec<u8>)>>,
    retention_drops: Arc<AtomicUsize>,
}
#[reverie::global_tool]
impl GlobalTool for SourceLog {
    type Config = u8;
    type Request = (bool, Vec<u8>);
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, result: Self::Request) {
        self.observations.lock().unwrap().push(result);
    }
}
struct Retention(Arc<AtomicUsize>);
impl Drop for Retention {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct SourceTool;
#[reverie::tool]
impl Tool for SourceTool {
    type GlobalState = SourceLog;
    type ThreadState = ();
    fn subscriptions(_: &u8) -> Subscription {
        [Sysno::write].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = call.into_parts();
        if args.arg0 == 779 {
            return Ok(0);
        } // actual ordinary skip/emulation
        if args.arg0 == 777 {
            // Goes through the real injected private stub and actual seccomp
            // observation; no generic clone flag/result is changed.
            return Ok(guest
                .inject(Syscall::Other(
                    Sysno::clone,
                    SyscallArgs::new(
                        (libc::CLONE_UNTRACED | libc::SIGCHLD) as usize,
                        0,
                        0,
                        0,
                        0,
                        0,
                    ),
                ))
                .await?);
        }
        if args.arg0 != 778 {
            return Ok(guest.inject(call).await?);
        }
        if *guest.config() == 5 {
            let regs = guest.regs().await;
            guest.set_regs(regs).await?; // same values still invalidate the original stop
        }
        let retention = Retention(guest.local_global_state().unwrap().retention_drops.clone());
        let result = guest
            .read_native_source(args.arg1, args.arg2, Box::new(retention))
            .await;
        if matches!(*guest.config(), 0 | 8) {
            assert_eq!(
                result,
                Ok(b"ABCD".to_vec()),
                "ordinary Command must be admitted"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(reverie::syscalls::NativeUserReadError::Refused(_))
                ),
                "actual exposure/control must refuse: {result:?}"
            );
        }
        guest
            .send_rpc((result.is_ok(), result.unwrap_or_default()))
            .await;
        Ok(4)
    }
}

async fn run(mode: u8) {
    let tracer = TracerBuilder::<SourceTool>::new(command(mode))
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), tracer.wait_completion())
        .await
        .expect("ordinary source Command deadline");
    let completed = match outcome {
        ToolRunOutcome::Complete(completed) => completed,
        ToolRunOutcome::CleanupPending(pending) => {
            eprintln!("source Command pending: {:#?}", pending.failure());
            eprintln!("source callbacks: {:#?}", pending.callback_diagnostics());
            let retained = pending.quarantine();
            panic!("source Command did not complete: {retained:#}");
        }
        ToolRunOutcome::UnsupportedBackend(_) => panic!("ordinary source Command unsupported"),
    };
    assert_eq!(
        completed.result.unwrap(),
        ExitStatus::Exited(0),
        "one child and one effect"
    );
    assert_eq!(
        *completed.global_state.observations.lock().unwrap(),
        vec![(
            matches!(mode, 0 | 8),
            if matches!(mode, 0 | 8) {
                b"ABCD".to_vec()
            } else {
                vec![]
            }
        )]
    );
    assert_eq!(
        completed
            .global_state
            .retention_drops
            .load(Ordering::SeqCst),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_command_source_positive() {
    run(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_traced_clone_still_runs_once_and_revokes_source() {
    run(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn private_stub_untraced_clone_runs_once_and_irreversibly_revokes_source() {
    run(2).await;
}
#[tokio::test(flavor = "current_thread")]
async fn injected_untraced_clone_runs_once_and_irreversibly_revokes_source() {
    run(3).await;
}

#[tokio::test(flavor = "current_thread")]
async fn untraced_child_newly_observed_madvise_preserves_native_semantics() {
    // Intentionally a normal required-success regression, not should_panic:
    // the current inherited TRACE tier returns ENOSYS in this untraced child.
    run(9).await;
}

#[derive(Debug, thiserror::Error)]
#[error("cancel at the actual injected clone Seccomp stop")]
struct SourceInjectedStopCancellation;

struct SourceInjectedStopGateReset;
impl Drop for SourceInjectedStopGateReset {
    fn drop(&mut self) {
        crate::task::SOURCE_INJECTED_STOP_GATE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceProcParent {
    pid: u32,
    parent: u32,
}

// Test observation only, never stop/wait/signal authority. comm is opaque bytes
// and can contain parentheses, newlines and non-UTF-8; only its suffix is text.
fn source_proc_parent(pid: u32, raw: &[u8]) -> anyhow::Result<Option<SourceProcParent>> {
    anyhow::ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid census PID");
    let open = raw
        .windows(2)
        .position(|pair| pair == b" (")
        .context("missing stat comm opening")?;
    let end = raw
        .windows(2)
        .rposition(|pair| pair == b") ")
        .context("missing stat comm closing")?;
    anyhow::ensure!(end >= open + 2, "malformed stat comm");
    anyhow::ensure!(
        std::str::from_utf8(&raw[..open])?.parse::<u32>()? == pid,
        "census stat PID changed"
    );
    let fields: Vec<_> = std::str::from_utf8(&raw[end + 2..])?
        .split_ascii_whitespace()
        .collect();
    anyhow::ensure!(fields.len() >= 20, "short census stat");
    anyhow::ensure!(
        fields[0].len() == 1 && b"RSDZTtXxKWPI".contains(&fields[0].as_bytes()[0]),
        "unknown census task state"
    );
    if (fields[1], fields[2], fields[3]) == ("0", "-1", "-1") {
        // A removed sampled task may have transferred its PID to a live execer.
        // This observation cannot classify the current PID's parent.
        return Ok(None);
    }
    let parent = fields[1].parse::<u32>()?;
    anyhow::ensure!(parent <= i32::MAX as u32, "invalid census parent PID");
    fields[2].parse::<u32>()?;
    fields[3].parse::<u32>()?;
    fields[19].parse::<u64>()?;
    Ok(Some(SourceProcParent { pid, parent }))
}

fn source_census_deadline(deadline: Instant) -> anyhow::Result<()> {
    anyhow::ensure!(
        Instant::now() < deadline,
        "source child census exceeded original fixture deadline"
    );
    Ok(())
}

fn source_resolve_proc_parent(
    pid: u32,
    deadline: Instant,
    mut read_current: impl FnMut() -> std::io::Result<Vec<u8>>,
) -> anyhow::Result<SourceProcParent> {
    let mut inconclusive = Vec::new();
    for _ in 0..2 {
        source_census_deadline(deadline)?;
        let raw = read_current().with_context(|| format!("read census PID {pid} stat"));
        source_census_deadline(deadline)?;
        // Even ENOENT/ESRCH leaves this test's census incomplete. No failure or
        // unsupported proc interface is silently converted to zero children.
        let raw = raw?;
        if let Some(parent) = source_proc_parent(pid, &raw).with_context(|| {
            format!(
                "parse census PID {pid} stat: {} bytes, prefix {:?}",
                raw.len(),
                &raw[..raw.len().min(512)]
            )
        })? {
            return Ok(parent);
        }
        inconclusive.push(raw[..raw.len().min(512)].to_vec());
    }
    anyhow::bail!("census PID {pid} still has an inconclusive removed-task stat: {inconclusive:?}")
}

// A procfs read error alone is never disappearance. Only the kernel's task
// lookup can corroborate it; a live replacement still needs its own stat row.
fn source_resolve_census_candidate(
    pid: u32,
    deadline: Instant,
    mut read_current: impl FnMut() -> std::io::Result<Vec<u8>>,
    probe_task: impl FnOnce() -> std::io::Result<()>,
) -> anyhow::Result<Option<SourceProcParent>> {
    anyhow::ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid census PID");
    let error = match source_resolve_proc_parent(pid, deadline, &mut read_current) {
        Ok(parent) => return Ok(Some(parent)),
        Err(error) => error,
    };
    let missing_stat = |error: &anyhow::Error| {
        matches!(
            error
                .downcast_ref::<std::io::Error>()
                .and_then(|error| error.raw_os_error()),
            Some(libc::ENOENT | libc::ESRCH)
        )
    };
    if !missing_stat(&error) {
        return Err(error);
    }
    source_census_deadline(deadline)?;
    let probe = probe_task();
    source_census_deadline(deadline)?;
    let absent = match probe {
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => true,
        Err(error) => {
            return Err(error).with_context(|| format!("probe census PID {pid} task"));
        }
        Ok(()) => false,
    };
    // Even after confirmed absence, inspect a replacement visible on this one
    // fresh read. It cannot be a new child of the still-held original root.
    match source_resolve_proc_parent(pid, deadline, read_current) {
        Ok(parent) => Ok(Some(parent)),
        Err(error) if absent && missing_stat(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn source_probe_census_task(pid: u32) -> std::io::Result<()> {
    use std::os::fd::FromRawFd;

    // PIDFD_THREAD == O_EXCL: a reused nonleader TID must count as present too.
    // This descriptor is only an existence observation, never a signal, wait,
    // source-read or cleanup owner. In particular, opening it does not reap.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as i32, libc::O_EXCL) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
    Ok(())
}

fn source_census_namespace(self_pid: u32, status: &[u8]) -> anyhow::Result<()> {
    // Proc NSpid lists each level from the proc mount's PID namespace to this
    // task's namespace. One matching entry binds proc to pidfd_open's numbers.
    // Parse only this field: the surrounding task name can contain opaque bytes.
    let mut rows = status
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"NSpid:"));
    let row = rows
        .next()
        .context("proc census lacks NSpid namespace evidence")?;
    anyhow::ensure!(rows.next().is_none(), "duplicate census NSpid field");
    let mut pids = std::str::from_utf8(row)?.split_ascii_whitespace();
    let pid = pids.next().context("empty census NSpid")?.parse::<u32>()?;
    anyhow::ensure!(
        self_pid > 0 && pid == self_pid && pids.next().is_none(),
        "proc census and task lookup PID namespaces differ"
    );
    Ok(())
}

fn source_census_root_held(receipt: &crate::task::SourceInjectedStopReceipt) -> anyhow::Result<()> {
    let held = receipt.held.lock().unwrap();
    anyhow::ensure!(
        held.as_ref().is_some_and(|held| {
            held.armed
                && held.root_tid == receipt.tid
                && held.status == HeldRootStopStatus::Syscall
                && held.terminal.same_generation(&receipt.terminal)
        }),
        "census lost the original held Seccomp generation"
    );
    anyhow::ensure!(
        !receipt.terminal.exit_stop_observed()
            && receipt.terminal.observed_exit_status()?.is_none(),
        "census root reached EXIT or terminal state"
    );
    Ok(())
}

fn source_parent_census(
    deadline: Instant,
    receipt: &crate::task::SourceInjectedStopReceipt,
) -> anyhow::Result<Vec<SourceProcParent>> {
    source_census_deadline(deadline)?;
    source_census_root_held(receipt)?;
    source_census_namespace(std::process::id(), &std::fs::read("/proc/self/status")?)?;
    let tasks = std::fs::read_dir(format!("/proc/{}/task", receipt.tid))?
        .collect::<std::io::Result<Vec<_>>>()?;
    anyhow::ensure!(
        tasks.len() == 1 && tasks[0].file_name() == receipt.tid.to_string().as_str(),
        "census root is not the original single-threaded process"
    );
    source_census_deadline(deadline)?;
    // Mode10 checked real SIGCHLD default/no-SA_NOCLDWAIT before reaching this
    // authentic stop. Its process clone has no THREAD/PARENT/AUTOREAP flag; its
    // child only writes X and exits. While the sole original parent is held,
    // any such child remains live or zombie, so a corroborated absent candidate
    // cannot be its child. This is not a general atomic process-tree census.
    let entries = std::fs::read_dir("/proc").context("enumerate source child census")?;
    let mut parents = Vec::new();
    for entry in entries {
        source_census_deadline(deadline)?;
        let entry = entry.context("read source child census entry")?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let path = entry.path().join("stat");
        if let Some(parent) = source_resolve_census_candidate(
            pid,
            deadline,
            || std::fs::read(&path),
            || source_probe_census_task(pid),
        )? {
            parents.push(parent);
        }
    }
    source_census_root_held(receipt)?;
    source_census_deadline(deadline)?;
    Ok(parents)
}

fn source_census_children(parents: &[SourceProcParent], parent: Pid) -> Vec<Pid> {
    parents
        .iter()
        .filter(|entry| entry.parent == parent.as_raw() as u32)
        .map(|entry| Pid::from_raw(entry.pid as i32))
        .collect()
}

fn source_proc_stat_fixture(comm: &[u8], parent: &str, group: &str, session: &str) -> Vec<u8> {
    let mut fields = vec!["0"; 20];
    fields[0] = "S";
    fields[1] = parent;
    fields[2] = group;
    fields[3] = session;
    fields[19] = "456";
    let mut raw = b"123 (".to_vec();
    raw.extend_from_slice(comm);
    raw.extend_from_slice(b") ");
    raw.extend_from_slice(fields.join(" ").as_bytes());
    raw.push(b'\n');
    raw
}

#[test]
fn source_parent_census_preserves_opaque_comm_and_refuses_malformed_stat() {
    let raw = source_proc_stat_fixture(b"odd ) (\xff\nname", "42", "123", "123");
    assert_eq!(
        source_proc_parent(123, &raw).unwrap(),
        Some(SourceProcParent {
            pid: 123,
            parent: 42
        })
    );
    assert!(source_proc_parent(124, &raw).is_err());
    assert!(source_proc_parent(123, b"123 (short) S 42 123 123\n").is_err());
    for parent in ["-1", "unknown", "2147483648"] {
        assert!(
            source_proc_parent(123, &source_proc_stat_fixture(b"bad", parent, "123", "123"))
                .is_err()
        );
    }
    assert!(source_proc_parent(123, &source_proc_stat_fixture(b"bad", "0", "-1", "123")).is_err());
    let mut invalid_tail = raw;
    invalid_tail.push(0xff);
    assert!(source_proc_parent(123, &invalid_tail).is_err());
}

#[test]
fn source_parent_census_rechecks_ambiguity_and_refuses_incomplete_observation() {
    // Controlled parser/IO inputs, not native stop or process authority. The
    // cancellation test below supplies the actual owned-child discriminator.
    let deadline = Instant::now() + Duration::from_secs(5);
    let stale = source_proc_stat_fixture(b"removed", "0", "-1", "-1");
    let current = source_proc_stat_fixture(b"exec replacement", "42", "123", "123");
    let mut reads = std::collections::VecDeque::from([Ok(stale.clone()), Ok(current)]);
    let row = source_resolve_proc_parent(123, deadline, || reads.pop_front().unwrap()).unwrap();
    assert_eq!(
        row,
        SourceProcParent {
            pid: 123,
            parent: 42
        }
    );
    assert!(
        reads.is_empty(),
        "fresh read of the same candidate required"
    );
    let mut reads = std::collections::VecDeque::from([Ok(stale.clone()), Ok(stale.clone())]);
    assert!(source_resolve_proc_parent(123, deadline, || reads.pop_front().unwrap()).is_err());
    assert!(reads.is_empty(), "two reads, not retry until success");
    for errno in [
        libc::ENOENT,
        libc::ESRCH,
        libc::EACCES,
        libc::EIO,
        libc::EINTR,
    ] {
        let mut reads = std::collections::VecDeque::from([
            Ok(stale.clone()),
            Err(std::io::Error::from_raw_os_error(errno)),
        ]);
        let error =
            source_resolve_proc_parent(123, deadline, || reads.pop_front().unwrap()).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(errno)
        );
        assert!(reads.is_empty());
    }
    let mut reads = 0;
    assert!(
        source_resolve_proc_parent(123, Instant::now(), || {
            reads += 1;
            Ok(stale.clone())
        })
        .is_err()
    );
    assert_eq!(reads, 0, "an expired original deadline allows no IO");
}

#[test]
fn source_parent_census_namespace_requires_current_single_level() {
    // Modeled proc-status bytes, not native namespace or stop evidence.
    assert!(source_census_namespace(123, b"Name:\t\xff\nNSpid:\t123\n").is_ok());
    for raw in [
        b"Name:\tmissing\n".as_slice(),
        b"NSpid:\n",
        b"NSpid:\t124\n",
        b"NSpid:\t0\n",
        b"NSpid:\t123\t123\n",
        b"NSpid:\t123\nNSpid:\t123\n",
        b"NSpid:\t\xff\n",
    ] {
        assert!(source_census_namespace(123, raw).is_err(), "{raw:?}");
    }
}

#[test]
fn source_parent_census_disappearance_requires_kernel_absence() {
    // Modeled stat/probe outcomes. The real census uses PIDFD_THREAD and the
    // actual original held root; these closures supply neither authority.
    let deadline = Instant::now() + Duration::from_secs(5);
    for stat_errno in [libc::ENOENT, libc::ESRCH] {
        let mut reads = 0;
        let mut probes = 0;
        let result = source_resolve_census_candidate(
            123,
            deadline,
            || {
                reads += 1;
                Err(std::io::Error::from_raw_os_error(stat_errno))
            },
            || {
                probes += 1;
                Err(std::io::Error::from_raw_os_error(libc::ESRCH))
            },
        );
        assert_eq!(result.unwrap(), None);
        assert_eq!((reads, probes), (2, 1));
    }
    for probe_errno in [
        libc::ENOENT,
        libc::EPERM,
        libc::EACCES,
        libc::EIO,
        libc::EINTR,
        libc::ENOSYS,
        libc::EINVAL,
        libc::EMFILE,
    ] {
        let error = source_resolve_census_candidate(
            123,
            deadline,
            || Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
            || Err(std::io::Error::from_raw_os_error(probe_errno)),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(probe_errno)
        );
    }
    for stat_errno in [libc::EACCES, libc::EPERM, libc::EIO, libc::EINTR] {
        let error = source_resolve_census_candidate(
            123,
            deadline,
            || Err(std::io::Error::from_raw_os_error(stat_errno)),
            || panic!("an unknown stat error must not reach the absence probe"),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(stat_errno)
        );
    }
}

#[test]
fn source_parent_census_live_replacement_requires_its_actual_parent_row() {
    // Modeled live/nonchild and zombie/child rows. A successful task probe must
    // not classify either as gone, including after an old-task stat error.
    let deadline = Instant::now() + Duration::from_secs(5);
    for (parent, probe_absent) in [("7", false), ("42", false), ("7", true), ("42", true)] {
        let mut current = source_proc_stat_fixture(b"replacement", parent, "123", "123");
        if parent == "42" {
            let state = current.windows(2).rposition(|pair| pair == b") ").unwrap() + 2;
            current[state] = b'Z';
        }
        let mut reads = std::collections::VecDeque::from([
            Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
            Ok(current),
        ]);
        let row = source_resolve_census_candidate(
            123,
            deadline,
            || reads.pop_front().unwrap(),
            || {
                if probe_absent {
                    Err(std::io::Error::from_raw_os_error(libc::ESRCH))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap()
        .expect("a live PID replacement cannot disappear from the census");
        assert_eq!(row.parent, parent.parse::<u32>().unwrap());
        assert!(
            reads.is_empty(),
            "inspect the current replacement exactly once"
        );
        assert_eq!(
            source_census_children(&[row], Pid::from_raw(42)),
            if parent == "42" {
                vec![Pid::from_raw(123)]
            } else {
                vec![]
            }
        );
    }
    for errno in [
        libc::ENOENT,
        libc::ESRCH,
        libc::EACCES,
        libc::EIO,
        libc::EINTR,
    ] {
        let mut reads = std::collections::VecDeque::from([
            Err(std::io::Error::from_raw_os_error(libc::ENOENT)),
            Err(std::io::Error::from_raw_os_error(errno)),
        ]);
        let error = source_resolve_census_candidate(
            123,
            deadline,
            || reads.pop_front().unwrap(),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(errno)
        );
        assert!(reads.is_empty(), "no second probe or retry until success");
    }
}

#[test]
fn source_parent_census_disappearance_preserves_parser_refusals() {
    // Modeled ambiguity is not kernel disappearance. Keep the original strict
    // parser and its one fresh sentinel reread on the actual census path.
    let deadline = Instant::now() + Duration::from_secs(5);
    let stale = source_proc_stat_fixture(b"removed", "0", "-1", "-1");
    let mut reads = std::collections::VecDeque::from([Ok(stale.clone()), Ok(stale)]);
    assert!(
        source_resolve_census_candidate(
            123,
            deadline,
            || reads.pop_front().unwrap(),
            || panic!("a repeated removed-task sentinel is not disappearance"),
        )
        .is_err()
    );
    assert!(reads.is_empty());
    assert!(
        source_resolve_census_candidate(
            123,
            deadline,
            || Ok(b"123 (short) S 42\n".to_vec()),
            || panic!("malformed stat must not reach the absence probe"),
        )
        .is_err()
    );
    for tail in [
        vec![Ok(b"123 (short) S 42\n".to_vec())],
        vec![
            Ok(source_proc_stat_fixture(b"removed", "0", "-1", "-1")),
            Ok(source_proc_stat_fixture(b"removed", "0", "-1", "-1")),
        ],
        vec![Err(std::io::Error::from_raw_os_error(libc::EACCES))],
        vec![Err(std::io::Error::from_raw_os_error(libc::EIO))],
        vec![Err(std::io::Error::from_raw_os_error(libc::EINTR))],
    ] {
        let mut reads = std::collections::VecDeque::from([Err(std::io::Error::from_raw_os_error(
            libc::ENOENT,
        ))]);
        reads.extend(tail);
        assert!(
            source_resolve_census_candidate(
                123,
                deadline,
                || reads.pop_front().unwrap(),
                || Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
            )
            .is_err(),
            "kernel absence cannot excuse malformed or inaccessible fresh stat"
        );
        assert!(reads.is_empty());
    }
    let current = source_proc_stat_fixture(b"valid child", "42", "123", "123");
    let row = source_resolve_census_candidate(
        123,
        deadline,
        || Ok(current.clone()),
        || panic!("a valid parent row needs no absence probe"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        row,
        SourceProcParent {
            pid: 123,
            parent: 42
        }
    );
    assert!(
        source_resolve_census_candidate(
            123,
            Instant::now(),
            || panic!("expired deadline must not read"),
            || panic!("expired deadline must not probe"),
        )
        .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn injected_clone_intermediate_seccomp_cancellation_retires_without_effect() {
    let mut guest = command(10);
    guest
        .stdout(reverie::process::Stdio::piped())
        .stderr(reverie::process::Stdio::piped());
    let tracer = TracerBuilder::<SourceTool>::new(guest)
        .config(10)
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let session = tracer.ordinary_session.clone();
    let terminate = tracer.termination_handle().unwrap();
    let (entered, received) = tokio::sync::oneshot::channel();
    crate::task::SOURCE_INJECTED_STOP_GATE.with(|slot| {
        assert!(slot.borrow().is_none(), "one original test gate");
        *slot.borrow_mut() = Some(crate::task::SourceInjectedStopGate { tid: root, entered });
    });
    let _reset = SourceInjectedStopGateReset;
    let deadline = Instant::now() + Duration::from_secs(5);
    let completion = tracer.wait_with_output_completion();
    futures::pin_mut!(completion);
    let mut receipt =
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
            tokio::select! {
                receipt = received => receipt.expect("actual injected Seccomp receipt"),
                outcome = &mut completion => {
                    if let ToolRunOutcome::CleanupPending(pending) = outcome {
                        eprintln!("source stop gate pending: {:#?}", pending.failure());
                        let retained = pending.quarantine();
                        panic!("source stop gate owner retained: {retained:#}");
                    }
                    panic!("completion preceded actual injected Seccomp stop");
                }
            }
        })
        .await
        .expect("actual injected Seccomp stop within original source Command deadline");
    // Seal observations before any failure-only rescue, then request and finish
    // original cleanup before asserting them. Unknown cleanup still fails.
    let armed_at_receipt = receipt.held.lock().unwrap().as_ref().is_some_and(|held| {
        held.armed
            && held.root_tid == root
            && held.status == HeldRootStopStatus::Syscall
            && held.terminal.same_generation(&receipt.terminal)
    });
    let terminal_before = receipt.terminal.observed_exit_status();
    let census_before = source_parent_census(deadline, &receipt);
    let children_before = census_before
        .as_ref()
        .map(|parents| source_census_children(parents, root));
    let positive_before = census_before
        .as_ref()
        .map(|parents| source_census_children(parents, Pid::from_raw(std::process::id() as i32)));
    eprintln!(
        "source actual injected stop: tid={}, seccomp={}, nr={}, args={:?}, ip={:#x}, armed={armed_at_receipt}, terminal={terminal_before:?}, children={children_before:?}, tracer_children={positive_before:?}; controlled pause before effect step",
        receipt.tid, receipt.seccomp, receipt.number, receipt.arguments, receipt.ip
    );
    // Rescue only a missing lease, using the witness derived from the actual
    // same Stopped before the callback paused. The original predicate above
    // stays false in the arm-only mutant; cleanup cannot turn it into a pass.
    let recovered_for_failure = if !armed_at_receipt {
        let mut held = receipt.held.lock().unwrap();
        if held.is_none() {
            *held = receipt.failure_cleanup.take();
            held.is_some()
        } else {
            false
        }
    } else {
        false
    };
    eprintln!("source failure-only original-stop cleanup rescue={recovered_for_failure}");
    assert!(terminate.terminate(Error::Tool(anyhow::Error::new(
        SourceInjectedStopCancellation
    ))));
    let outcome = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        &mut completion,
    )
    .await
    .expect("original cleanup exceeded the single source Command deadline");
    let completed = match outcome {
        ToolRunOutcome::Complete(completed) => completed,
        ToolRunOutcome::CleanupPending(pending) => {
            eprintln!(
                "source intermediate cleanup pending: {:#?}",
                pending.failure()
            );
            eprintln!("source callbacks: {:#?}", pending.callback_diagnostics());
            let retained = pending.quarantine();
            panic!("actual intermediate stop was not retired: {retained:#}");
        }
        ToolRunOutcome::UnsupportedBackend(_) => panic!("ordinary source Command unsupported"),
    };
    let terminal_after = receipt.terminal.observed_exit_status();
    let notifier_retired = receipt.terminal.wait(Duration::ZERO);
    let held_after = receipt.held.lock().unwrap().is_some();
    eprintln!(
        "source actual injected cleanup: tid={}, terminal={terminal_after:?}, notifier_retired={notifier_retired}, held_after={held_after}",
        receipt.tid
    );
    let failure = completed
        .result
        .expect_err("cancellation became guest success");
    assert!(matches!(
        failure.primary(),
        Error::Tool(error) if error.downcast_ref::<SourceInjectedStopCancellation>().is_some()
    ));
    assert_eq!(failure.origin().phase, "ptrace supervisor termination");
    assert!(
        failure.secondary().is_empty(),
        "cleanup errors: {failure:#?}"
    );
    assert_eq!(receipt.tid, root);
    assert!(
        !receipt.seccomp,
        "private effect must pause at actual ENTRY, not inherited seccomp"
    );
    assert_eq!(receipt.number, Sysno::clone as u64);
    assert_eq!(
        receipt.arguments,
        [(libc::CLONE_UNTRACED | libc::SIGCHLD) as u64, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        receipt.ip,
        crate::cp::PRIVATE_PAGE_OFFSET + crate::cp::SYSCALL_INSTR_SIZE
    );
    assert!(
        armed_at_receipt,
        "actual intermediate stop lacked its original lease"
    );
    assert!(
        !recovered_for_failure,
        "passing control required failure rescue"
    );
    assert_eq!(terminal_before.unwrap(), None);
    assert!(
        positive_before.unwrap().contains(&root),
        "supported PPID census must identify our actually owned child"
    );
    assert!(
        children_before.unwrap().is_empty(),
        "clone ran before its effect step"
    );
    assert_eq!(
        terminal_after.unwrap(),
        Some(ExitStatus::Signaled(Signal::SIGKILL, false))
    );
    assert!(notifier_retired, "original notifier not retired");
    assert!(!held_after, "held stop not retired");
    let prefix = failure
        .captured_prefix()
        .expect("actual capture through EOF");
    assert_eq!(
        prefix.stdout(),
        b"",
        "child X effect executed after cancellation"
    );
    assert_eq!(prefix.stderr(), b"");
    assert!(
        completed
            .global_state
            .observations
            .lock()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        completed
            .global_state
            .retention_drops
            .load(Ordering::SeqCst),
        0
    );
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    assert_eq!(
        unsafe { libc::waitpid(root.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
        -1,
        "original completion must have reaped its exact root"
    );
    assert_eq!(Errno::last(), Errno::ECHILD);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_discard_exposure_refuses_source_after_return() {
    run(4).await;
}
#[tokio::test(flavor = "current_thread")]
async fn actual_same_value_register_control_invalidates_source_stop() {
    run(5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_result_before_true_join_retains_completion_and_caller() {
    let tracer = TracerBuilder::<SourceTool>::new(command(0))
        .config(0)
        .spawn()
        .await
        .unwrap();
    let session = tracer.ordinary_session.clone();
    let drops = tracer.gref.retention_drops.clone();
    let terminate = tracer.termination_handle().unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let (release, waiting) = std::sync::mpsc::channel();
    session
        .source_jobs
        .pause_next_retirement(crate::task::source_jobs::RetirementPause {
            entered: entered.clone(),
            release: waiting,
        });
    let completion = tracer.wait_completion();
    futures::pin_mut!(completion);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !entered.load(Ordering::Acquire) {
            tokio::select! {
                _ = &mut completion => panic!("completion preceded real thread retirement"),
                _ = tokio::time::sleep(Duration::from_millis(1)) => {},
            }
        }
    })
    .await
    .expect("real reader did not reach TLS retirement pause");
    assert_eq!(session.source_jobs.pending_jobs(), 1);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "IO return cannot release caller custody"
    );
    assert!(terminate.terminate(anyhow::anyhow!("source cancellation control").into()));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut completion)
            .await
            .is_err(),
        "original completion must remain pending through cancelled unjoined worker"
    );
    assert_eq!(session.source_jobs.pending_jobs(), 1);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "callback cancellation cannot release custody"
    );
    release.send(()).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(3), &mut completion)
        .await
        .unwrap();
    let ToolRunOutcome::Complete(completed) = outcome else {
        panic!("released actual thread did not join");
    };
    assert!(completed.result.is_err());
    assert!(
        completed
            .global_state
            .observations
            .lock()
            .unwrap()
            .is_empty(),
        "cancelled bytes were published"
    );
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn inherited_filter_stack_cannot_masquerade_as_complete_source_observation() {
    run(6).await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_x32_entry_cannot_hide_birth_capable_abi_history() {
    run(7).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_emulated_syscall_does_not_poison_later_source() {
    run(8).await;
}

#[path = "source_observation_equivalence_tests.rs"]
mod observation_equivalence;
