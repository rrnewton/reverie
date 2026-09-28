from pathlib import Path
import difflib,hashlib,json,os
P=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/sigchld-first-guard-proposal-20260917/cli-part')
paths=['hermit-cli/src/lib.rs','hermit-cli/src/error.rs','hermit-cli/src/kvm_failure_tests.rs']
texts={p:(P/'base'/p).read_text() for p in paths}
old=texts[paths[0]]
start=old.index('// A runtime error still owns GlobalState.')
end=old.index('\n#[cfg(test)]\nmod kvm_failure_tests;',start)
new='''// GlobalState can retain a scheduler-originated refusal independently of the
// backend's return value. Join before inspecting it: the scheduler can publish
// its final refusal while the caller is waiting for natural completion.
async fn finish_kvm_tool_completion(
    mut completion: reverie_kvm::ToolRunCompletion<detcore::GlobalState>,
    print_summary: bool,
    print_summary_to_json_file: &Option<PathBuf>,
) -> Result<(i32, Vec<u8>, Vec<u8>), Error> {
    let scheduler = completion.global_state.join_internal_scheduler().await;
    match completion.result {
        Ok(output)
            if scheduler.is_ok() && completion.global_state.signal_refusal().is_none() =>
        {
            completion
                .global_state
                .clean_up(print_summary, print_summary_to_json_file)
                .await;
            Ok(output)
        }
        result => {
            let mut cleanup = completion
                .global_state
                .clean_up_after_backend_failure()
                .await;
            // The owned handle was consumed above. Retain its actual outcome;
            // the second natural-join call cannot replace it with Ok(()).
            cleanup.scheduler = scheduler;
            Err(kvm_completion_error(result.err(), Some(cleanup)))
        }
    }
}

fn kvm_execution_error(
    primary: reverie_kvm::Error,
    cleanup: Option<detcore::BackendFailureCleanup>,
) -> Error {
    kvm_completion_error(Some(primary), cleanup)
}

// A policy refusal does not excuse an independent backend or cleanup failure.
// Keep that distinction typed until SerializableError records the boundary kind.
#[derive(Debug)]
struct KvmIndependentSignalFailure;

impl std::fmt::Display for KvmIndependentSignalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KVM signal refusal accompanied by an independent execution or cleanup failure")
    }
}

impl std::error::Error for KvmIndependentSignalFailure {}

fn kvm_completion_error(
    backend: Option<reverie_kvm::Error>,
    cleanup: Option<detcore::BackendFailureCleanup>,
) -> Error {
    let (mut scheduler, recording, mut refusal) = match cleanup {
        Some(cleanup) => (
            cleanup.scheduler.err(),
            cleanup.preemption_recording.err(),
            cleanup.signal_refusal,
        ),
        None => (None, None, None),
    };
    let independent_failure = refusal.is_some()
        && (scheduler.is_some()
            || recording.is_some()
            || backend
                .as_ref()
                .is_some_and(|error| !matches!(error, reverie_kvm::Error::RunAborted)));
    // The manifest runner retains the first Error line. Show the actual cause
    // there, including a separately retained refusal when the backend failed.
    let primary = backend
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| refusal.as_ref().map(ToString::to_string))
        .or_else(|| scheduler.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| "completion failed without a retained cause".to_owned());
    let mut message = format!("KVM guest execution failed: {primary}");
    if backend.is_some()
        && let Some(refusal) = &refusal
    {
        message.push_str(&format!("; {refusal}"));
    }
    let mut error = match backend {
        Some(backend) => Error::new(backend),
        None => match refusal.take() {
            Some(refusal) => Error::new(refusal),
            None => match scheduler.take() {
                Some(scheduler) => Error::new(scheduler),
                // This is an internal invariant failure, not a fabricated
                // backend failure or a policy refusal.
                None => Error::msg("KVM completion failed without a retained cause"),
            },
        },
    };
    if let Some(refusal) = refusal {
        error = error.context(refusal);
    }
    if let Some(recording) = recording {
        error = error.context(format!("partial preemption recording failed: {recording}"));
    }
    if let Some(scheduler) = scheduler {
        error = error.context(scheduler);
    }
    if independent_failure {
        error = error.context(KvmIndependentSignalFailure);
    }
    error.context(message)
}
'''
old=old[:start]+new+old[end:]
needle='    config.backend_requires_thread_directed_process_signals = backend == Backend::Dbt;'
assert old.count(needle)==1
old=old.replace(needle,needle+'\n    // KVM task IDs belong to its virtual process tree, never the host kernel.\n    config.backend_rejects_host_signals = backend == Backend::Kvm;')
needle='    #[test]\n    fn backend_write_signal_contract_is_explicit() {'
assert old.count(needle)==1
old=old.replace(needle,'''    #[test]
    fn backend_host_signal_refusal_contract_is_normalized() {
        assert!(!super::DetConfig::default().backend_rejects_host_signals);
        for backend in [
            Backend::Ptrace,
            Backend::Dbt,
            Backend::Kvm,
            Backend::Sabre,
            Backend::Liteinst,
            Backend::E9patch,
        ] {
            for stale_value in [false, true] {
                let config = prepare_backend_config(
                    super::DetConfig {
                        backend_rejects_host_signals: stale_value,
                        ..super::DetConfig::default()
                    },
                    backend,
                );
                assert_eq!(config.backend_rejects_host_signals, backend == Backend::Kvm);
                assert_eq!(
                    config.backend_requires_thread_directed_process_signals,
                    backend == Backend::Dbt,
                );
            }
        }
    }

'''+needle)
texts[paths[0]]=old
old=texts[paths[1]]
needle='fn is_policy_refusal(err: &Error) -> bool {\n'
assert old.count(needle)==1
old=old.replace(needle,needle+'''    if err
        .downcast_ref::<crate::KvmIndependentSignalFailure>()
        .is_some()
    {
        return false;
    }
    // Anyhow preserves typed context values for downcast_ref even when the
    // std::error::Error source chain does not expose the context value itself.
    if err.downcast_ref::<detcore::KvmSignalRefusal>().is_some() {
        return true;
    }
''')
needle='''                reverie::Error::Tool(inner) => {
                    inner.downcast_ref::<detcore::UnsupportedSyscallError>()
                }
                _ => None,
            })
            .is_some()'''
