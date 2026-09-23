/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The ordinary ptrace post-exec resume must not consume a newly armed timer.
//!
//! The hosted control requires ptrace but no PMU. The explicitly selected
//! hardware control also requires a working precise RCB timer; missing PMU
//! support is a failure, never a successful early return.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Backend;
use reverie::Errno;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie_ptrace::PtraceBackend;
use reverie_ptrace::PtraceBackendStatsSnapshot;
use serde::Deserialize;
use serde::Serialize;

const ENTRY: u64 = 0x401000;
const LOOP_HEAD: u64 = ENTRY + 7;
const LOOP_BRANCHES: u32 = 2_000_000;
const TIMER_RCBS: u64 = 1_000_000;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Observation {
    PostExec,
    Timer {
        elapsed: u64,
        rip: u64,
        remaining: u64,
    },
}

#[derive(Default)]
struct Observations(Mutex<Vec<Observation>>);

#[reverie::global_tool]
impl GlobalTool for Observations {
    type Request = Observation;
    type Response = ();
    type Config = bool;

    async fn receive_rpc(&self, _from: Pid, observation: Observation) {
        self.0.lock().unwrap().push(observation);
    }
}

#[derive(Default)]
struct PostExecTimerTool;

#[reverie::tool]
impl Tool for PostExecTimerTool {
    type GlobalState = Observations;
    type ThreadState = Option<u64>;

    fn subscriptions(_arm_timer: &bool) -> Subscription {
        Subscription::none()
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        assert!(!guest.is_command_bootstrap());
        assert_eq!(guest.regs().await.rip, ENTRY);
        guest.send_rpc(Observation::PostExec).await;
        if *guest.config() {
            let origin = guest.read_clock().expect("read post-exec PMU clock");
            assert!(guest.thread_state_mut().replace(origin).is_none());
            guest
                .set_timer_precise(TimerSchedule::Rcbs(TIMER_RCBS))
                .expect("arm a real precise post-exec timer");
        }
        Ok(())
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let origin = guest.thread_state().expect("unrequested timer callback");
        let elapsed = guest
            .read_clock()
            .expect("read delivered timer PMU clock")
            .checked_sub(origin)
            .expect("guest clock moved backwards");
        let registers = guest.regs().await;
        guest
            .send_rpc(Observation::Timer {
                elapsed,
                rip: registers.rip,
                remaining: registers.r15,
            })
            .await;
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        // A static ELF avoids loader branches and needs neither a C compiler
        // nor a runtime DSO. It issues no signals or intervening syscalls.
        // The initial NOP makes an accidental post-exec SINGLESTEP produce
        // a synthetic SIGTRAP before the loop or a PMU deadline can fire.
        let mut code = vec![0x90, 0x41, 0xbf]; // nop; mov r15d, LOOP_BRANCHES
        code.extend_from_slice(&LOOP_BRANCHES.to_le_bytes());
        assert_eq!(ENTRY + code.len() as u64, LOOP_HEAD);
        code.extend_from_slice(&[
            0x49, 0xff, 0xcf, // dec r15
            0x75, 0xfb, // jnz LOOP_HEAD
            0x44, 0x89, 0xff, // mov edi, r15d (zero after the loop)
            0xb8, 60, 0, 0, 0, // mov eax, SYS_exit
            0x0f, 0x05, // syscall
        ]);

        let size = 0x1000 + code.len();
        let mut elf = vec![0_u8; size];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2_u16.to_le_bytes()); // ET_EXEC
        elf[18..20].copy_from_slice(&62_u16.to_le_bytes()); // EM_X86_64
        elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
        elf[24..32].copy_from_slice(&ENTRY.to_le_bytes());
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes()); // program headers
        elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes()); // PT_LOAD
        elf[68..72].copy_from_slice(&5_u32.to_le_bytes()); // readable/executable
        elf[80..88].copy_from_slice(&0x400000_u64.to_le_bytes());
        elf[88..96].copy_from_slice(&0x400000_u64.to_le_bytes());
        elf[96..104].copy_from_slice(&(size as u64).to_le_bytes());
        elf[104..112].copy_from_slice(&(size as u64).to_le_bytes());
        elf[112..120].copy_from_slice(&0x1000_u64.to_le_bytes());
        elf[0x1000..].copy_from_slice(&code);

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-post-exec-timer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&path)
            .expect("create static post-exec fixture");
        file.write_all(&elf)
            .expect("write static post-exec fixture");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn run(arm_timer: bool) -> (Vec<Observation>, PtraceBackendStatsSnapshot) {
    let fixture = Fixture::new();
    let (output, observations, stats) = tokio::time::timeout(
        Duration::from_secs(5),
        PtraceBackend::run_with_output::<PostExecTimerTool>(Command::new(&fixture.0), arm_timer),
    )
    .await
    .expect("plain ptrace post-exec fixture timed out")
    .expect("run plain ptrace post-exec fixture");
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(stats.exec_stops(), 1);
    assert_eq!(stats.tracees_started(), 1);
    assert_eq!(stats.exited_tracees(), 1);
    (observations.0.into_inner().unwrap(), stats)
}

#[tokio::test(flavor = "current_thread")]
async fn plain_ptrace_post_exec_resumes_without_a_synthetic_signal_stop() {
    let (observations, stats) = run(false).await;
    assert_eq!(observations, vec![Observation::PostExec]);
    assert_eq!(
        stats.signal_stops(),
        0,
        "a signal-free guest must not acquire a controller SINGLESTEP stop after post-exec"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a working PMU and supported precise RCB timer profile"]
async fn plain_ptrace_post_exec_timer_fires_at_exact_rcb_deadline() {
    assert!(
        reverie_ptrace::is_perf_supported(),
        "this explicitly selected hardware regression requires PMU access"
    );
    // A large inherited skid-margin override can turn this into an artificial
    // signal plus single stepping. Require the physical notification path.
    assert!(
        TIMER_RCBS > reverie_ptrace::PmuConfig::new().max_single_step_count(),
        "post-exec hardware regression requires a physical PMU notification"
    );
    let (observations, _stats) = run(true).await;
    assert_eq!(
        observations,
        vec![
            Observation::PostExec,
            Observation::Timer {
                elapsed: TIMER_RCBS,
                rip: LOOP_HEAD,
                remaining: u64::from(LOOP_BRANCHES) - TIMER_RCBS,
            },
        ],
        "post-exec must deliver exactly one callback at the requested branch and instruction"
    );
}
