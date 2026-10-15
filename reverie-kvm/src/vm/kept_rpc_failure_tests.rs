/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included inside vm::tests so the existing minimal ELF fixture remains private.
mod kept_rpc_failure_tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicU64;

    use super::*;

    /// What the GlobalTool saw. One per run, keyed by the run's mode.
    #[derive(Default)]
    struct Counts {
        /// GlobalTool::wait_for_backend_failure calls: one per failure wait
        /// the backend built.
        waits: AtomicU64,
        /// RPCs of each kind that reached receive_rpc.
        first: AtomicU64,
        second: AtomicU64,
        /// Set by a FAIL_DURING_FIRST run's first RPC: the backend failure the
        /// GlobalTool's waits report from then on.
        failed: AtomicBool,
    }

    static ACTIVE: Mutex<Option<Arc<Counts>>> = Mutex::new(None);

    /// Modes: RPCs per getpid, or the failure-precedence run.
    const ONE_RPC: u8 = 1;
    const FOUR_RPCS: u8 = 4;
    const FAIL_DURING_FIRST: u8 = 0;

    #[derive(Default)]
    struct RpcGlobal {
        counts: Option<Arc<Counts>>,
        mode: u8,
    }

    #[reverie::global_tool]
    impl GlobalTool for RpcGlobal {
        type Request = u8;
        type Response = ();
        type Config = u8;

        async fn init_global_state(mode: &u8) -> Self {
            Self {
                counts: Some(ACTIVE.lock().unwrap().as_ref().unwrap().clone()),
                mode: *mode,
            }
        }

        async fn receive_rpc(&self, _from: Pid, kind: u8) {
            let counts = self.counts.as_ref().unwrap();
            match kind {
                1 => {
                    counts.first.fetch_add(1, Ordering::SeqCst);
                    if self.mode == FAIL_DURING_FIRST {
                        counts.failed.store(true, Ordering::SeqCst);
                    }
                }
                2 => {
                    counts.second.fetch_add(1, Ordering::SeqCst);
                }
                _ => panic!("unexpected RPC {kind}"),
            }
        }

        async fn wait_for_backend_failure(&self) {
            let counts = self.counts.as_ref().unwrap().clone();
            counts.waits.fetch_add(1, Ordering::SeqCst);
            std::future::poll_fn(move |_| {
                if counts.failed.load(Ordering::SeqCst) {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await
        }
    }

    #[derive(Default)]
    struct RpcTool {
        mode: u8,
    }

    #[reverie::tool]
    impl Tool for RpcTool {
        type GlobalState = RpcGlobal;
        type ThreadState = ();

        fn new(_: Pid, mode: &u8) -> Self {
            Self { mode: *mode }
        }

        fn subscriptions(_: &u8) -> reverie::Subscription {
            let mut subscription = reverie::Subscription::none();
            subscription.syscall(reverie::syscalls::Sysno::getpid);
            subscription
        }

        async fn handle_syscall_event<G: reverie::Guest<Self>>(
            &self,
            guest: &mut G,
            call: reverie::syscalls::Syscall,
        ) -> std::result::Result<i64, reverie::Error> {
            if self.mode == FAIL_DURING_FIRST {
                // The first RPC publishes the failure; the second must be
                // stopped before it reaches the GlobalTool.
                guest.send_rpc(1).await;
                guest.send_rpc(2).await;
            } else {
                for _ in 0..self.mode {
                    guest.send_rpc(1).await;
                }
            }
            guest.tail_inject(call).await
        }
    }

    // mov eax, SYS_getpid; syscall (three times); mov eax, SYS_exit; xor edi,
    // edi; syscall.
    fn three_getpids_then_exit() -> Vec<u8> {
        let getpid = [0xb8, 39, 0, 0, 0, 0x0f, 0x05];
        let exit = [0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05];
        [&getpid[..], &getpid, &getpid, &exit].concat()
    }

    fn run(mode: u8) -> (Arc<Counts>, Result<i32>) {
        let counts = Arc::new(Counts::default());
        {
            let mut active = ACTIVE.lock().unwrap();
            assert!(active.is_none());
            *active = Some(counts.clone());
        }
        let mut backend =
            KvmBackend::new(16 * 1024 * 1024).expect("kept RPC failure test requires /dev/kvm");
        backend
            .install_static_elf(&minimal_test_elf(&three_getpids_then_exit()), "/bin/kept-rpc")
            .unwrap();
        let result = futures::executor::block_on(
            backend.run_static_elf_with_tool::<RpcTool>(mode, true),
        )
        .map(|(_, status, _, _)| status);
        ACTIVE.lock().unwrap().take();
        drop(backend);
        (counts, result)
    }

    /// The ordinary syscall callback's RPCs share one kept failure wait per
    /// thread, so the GlobalTool builds as many failure waits when every
    /// getpid sends four RPCs as when it sends one. Before, each RPC built
    /// its own wait.
    #[test]
    fn rpcs_share_the_threads_kept_failure_wait() {
        let (one, status) = run(ONE_RPC);
        assert_eq!(status.unwrap(), 0);
        assert_eq!(one.first.load(Ordering::SeqCst), 3);
        let (four, status) = run(FOUR_RPCS);
        assert_eq!(status.unwrap(), 0);
        assert_eq!(four.first.load(Ordering::SeqCst), 12);
        assert_eq!(
            four.waits.load(Ordering::SeqCst),
            one.waits.load(Ordering::SeqCst),
            "nine more RPCs built no more failure waits"
        );
    }

    /// A failure published during one RPC stops the next RPC of the same
    /// callback before it reaches the GlobalTool, as when each RPC built its
    /// own wait: the kept wait is polled first and reports the failure.
    #[test]
    fn a_failure_published_during_an_rpc_stops_the_next_one() {
        let (counts, result) = run(FAIL_DURING_FIRST);
        assert!(result.is_err(), "the run must end in the published failure");
        assert_eq!(counts.first.load(Ordering::SeqCst), 1);
        assert_eq!(
            counts.second.load(Ordering::SeqCst),
            0,
            "the RPC after the failure was dispatched"
        );
    }
}