assert old.count(needle)==1
old=old.replace(needle,'''                reverie::Error::Tool(inner) => Some(inner),
                _ => None,
            })
            .is_some_and(is_policy_refusal)''')
needle='''        .any(|cause| cause.is::<detcore::UnsupportedSyscallError>())'''
assert old.count(needle)==1
old=old.replace(needle,'''        .any(|cause| {
            cause.is::<detcore::UnsupportedSyscallError>()
                || cause.is::<detcore::KvmSignalRefusal>()
        })''')
needle='    #[test]\n    fn skid_overshoot_is_serialized_as_a_policy_refusal() {'
assert old.count(needle)==1
old=old.replace(needle,'''    #[test]
    fn kvm_signal_refusal_is_classified_through_typed_and_transparent_wrappers() {
        let refusal = detcore::KvmSignalRefusal {
            signal: libc::SIGCHLD,
            target: detcore::types::DetTid::from_raw(17),
            turn: 23,
        };
        for shape in 0..5 {
            let error = match shape {
                0 => Error::new(refusal.clone()),
                1 => Error::new(refusal.clone()).context("caller context"),
                2 => Error::new(reverie::Error::Tool(Error::new(refusal.clone()))),
                3 => Error::new(reverie_kvm::Error::Reverie(reverie::Error::Tool(
                    Error::new(refusal.clone()),
                ))),
                4 => Error::new(reverie_kvm::Error::RunAborted).context(refusal.clone()),
                _ => unreachable!(),
            };
            let error = SerializableError::from(error);
            assert_eq!(error.kind(), FailureKind::PolicyRefusal);
            let wire = serde_json::to_vec(&error).unwrap();
            assert_eq!(serde_json::from_slice::<SerializableError>(&wire).unwrap(), error);
        }
        // Identical prose without the typed refusal remains an ordinary error.
        let same_text = SerializableError::from(Error::msg(refusal.to_string()));
        assert_eq!(same_text.kind(), FailureKind::Error);
    }

'''+needle)
texts[paths[1]]=old
old=texts[paths[2]]
needle='''            preemption_recording: Err("recording control".to_owned()),'''
assert old.count(needle)==1
old=old.replace(needle,needle+'\n            signal_refusal: None,')
needle='#[cfg(feature = "kvm-native-test-support")]\nmod combined {'
assert old.count(needle)==1
old=old.replace(needle,'''#[tokio::test]
async fn kvm_signal_completion_retains_refusal_backend_and_independent_cleanup() {
    use crate::error::FailureKind;
    use crate::error::SerializableError;

    // The public consuming cleanup value is the exact boundary used after the
    // scheduler has joined. Actual RPC refusal publication is tested in Detcore.
    for (backend_shape, scheduler_failed, recording_failed, expected_kind) in [
        (0, false, false, FailureKind::PolicyRefusal),
        (1, false, false, FailureKind::PolicyRefusal),
        (2, false, false, FailureKind::Error),
        (3, false, false, FailureKind::Error),
        (0, true, false, FailureKind::Error),
        (0, false, true, FailureKind::Error),
        (1, true, true, FailureKind::Error),
    ] {
        let refusal = detcore::KvmSignalRefusal {
            signal: libc::SIGCHLD,
            target: detcore::types::DetTid::from_raw(17),
            turn: 23,
        };
        let secondary = std::sync::Arc::new(reverie_kvm::Error::HostIo(
            std::io::Error::from_raw_os_error(libc::EIO),
        ));
        let backend = match backend_shape {
            0 => None,
            1 => Some(reverie_kvm::Error::RunAborted),
            2 => Some(reverie_kvm::Error::InvalidGuestPid(-17)),
            3 => Some(reverie_kvm::Error::WithCleanup {
                primary: std::sync::Arc::new(reverie_kvm::Error::RunAborted),
                cleanup: vec![secondary.clone()],
            }),
            _ => unreachable!(),
        };
        let scheduler = if scheduler_failed {
            tokio::spawn(async { panic!("independent scheduler failure") }).await
        } else {
            Ok(())
        };
        let error = kvm_completion_error(
            backend,
            Some(detcore::BackendFailureCleanup {
                scheduler,
                preemption_recording: if recording_failed {
                    Err("independent recording failure".to_owned())
                } else {
                    Ok(())
                },
                signal_refusal: Some(refusal.clone()),
            }),
        );
        assert_eq!(error.downcast_ref::<detcore::KvmSignalRefusal>(), Some(&refusal));
        assert!(error.to_string().contains(&refusal.to_string()));
        let backend = error.downcast_ref::<reverie_kvm::Error>();
        match backend_shape {
            0 => assert!(backend.is_none(), "no backend error was returned"),
            1 => assert!(matches!(backend, Some(reverie_kvm::Error::RunAborted))),
            2 => assert!(matches!(backend, Some(reverie_kvm::Error::InvalidGuestPid(-17)))),
            3 => {
                let Some(reverie_kvm::Error::WithCleanup { primary, cleanup }) = backend else {
                    panic!("the real aggregated cleanup error was lost");
                };
                assert!(matches!(primary.as_ref(), reverie_kvm::Error::RunAborted));
                assert_eq!(cleanup.len(), 1);
                assert!(std::sync::Arc::ptr_eq(&cleanup[0], &secondary));
            }
            _ => unreachable!(),
        }
        assert_eq!(
            error.downcast_ref::<tokio::task::JoinError>().is_some(),
            scheduler_failed,
        );
        assert_eq!(
            format!("{error:#}").contains("partial preemption recording failed: independent recording failure"),
            recording_failed,
        );
        assert_eq!(SerializableError::from(error).kind(), expected_kind);
    }
}

#[tokio::test]
async fn kvm_successful_backend_with_failed_scheduler_retains_real_join_error() {
    let scheduler = tokio::spawn(async { panic!("scheduler failed after backend success") })
        .await
        .expect_err("the control must supply a real JoinError");
    let error = kvm_completion_error(
        None,
        Some(detcore::BackendFailureCleanup {
            scheduler: Err(scheduler),
            preemption_recording: Ok(()),
            signal_refusal: None,
        }),
    );
    assert!(error.downcast_ref::<reverie_kvm::Error>().is_none());
    assert!(error.downcast_ref::<detcore::KvmSignalRefusal>().is_none());
    assert!(error.downcast_ref::<tokio::task::JoinError>().unwrap().is_panic());
    assert_eq!(
        crate::error::SerializableError::from(error).kind(),
        crate::error::FailureKind::Error,
    );
}

'''+needle)
texts[paths[2]]=old
records=[];patch=[]
for name in paths:
 base=(P/'base'/name).read_bytes();new=texts[name].encode();dest=P/'changed'/name;dest.parent.mkdir(parents=True,exist_ok=True)
 with dest.open('xb') as f:f.write(new)
 os.chmod(dest,0o644)
 records.append(dict(path=name,base_bytes=len(base),base_sha256=hashlib.sha256(base).hexdigest(),mode='100644',bytes=len(new),sha256=hashlib.sha256(new).hexdigest()))
 patch.append('diff --git a/'+name+' b/'+name+'\n'+''.join(difflib.unified_diff(base.decode().splitlines(True),new.decode().splitlines(True),fromfile='a/'+name,tofile='b/'+name)))
(P/'candidate.patch').write_text(''.join(patch))
(P/'FILES.json').write_text(json.dumps(records,indent=2)+'\n')
print(json.dumps(records,indent=2))
