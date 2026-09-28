from pathlib import Path
import shutil
D=Path(__file__).resolve().parent
files=['detcore-model/src/config.rs','detcore/src/lib.rs','detcore/src/scheduler.rs','detcore/src/tool_global.rs']
for f in files:
 p=D/'changed'/f; p.parent.mkdir(parents=True,exist_ok=True); shutil.copy2(D/'base'/f,p)
def edit(f,old,new):
 p=D/'changed'/f; s=p.read_text(); assert s.count(old)==1,(f,s.count(old),old[:80]);p.write_text(s.replace(old,new))
f=files[0]
edit(f,'    /// The backend can wake a scheduler-managed pipe write for a cross-task signal while', '''    /// The KVM backend cannot deliver scheduler signals to virtual task identities yet.
    /// Refuse the run before any host signal syscall, including diagnostic stacktrace signals.
    /// The CLI normalizes this internal setting from its selected backend.
    #[serde(default)]
    #[clap(skip)]
    pub backend_rejects_host_signals: bool,

    /// The backend can wake a scheduler-managed pipe write for a cross-task signal while''')
edit(f,'        assert!(!config.backend_requires_thread_directed_process_signals);','        assert!(!config.backend_requires_thread_directed_process_signals);\n        assert!(!config.backend_rejects_host_signals);')
edit(f,'    #[test]\n    fn missing_mountinfo_provenance_deserializes_as_empty()', '''    #[test]
    fn missing_backend_signal_refusal_setting_defaults_to_false() {
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("backend_rejects_host_signals");
        let config: Config = serde_json::from_value(value).unwrap();
        assert!(!config.backend_rejects_host_signals);
    }

    #[test]
    fn missing_mountinfo_provenance_deserializes_as_empty()''')
f=files[1]
edit(f,'// AUTONOMOUS-BOT-IMPLEMENTED\n// TODO-HUMAN-REVIEW(PR-644): Review the typed fail-closed backend signal.', '''/// A KVM signal request stopped before a virtual task identity reached a host signal syscall.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KvmSignalRefusal {
    /// The complete Linux signal number, including realtime signals.
    pub signal: i32,
    /// The virtual task selected by the scheduler or stacktrace request.
    pub target: DetTid,
    /// The scheduler turn at which the refusal became terminal.
    pub turn: u64,
}

impl std::fmt::Display for KvmSignalRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "KVM cannot deliver signal {} to virtual thread {} at scheduler turn {}",
            self.signal, self.target, self.turn,
        )
    }
}

impl std::error::Error for KvmSignalRefusal {}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-644): Review the typed fail-closed backend signal.''')
f=files[2]
edit(f,'\nuse crate::config::Config;','\nuse crate::KvmSignalRefusal;\nuse crate::config::Config;')
edit(f,'    backend_failure: Option<reverie::BackendFailure>,','    backend_failure: Option<reverie::BackendFailure>,\n    signal_refusal: Option<KvmSignalRefusal>,')
edit(f,'    backend_requires_thread_directed_process_signals: bool,','    backend_requires_thread_directed_process_signals: bool,\n\n    /// Virtual KVM task identities must never reach host signal syscalls.\n    backend_rejects_host_signals: bool,')
edit(f,'            backend_failure: None,','            backend_failure: None,\n            signal_refusal: None,')
edit(f,'            backend_supports_parked_write_signal_interruption: cfg','            backend_rejects_host_signals: cfg.backend_rejects_host_signals,\n            backend_supports_parked_write_signal_interruption: cfg')
edit(f,'''    let (next_dtid, req, resp) = {
        let mut sched = sched.lock().unwrap();
        if sched.backend_failed() {
            return Err(SkipTurn);
        }
        sched.step2_process_blocked(&global_time)?;
        sched.step3_peek().ok_or(SkipTurn)?
    };

    finish_selected_turn''', '''    let (selection, failure_wake) = {
        let mut sched = sched.lock().unwrap();
        if sched.backend_failed() {
            return Err(SkipTurn);
        }
        let selection = sched
            .step2_process_blocked(&global_time)
            .and_then(|()| sched.step3_peek().ok_or(SkipTurn));
        (selection, sched.take_failure_notification())
    };
    if let Some(wake) = failure_wake {
        // Publish only after the terminal state and transaction are committed.
        let _ = wake.send(());
    }
    let (next_dtid, req, resp) = selection?;

    finish_selected_turn''')
