/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Additive ordinary TRACEEXIT contract. The historical failed test is unchanged.
//! Reuses its actual signal, stopped-generation, callback and cleanup hooks.
//! This is not the separate already-final/ESRCH Startup-owner path.
use super::*;

// A read-only projection of the actual fixture's retained observations.
// It is a predicate, not an owner, a new cleanup receipt or process authority.
struct OrdinaryOwnership<'a> {
    restore_result: Option<&'a str>,
    restore_died: bool,
    resumes: &'a [(OwnerPath, Result<(), String>)],
    callback_owner: Option<OwnerPath>,
    startup_terminal_selected: bool,
}
fn ordinary_ownership_matches(proof: &OrdinaryOwnership<'_>) -> bool {
    proof.restore_result == Some("Ok(())")
        && !proof.restore_died
        && matches!(proof.resumes, [(OwnerPath::Ordinary, Ok(()))])
        && proof.callback_owner == Some(OwnerPath::Ordinary)
        && !proof.startup_terminal_selected
}

#[tokio::test(flavor = "current_thread")]
async fn command_traceexit_before_restore_uses_ordinary_terminal_owner() {
    let fixture = std::path::PathBuf::from(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    assert!(fixture.is_absolute());
    let log = Arc::new(Mutex::new(Log::default()));
    ACTIVE.with(|slot| assert!(slot.replace(Some(Arc::clone(&log))).is_none()));
    let _reset = Reset;
    let mut command = reverie::process::Command::new(fixture);
    command.arg("startup-deaths");
    let tracer = crate::TracerBuilder::<Observer>::new(command)
        .spawn()
        .await
        .unwrap();
    let stop_requested = Arc::clone(&log.lock().unwrap().stop_requested);
    let termination = tracer.termination_handle();
    let completion = tracer.wait();
    tokio::pin!(completion);
    let outcome = tokio::select! {
        result = &mut completion => result,
        () = stop_requested.notified() => {
            let cause = log.lock().unwrap().failures.join("; ");
            let requested = termination.as_ref().is_some_and(|handle| {
                handle.terminate(anyhow::anyhow!("startup fixture retained failure: {cause}").into())
            });
            println!("STARTUP_FAILURE_TERMINATION requested={requested} cause={cause}");
            // Same original consuming wait and its unchanged bounded cleanup.
            completion.await
        }
    };
    println!(
        "STARTUP_ORIGINAL_WAIT result={:?} retained_failures={:?}",
        outcome.as_ref().map(|(status, _)| status),
        log.lock().unwrap().failures
    );
    let cleanups: Vec<_> = log
        .lock()
        .unwrap()
        .children
        .iter()
        .map(|c| Arc::clone(&c.cleanup))
        .collect();
    // Report dimensions even on an error, never manufacture a cleanup receipt.
    for cleanup in &cleanups {
        println!("STARTUP_FINAL_DIMENSIONS {:?}", physical(&log, cleanup));
    }
    let (status, _) = outcome.expect("original Command cleanup must complete");
    assert_eq!(status, reverie::process::ExitStatus::Exited(0));
    // Original one-second notifier bounds, outside the fixture mutex.
    for cleanup in &cleanups {
        assert!(cleanup.wait(Duration::from_secs(1)));
        assert!(physical(&log, cleanup).is_some_and(|p| p.complete()));
    }
    let root = log.lock().unwrap().root_cleanup.take().unwrap();
    assert!(root.wait(Duration::from_secs(1)));
    assert_eq!(
        root.observed_exit_status().unwrap(),
        Some(ExitStatus::Exited(0))
    );
    let log = log.lock().unwrap();
    assert_eq!(log.children.len(), 8);
    assert_eq!(log.created.len(), 8);
    assert_eq!(log.returned.len(), 8);
    assert_eq!(log.restored_parents.len(), 8);
    assert_eq!(log.guest_results.len(), 8);
    assert_eq!(log.joined.len(), 8);
    assert_eq!(log.started.len(), 1, "no child thread-start callback");
    assert_eq!(log.terminal.len(), 9);
    assert_eq!(log.consumed.len(), 9);
    let (tasks, daemons) = log.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    for (child, raw) in log.children.iter().zip(&log.guest_results) {
        assert_eq!(*raw, i64::from(child.tid.as_raw()));
        assert_eq!(log.restored_parents[&child.tid], *raw);
        assert_eq!(
            child.cleanup.observed_exit_status().unwrap(),
            Some(ExitStatus::Signaled(Signal::SIGKILL, false))
        );
        assert_eq!(
            log.terminal[&child.tid],
            ExitStatus::Signaled(Signal::SIGKILL, false)
        );
        assert!(child.callbacks_done.is_some());
        assert!(child.body_done && child.notification_sent);
        assert_eq!(
            child.body_status,
            Some(ExitStatus::Signaled(Signal::SIGKILL, false))
        );
        assert!(log.consumed.contains(&child.tid));
        assert!(!log.started.contains(&child.tid));
    }
    assert!(
        log.failures.is_empty(),
        "retained live-hook failures: {:?}",
        log.failures
    );
    println!(
        "STARTUP_PHYSICAL_CLEANUP_DIAGNOSTIC children=8 parent_returns=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0"
    );
    // All actual final/status/callback/body and original wait checks above are
    // the historical cleanup prefix. Only now inspect this separate contract.
    assert!(
        log.joined.iter().all(|p| !p.failed),
        "successful ordinary restoration must not cause full-history failure"
    );
    for (index, child) in log.children.iter().enumerate() {
        let proof = OrdinaryOwnership {
            restore_result: child.restore_result.as_deref(),
            restore_died: child.restore_died,
            resumes: &child.resumes,
            callback_owner: child.callbacks_done.map(|(owner, _, _)| owner),
            startup_terminal_selected: child.terminal_branch.is_some(),
        };
        assert!(
            ordinary_ownership_matches(&proof),
            "real readable TRACEEXIT must use one Ordinary EXIT owner and Ordinary callbacks"
        );
        assert_eq!(
            child.callbacks_done,
            Some((OwnerPath::Ordinary, 1, 0)),
            "only the original parent remains when each child callback completes"
        );
        let member = child
            .member
            .as_ref()
            .expect("every real child was enrolled");
        assert_eq!(member.index, (index + 1) as u64);
        assert!(!child.before.failed);
        assert_eq!(child.before.tasks, 2);
        assert_eq!(child.before.next_task, (index + 2) as u64);
        let joined = log.joined[index];
        assert!(!joined.failed);
        assert_eq!(joined.tasks, 1, "each original child metadata retired");
        assert_eq!(joined.operations, 0, "no completed native debt retained");
        assert_eq!(joined.next_task, (index + 2) as u64);
    }
    let final_population = population(log.history.as_ref().unwrap());
    assert!(!final_population.failed);
    assert_eq!(final_population.tasks, 0);
    assert_eq!(final_population.operations, 0);
    assert_eq!(final_population.next_task, 9);
    let history = log.history.as_ref().unwrap().0.lock().unwrap();
    assert!(
        history.source_closed && !history.read_open(),
        "ordinary process-child terminal policy remains source-closed"
    );
    // This is bounded lineage/physical cleanup, NOT re-admitted source reads.
    println!(
        "TRACEEXIT_ORDINARY_CONTRACT children=8 parent_returns=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0 ordinary_resumes=8 startup_resumes=0 final={final_population:?} source_closed=true"
    );
}

#[test]
fn ordinary_owner_reader_requires_exact_owner_and_multiplicity() {
    let ordinary = [(OwnerPath::Ordinary, Ok(()))];
    let mut proof = OrdinaryOwnership {
        restore_result: Some("Ok(())"),
        restore_died: false,
        resumes: &ordinary,
        callback_owner: Some(OwnerPath::Ordinary),
        startup_terminal_selected: false,
    };
    assert!(ordinary_ownership_matches(&proof));
    let startup = [(OwnerPath::Startup, Ok(()))];
    proof.resumes = &startup;
    assert!(
        !ordinary_ownership_matches(&proof),
        "wrong actual EXIT owner"
    );
    proof.resumes = &ordinary;
    proof.callback_owner = Some(OwnerPath::Startup);
    assert!(!ordinary_ownership_matches(&proof), "wrong callback owner");
    proof.callback_owner = None;
    assert!(
        !ordinary_ownership_matches(&proof),
        "missing callback owner"
    );
    proof.callback_owner = Some(OwnerPath::Ordinary);
    let duplicate = [(OwnerPath::Ordinary, Ok(())), (OwnerPath::Ordinary, Ok(()))];
    proof.resumes = &duplicate;
    assert!(!ordinary_ownership_matches(&proof), "duplicate EXIT resume");
    proof.resumes = &[];
    assert!(!ordinary_ownership_matches(&proof), "missing EXIT resume");
    let refused = [(OwnerPath::Ordinary, Err("actual refusal".to_owned()))];
    proof.resumes = &refused;
    assert!(!ordinary_ownership_matches(&proof), "failed EXIT resume");
    proof.resumes = &ordinary;
    assert!(ordinary_ownership_matches(&proof));
}

#[test]
fn ordinary_owner_reader_rejects_death_or_startup_selection() {
    let ordinary = [(OwnerPath::Ordinary, Ok(()))];
    let mut proof = OrdinaryOwnership {
        restore_result: Some("Ok(())"),
        restore_died: false,
        resumes: &ordinary,
        callback_owner: Some(OwnerPath::Ordinary),
        startup_terminal_selected: false,
    };
    assert!(ordinary_ownership_matches(&proof));
    proof.restore_result = None;
    assert!(
        !ordinary_ownership_matches(&proof),
        "missing actual restore result"
    );
    proof.restore_result = Some("Err(Died)");
    assert!(
        !ordinary_ownership_matches(&proof),
        "actual restore was not successful"
    );
    proof.restore_result = Some("Ok(())");
    proof.restore_died = true;
    assert!(
        !ordinary_ownership_matches(&proof),
        "contradictory death observation"
    );
    proof.restore_died = false;
    proof.startup_terminal_selected = true;
    assert!(
        !ordinary_ownership_matches(&proof),
        "actual Startup path is distinct"
    );
    proof.startup_terminal_selected = false;
    assert!(ordinary_ownership_matches(&proof));
}
