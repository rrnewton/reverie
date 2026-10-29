/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included inside vm::tests, for the timer's late-programming hook.
mod late_first_stage_tests {
    use std::process::Command;

    use reverie::Guest;
    use reverie::syscalls::SyscallInfo;

    use super::*;
    use crate::preemption_timer::FIRST_STAGE_LEAD;
    use crate::preemption_timer::LATE_PROGRAMMING;

    /// The precise request: long enough to be staged with the long first
    /// stage lead.
    const TICKS: u64 = 4 * FIRST_STAGE_LEAD;

    /// How late the first stage's kick comes: half the lead, past the
    /// latest kick measured on this backend's host (257,574 RCBs late), and
    /// far past the first stage margin reverie-ptrace stages with (10,000).
    const LATE: u64 = FIRST_STAGE_LEAD / 2;

    /// RCBs the guest runs past the target, so that an event delivered late
    /// still comes before it exits.
    const TAIL: u64 = 2 * FIRST_STAGE_LEAD;

    /// The Tool's record: the clock at the request and at each timer event.
    #[derive(Default)]
    struct LateLog(Mutex<Vec<(bool, u64)>>);

    #[reverie::global_tool]
    impl GlobalTool for LateLog {
        type Request = (bool, u64);
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, event: (bool, u64)) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// A Tool that requests one precise event of [`TICKS`] RCBs at the
    /// guest's getppid and records where it lands.
    #[derive(Default)]
    struct LateTool;

    #[reverie::tool]
    impl Tool for LateTool {
        type GlobalState = LateLog;
        type ThreadState = ();

        fn subscriptions(_: &()) -> reverie::Subscription {
            reverie::Subscription::all_syscalls()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: reverie::syscalls::Syscall,
        ) -> std::result::Result<i64, reverie::Error> {
            if syscall.number() == reverie::syscalls::Sysno::getppid {
                let clock = guest.read_clock().unwrap();
                guest
                    .set_timer_precise(reverie::TimerSchedule::Rcbs(TICKS))
                    .unwrap();
                guest.send_rpc((false, clock)).await;
            }
            guest.tail_inject(syscall).await
        }

        async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
            let clock = guest.read_clock().unwrap();
            guest.send_rpc((true, clock)).await;
        }
    }

    /// Runs the guest (getppid, then a loop of one RCB per iteration past
    /// the target, then exit) under [`LateTool`], with its first
    /// programming's kick `LATE` RCBs late. Returns the request's clock, the
    /// clocks of the timer events, and the late events recorded.
    fn run(image: &[u8], program: &str, directory: &std::path::Path) -> (u64, Vec<u64>, u64) {
        let _turn = crate::preemption_timer::witness_turn();
        let mut backend = KvmBackend::new(16 * 1024 * 1024).expect("this test requires /dev/kvm");
        backend
            .install_static_elf_with_context(image, &[program], &[], directory)
            .unwrap();
        // The vCPU runs on this thread: its first finite programming, the
        // request's first stage, takes this.
        LATE_PROGRAMMING.with(|late| late.set(LATE));
        let result =
            futures::executor::block_on(backend.run_static_elf_with_tool::<LateTool>((), true));
        let unused = LATE_PROGRAMMING.with(std::cell::Cell::take);
        let witnessed = reverie::take_skid_overshoot_count();
        let (log, status, _, stderr) = result.unwrap();
        assert_eq!(
            (status, unused),
            (0, 0),
            "{}",
            String::from_utf8_lossy(&stderr)
        );
        let events = log.0.into_inner().unwrap();
        let requested = events
            .iter()
            .find_map(|&(timer, clock)| (!timer).then_some(clock))
            .unwrap();
        let timers = events
            .iter()
            .filter_map(|&(timer, clock)| timer.then_some(clock))
            .collect();
        (requested, timers, witnessed)
    }

    /// A first stage kick that comes [`LATE`] RCBs late, as when the host
    /// holds the PMU interrupt behind it, still comes short of the target: the
    /// first stage is programmed [`FIRST_STAGE_LEAD`] short of it, so the
    /// second stage and the single steps take the guest to the target, where
    /// the event lands exactly and nothing is recorded late. A first stage
    /// programmed reverie-ptrace's first stage margin (10,000 RCBs) short of
    /// the target was passed by such a kick, and the event was late. The
    /// second stage's own kick can still be held by the host, rarely: one of
    /// three guests landing exactly passes, and none does with the short
    /// first stage.
    #[test]
    fn a_late_first_stage_kick_still_lands_the_event_exactly() {
        let directory = std::env::temp_dir().join(format!(
            "reverie-timer-late-first-stage-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("guest.S");
        let executable = directory.join("guest");
        std::fs::write(
            &path,
            format!(
                ".global _start\n.text\n_start:\nmov $110,%eax\nsyscall\n\
                 mov ${},%rcx\n1: dec %rcx\njnz 1b\n\
                 xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n",
                TICKS + TAIL
            ),
        )
        .unwrap();
        let output = Command::new("/usr/bin/gcc")
            .args(["-nostdlib", "-static", "-Wl,--build-id=none"])
            .arg(&path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(output.status.success(), "gcc failed: {output:?}");
        let image = std::fs::read(&executable).unwrap();
        let program = executable.to_str().unwrap();
        let mut runs = Vec::new();
        for _ in 0..3 {
            let (requested, timers, witnessed) = run(&image, program, &directory);
            let exact = timers == [requested + TICKS] && witnessed == 0;
            runs.push((requested, timers, witnessed));
            if exact {
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&directory);
        let (requested, timers, _) = runs.last().unwrap();
        assert_eq!(
            (timers.as_slice(), runs.last().unwrap().2),
            ([requested + TICKS].as_slice(), 0),
            "no guest landed exactly with its first stage kick {LATE} RCBs late \
             (request, timer events, late events recorded): {runs:?}"
        );
        println!(
            "late first stage: kick {LATE} RCBs late, lead {FIRST_STAGE_LEAD}; event at \
             {} for target {}, {} guests: {runs:?}",
            timers[0],
            requested + TICKS,
            runs.len()
        );
    }
}