edit(f,'''    pub(crate) fn backend_failed(&self) -> bool {
        self.backend_failure.is_some()
    }
''', '''    pub(crate) fn backend_failed(&self) -> bool {
        self.backend_failure.is_some() || self.signal_refusal.is_some()
    }

    pub(crate) fn signal_refusal(&self) -> Option<KvmSignalRefusal> {
        self.signal_refusal.clone()
    }

    /// Refuse before interpreting a virtual identity as a host task. The caller
    /// publishes the existing failure notification after releasing this mutex.
    pub(crate) fn reject_host_signal(&mut self, target: DetTid, signal: i32) -> bool {
        if !self.backend_rejects_host_signals {
            return false;
        }
        if !self.backend_failed() {
            if self.run_queue.tentative_pop_in_progress() {
                self.run_queue.undo_tentative_pop();
            }
            self.signal_refusal = Some(KvmSignalRefusal {
                signal,
                target,
                turn: self.turn,
            });
        }
        true
    }

    pub(crate) fn take_failure_notification(&mut self) -> Option<oneshot::Sender<()>> {
        if self.backend_failed() {
            self.backend_failure_sender.take()
        } else {
            None
        }
    }
''')
edit(f,'''        self.step2b_process_timed(); // May populate run_queue.
        self.step2c_process_io_blockers()?;''', '''        self.step2b_process_timed(); // May populate run_queue.
        if self.backend_failed() {
            return Err(SkipTurn);
        }
        self.step2c_process_io_blockers()?;''')
edit(f,'''        info!(
            "[dtid {}] Alarm fired, delivering signal {} to guest.",''', '''        if self.reject_host_signal(target, sig as i32) {
            return;
        }
        info!(
            "[dtid {}] Alarm fired, delivering signal {} to guest.",''')
edit(f,'''    fn signal_guest(&mut self, dettid: DetTid, signal: Signal) {
        debug!(''', '''    fn signal_guest(&mut self, dettid: DetTid, signal: Signal) {
        if self.reject_host_signal(dettid, signal as i32) {
            return;
        }
        debug!(''')
f=files[3]
edit(f,'\nuse crate::config::Config;','\nuse crate::KvmSignalRefusal;\nuse crate::config::Config;')
edit(f,'''pub struct BackendFailureCleanup {
    /// Natural scheduler completion''', '''pub struct BackendFailureCleanup {
    /// The first KVM signal refusal, retained independently of cleanup errors.
    pub signal_refusal: Option<KvmSignalRefusal>,
    /// Natural scheduler completion''')
edit(f,'''    /// Consume failed-run state after the scheduler has naturally finished.''', '''    /// Join the scheduler without publishing a run summary. KVM completion
    /// must inspect the terminal state after this join, including a refusal
    /// recorded while the scheduler was finishing.
    pub async fn join_internal_scheduler(&mut self) -> Result<(), tokio::task::JoinError> {
        if let Some(handle) = self.sched_handle.take() {
            handle.await
        } else {
            Ok(())
        }
    }

    /// Return the retained KVM signal cause, including after a scheduler panic.
    pub fn signal_refusal(&self) -> Option<KvmSignalRefusal> {
        self.sched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .signal_refusal()
    }

    /// Consume failed-run state after the scheduler has naturally finished.''')
edit(f,'''        let scheduler = if let Some(handle) = self.sched_handle.take() {
            handle.await
        } else {
            Ok(())
        };
        // A scheduler panic''', '''        let scheduler = self.join_internal_scheduler().await;
        // A scheduler panic''')
edit(f,'''        let writer = self
            .sched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .preemption_writer
            .take();''', '''        let (writer, signal_refusal) = {
            let mut sched = self
                .sched
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (sched.preemption_writer.take(), sched.signal_refusal())
        };''')
edit(f,'''        BackendFailureCleanup {
            scheduler,
            preemption_recording,
        }''', '''        BackendFailureCleanup {
            signal_refusal,
            scheduler,
            preemption_recording,
        }''')
edit(f,'''            let _sched = self.lock_rpc_scheduler(false).await;
            trace!(
                "[dtid {}] signaling thread with {} at the point of stack trace printing.",''', '''            let mut sched = self.lock_rpc_scheduler(false).await;
            if sched.reject_host_signal(ev.dettid, sig.raw()) {
                let wake = sched.take_failure_notification();
                drop(sched);
                if let Some(wake) = wake {
                    let _ = wake.send(());
                }
                // Keep the ordinary RPC pending. The backend's existing
                // failure listener returns RunAborted and consumes cleanup;
                // neither Continue nor ThreadExited is a valid reply here.
                let _sched = self.lock_rpc_scheduler(false).await;
                unreachable!("terminal signal refusal cannot admit an ordinary RPC");
            }
            trace!(
                "[dtid {}] signaling thread with {} at the point of stack trace printing.",''')
print('Prepared four Detcore source copies; no compilation or tests executed.')
