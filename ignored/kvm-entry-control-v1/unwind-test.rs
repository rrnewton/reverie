    #[test]
    fn unwinding_control_operations_preserve_cleanup_and_failure() {
        use std::panic::AssertUnwindSafe;
        use std::panic::catch_unwind;

        const CHILD: &str = "REVERIE_KVM_ENTRY_UNWIND_CHILD";
        let Ok(case) = std::env::var(CHILD) else {
            let mut failures = Vec::new();
            for case in ["mask", "send"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "entry::tests::unwinding_control_operations_preserve_cleanup_and_failure",
                        "--nocapture",
                    ])
                    .env(CHILD, case)
                    .output()
                    .unwrap();
                if !output.status.success() {
                    failures.push(format!(
                        "{case}: status={}\n{}{}",
                        output.status,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
            return;
        };

        fn mask_bits() -> u64 {
            // SAFETY: the query writes a live sigset and does not change it.
            unsafe {
                let mut mask: libc::sigset_t = std::mem::zeroed();
                assert_eq!(
                    libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask),
                    0
                );
                (1..=64).fold(0, |bits, signal| {
                    bits | ((libc::sigismember(&mask, signal) as u64) << (signal - 1))
                })
            }
        }

        if case == "send" {
            // Leave signal 64 blocked in the original mask, so a missed drain
            // cannot disappear through the handler when that mask is restored.
            unsafe {
                let mut reserved: libc::sigset_t = std::mem::zeroed();
                assert_eq!(libc::sigemptyset(&mut reserved), 0);
                assert_eq!(libc::sigaddset(&mut reserved, 64), 0);
                assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &reserved, std::ptr::null_mut()), 0);
            }
        }
        let original_mask = mask_bits();
        let global = Arc::new(());
        let run = crate::failure::RunFailure::new(&global);
        let origin = FailureContext::new(
            run.clone(),
            reverie::Pid::from_raw(37),
            reverie::Pid::from_raw(39),
        );
        let gate = EntryGate::new();
        let mut participant = gate.register().unwrap();
        participant.set_origin(Some(origin));
        let id = participant.id;
        let changed = gate.subscribe();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let entry = participant.try_enter().unwrap().unwrap();
            let mask = signal::Mask::block().unwrap();
            let mut guard = RunEntry {
                entry: Some(entry),
                mask: Some(mask),
            };
            match case.as_str() {
                "mask" => {
                    let _ = guard.entry.as_mut().unwrap().activate(|| {
                        panic!("mask operation unwound");
                    });
                }
                "send" => {
                    assert!(guard.entry.as_mut().unwrap().activate(|| Ok(())).unwrap());
                    let _ = gate.try_close_with(|thread| {
                        // Queue a real control signal before the injected unwind.
                        // Its drain must finish before the original mask returns.
                        assert_eq!(unsafe { libc::pthread_kill(thread, 64) }, 0);
                        panic!("send operation unwound");
                    });
                }
                _ => panic!("unknown unwind control"),
            }
            panic!("injected operation did not unwind");
        }));
        let payload = outcome.expect_err("operation panic was swallowed");
        let expected = if case == "mask" {
            "mask operation unwound"
        } else {
            "send operation unwound"
        };
        assert_eq!(payload.downcast_ref::<&str>().copied(), Some(expected));
        assert_eq!(mask_bits(), original_mask, "unwind changed the host mask");
        assert!(!gate.state.is_poisoned(), "cleanup would panic on registry lock");
        assert!(changed.now_or_never().is_some(), "unwind lost the failure wake");
        let pending = gate.pending_failure().unwrap();
        let operation = if case == "mask" {
            assert!(Arc::ptr_eq(&pending.origin.as_ref().unwrap().run, &run));
            "install KVM temporary signal mask unwound"
        } else {
            "interrupt running vCPU unwound"
        };
        assert!(matches!(pending.error().primary(), Error::EntryControl { operation: actual, .. } if *actual == operation));
        let Error::WithCleanup { cleanup, .. } = pending.error() else {
            panic!("unwind cleanup cause was lost");
        };
        assert!(cleanup.iter().any(|error| matches!(error.primary(), Error::EntryControl { operation: "guest entry unwound or was abandoned", .. })));
        assert!(run.primary().is_none(), "capture published a Tool callback");
        assert_eq!(gate.state.lock().unwrap().members[&id].activity, Activity::Stopped);
        assert!(gate.admit_operation().is_err());
        assert!(participant.try_enter().is_err());
        assert!(gate.try_copy(None).is_err());
        assert!(gate.try_close().is_err());
        // A second mask guard refuses a stale queued control signal.
        signal::Mask::block().unwrap().finish().unwrap();
        assert_eq!(mask_bits(), original_mask);
        drop(participant);
        assert!(gate.state.lock().unwrap().members.is_empty());
    }

