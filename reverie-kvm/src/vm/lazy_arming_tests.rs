/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included inside vm::tests, for the timer's syscall count.
mod lazy_arming_tests {
    use std::process::Command;

    use reverie::Guest;
    use reverie::syscalls::SyscallInfo;

    use super::*;
    use crate::preemption_timer::MAX_ARMING_SLACK;
    use crate::preemption_timer::TIMER_SYSCALLS;

    /// The guest's syscalls, each of which renews the request.
    const RENEWALS: u64 = 20_000;

    /// How much earlier each renewal moves the target: a Tool that charges
    /// each syscall 1,000 RCBs of a thread's budget of branches, and renews
    /// its request for the rest of the budget after each syscall.
    const MOVE: u64 = 1_000;

    /// The first request's distance: far enough that no renewal brings the
    /// target near the clock, so that no timer event comes due.
    const BUDGET: u64 = 1 << 32;

    /// The timer syscalls of opening the timer event at the first armed
    /// entry, once this process has installed the kick handler (see
    /// `clock::preemption_timer_tests::OPEN_SYSCALLS`).
    const OPEN_SYSCALLS: u64 = 9;

    /// The Tool's record: timer events delivered, and the renewals it made.
    #[derive(Default)]
    struct RenewalLog(Mutex<(u64, u64)>);

    #[reverie::global_tool]
    impl GlobalTool for RenewalLog {
        type Request = bool;
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, event: bool) {
            let mut log = self.0.lock().unwrap();
            if event {
                log.0 += 1;
            } else {
                log.1 += 1;
            }
        }
    }

    /// A Tool that renews one precise request at every getppid, each time
    /// [`MOVE`] RCBs earlier, as a Tool that charges each syscall against a
    /// thread's budget renews it after each syscall. Its thread state is the
    /// target.
    #[derive(Default)]
    struct RenewingTool;

    #[reverie::tool]
    impl Tool for RenewingTool {
        type GlobalState = RenewalLog;
        type ThreadState = u64;

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
                let target = match *guest.thread_state() {
                    0 => clock + BUDGET,
                    previous => previous - MOVE,
                };
                *guest.thread_state_mut() = target;
                guest
                    .set_timer_precise(reverie::TimerSchedule::Rcbs(target - clock))
                    .unwrap();
                guest.send_rpc(false).await;
            }
            guest.tail_inject(syscall).await
        }

        async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
            guest.send_rpc(true).await;
        }
    }

    /// A precise request that a Tool renews at every syscall, each time
    /// [`MOVE`] RCBs earlier, costs the timer one reprogramming per
    /// `MAX_ARMING_SLACK / MOVE` renewals, not one per renewal: each
    /// reprogramming leaves room before the point the request needs, so the
    /// point moving earlier keeps it. Without that room every renewal moved
    /// the point before the pending overflow, and every entry reprogrammed
    /// the event: a host syscall per guest syscall.
    #[test]
    fn renewing_a_request_at_every_syscall_seldom_reprograms_the_timer() {
        let directory = std::env::temp_dir().join(format!(
            "reverie-timer-lazy-arming-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("guest.S");
        let executable = directory.join("guest");
        std::fs::write(
            &path,
            format!(
                ".global _start\n.text\n_start:\nmov ${RENEWALS},%r12\n\
                 1: mov $110,%eax\nsyscall\ndec %r12\njnz 1b\n\
                 xor %edi,%edi\nmov $231,%eax\nsyscall\nud2\n"
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
        // The count is the vCPU thread's, which is this one.
        crate::preemption_timer::reserve_kick_signal().unwrap();
        let before = TIMER_SYSCALLS.with(|count| count.get());
        let mut backend = KvmBackend::new(16 * 1024 * 1024).expect("this test requires /dev/kvm");
        let program = executable.to_str().unwrap();
        backend
            .install_static_elf_with_context(&image, &[program], &[], &directory)
            .unwrap();
        let result =
            futures::executor::block_on(backend.run_static_elf_with_tool::<RenewingTool>((), true));
        let syscalls = TIMER_SYSCALLS.with(|count| count.get()) - before;
        let _ = std::fs::remove_dir_all(&directory);
        let (log, status, _, stderr) = result.unwrap();
        let (events, renewals) = log.0.into_inner().unwrap();
        assert_eq!(
            (status, events, renewals),
            (0, 0, RENEWALS),
            "{}",
            String::from_utf8_lossy(&stderr)
        );
        // The first renewal moves the point before the first programming,
        // and each later reprogramming follows a move of the slack.
        let reprogrammings = RENEWALS * MOVE / MAX_ARMING_SLACK + 1;
        assert!(
            (OPEN_SYSCALLS..=OPEN_SYSCALLS + reprogrammings + 1).contains(&syscalls),
            "{syscalls} timer syscalls for {RENEWALS} renewals, each {MOVE} RCBs earlier: \
             expected at most {} ({OPEN_SYSCALLS} to open the timer, {reprogrammings} \
             reprogrammings and one to quiet it)",
            OPEN_SYSCALLS + reprogrammings + 1
        );
        println!(
            "lazy arming: {RENEWALS} renewals, each {MOVE} RCBs earlier, cost {syscalls} timer \
             syscalls (slack {MAX_ARMING_SLACK} RCBs)"
        );
    }
}
