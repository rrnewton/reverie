/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-private preparation for callback-independent process publication.
//!
//! There is deliberately no Tool installation hook or production publisher.
//! This does not authorize selection, resume a task, or change waitability.
//! Activating it requires the separately reviewed scheduler/selection protocol.
//!
//! Initial registry/image lookups release their guards before taking files.
//! Publication takes file table -> transaction -> lifecycle -> run failure ->
//! process signals. Image validation briefly reacquires image below transaction;
//! child lookup briefly reacquires registry below lifecycle. Those guards are
//! released before failure/process acquisition. The run failure guard remains
//! held through readiness I/O after process/lifecycle guards are released,
//! serializing publications across
//! processes in this run. Existing executor process/thread locks remain below
//! the transaction. No registry or retirement mutex is held during host closes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

use reverie::SignalEvent;
use reverie::SignalProcessId;
use reverie::syscalls::Errno;

use super::FileTableState;
use super::LoadedStaticElf;
use super::set_signalfd_ready;
use crate::elf::TaskLifecycleTable;
use crate::signal::ProcessSignalState;

#[derive(Clone, Debug)]
struct ImageRevision(Arc<()>);

impl PartialEq for ImageRevision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ImageRevision {}

#[derive(Clone)]
struct CurrentImage {
    revision: ImageRevision,
    files: Weak<Mutex<FileTableState>>,
    signals: Weak<Mutex<ProcessSignalState>>,
}

/// Executors own this binding; the run registry never owns a process or image.
/// Threads share it, fork registers a fresh binding, and exec replaces `image`.
pub(super) struct ProcessBinding {
    identity: SignalProcessId,
    parent: Option<SignalProcessId>,
    transaction: Weak<Mutex<()>>,
    lifecycle: Weak<Mutex<TaskLifecycleTable>>,
    image: Mutex<CurrentImage>,
}

impl ProcessBinding {
    pub(super) fn rebind(&self, state: &LoadedStaticElf, files: &Arc<Mutex<FileTableState>>) {
        // Caller holds the authoritative file table and process transaction.
        *self.image.lock().unwrap_or_else(|p| p.into_inner()) = CurrentImage {
            revision: ImageRevision(Arc::new(())),
            files: Arc::downgrade(files),
            signals: Arc::downgrade(&state.process_signals),
        };
    }
}

#[derive(Default)]
pub(super) struct ProcessSignalRegistry {
    processes: Mutex<BTreeMap<(i32, u64), Weak<ProcessBinding>>>,
    // No callbacks or G references. An eventual owner must make its own run
    // terminal after saving FailedAfterCommit. This latch refuses further
    // publication; it is not a substitute for that owner transition.
    failure: Mutex<Option<PublicationFailure>>,
}

impl ProcessSignalRegistry {
    pub(super) fn register(
        &self,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
        generation: u64,
        parent: Option<SignalProcessId>,
    ) -> Arc<ProcessBinding> {
        let binding = Arc::new(ProcessBinding {
            identity: SignalProcessId {
                tgid: reverie::Pid::from_raw(state.pid),
                generation,
            },
            parent,
            transaction: Arc::downgrade(&state.signal_transaction),
            lifecycle: Arc::downgrade(&state.task_lifecycle),
            image: Mutex::new(CurrentImage {
                revision: ImageRevision(Arc::new(())),
                files: Arc::downgrade(files),
                signals: Arc::downgrade(&state.process_signals),
            }),
        });
        let mut processes = self.processes.lock().unwrap_or_else(|p| p.into_inner());
        processes.retain(|_, process| process.strong_count() != 0);
        processes.insert((state.pid, generation), Arc::downgrade(&binding));
        binding
    }

    fn lookup(&self, identity: SignalProcessId) -> Option<Arc<ProcessBinding>> {
        self.processes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(identity.tgid.as_raw(), identity.generation))?
            .upgrade()
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "private inactive capability; no production Tool installation yet"
        )
    )]
    pub(super) fn control(self: &Arc<Self>) -> ProcessSignalControl {
        ProcessSignalControl(Arc::downgrade(self))
    }
}

/// Retaining a control does not retain executors, descriptors, or GlobalState.
pub(super) struct ProcessSignalControl(Weak<ProcessSignalRegistry>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationRejection {
    Closed,
    StaleProcess,
    ChangedImage,
    InvalidEvent,
    InvalidCompletion,
    Backend(Errno),
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationDisposition {
    Ignored,
    Default,
    Caught,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PendingChange {
    Queued,
    Coalesced,
    Suppressed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublicationReceipt {
    process: SignalProcessId,
    image: ImageRevision,
    signal: i32,
    pending_generation: u64,
    change: PendingChange,
    disposition: PublicationDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublicationFailure {
    receipt: PublicationReceipt,
    errno: Errno,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ProcessPublication {
    Rejected(PublicationRejection),
    Committed(PublicationReceipt),
    FailedAfterCommit(PublicationFailure),
}

/// A future completion rendezvous must construct this only from committed
/// child accounting, before retiring the owned child completion. There is no
/// production constructor or child publication caller in this prerequisite.
/// Normal exits only; killed/core exits require their own provenance support.
pub(super) struct CommittedChildExit {
    parent: SignalProcessId,
    child: SignalProcessId,
    status: u8,
    uid: u32,
    user_time: i64,
    system_time: i64,
}

impl ProcessSignalControl {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "inactive private entry point; scheduler activation is separate"
        )
    )]
    pub(super) fn publish_alarm(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
    ) -> ProcessPublication {
        let mut expected = [0; reverie::SIGNAL_INFO_SIZE];
        expected[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
        expected[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
        if event.signal() != libc::SIGALRM
            || event.siginfo() != expected
            || event.target() != (reverie::SignalTarget::Process { pid: target.tgid })
        {
            return ProcessPublication::Rejected(PublicationRejection::InvalidEvent);
        }
        self.publish(target, event, None)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "inactive private entry point; child completion rendezvous is separate"
        )
    )]
    pub(super) fn publish_child_exit(&self, completion: CommittedChildExit) -> ProcessPublication {
        if completion.user_time < 0 || completion.system_time < 0 {
            return ProcessPublication::Rejected(PublicationRejection::InvalidCompletion);
        }
        let mut info = [0; reverie::SIGNAL_INFO_SIZE];
        info[..4].copy_from_slice(&libc::SIGCHLD.to_ne_bytes());
        info[8..12].copy_from_slice(&libc::CLD_EXITED.to_ne_bytes());
        info[16..20].copy_from_slice(&completion.child.tgid.as_raw().to_ne_bytes());
        info[20..24].copy_from_slice(&completion.uid.to_ne_bytes());
        info[24..28].copy_from_slice(&i32::from(completion.status).to_ne_bytes());
        info[32..40].copy_from_slice(&completion.user_time.to_ne_bytes());
        info[40..48].copy_from_slice(&completion.system_time.to_ne_bytes());
        let event = SignalEvent::new(
            libc::SIGCHLD,
            info,
            reverie::SignalTarget::Process {
                pid: completion.parent.tgid,
            },
        );
        let Ok(event) = event else {
            return ProcessPublication::Rejected(PublicationRejection::InvalidCompletion);
        };
        self.publish(completion.parent, event, Some(&completion))
    }

    fn publish(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
        completion: Option<&CommittedChildExit>,
    ) -> ProcessPublication {
        use ProcessPublication::Rejected;
        use PublicationRejection::*;
        let Some(registry) = self.0.upgrade() else {
            return Rejected(Closed);
        };
        let Some(binding) = registry.lookup(target) else {
            return Rejected(StaleProcess);
        };
        let image = binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let (Some(files), Some(transaction), Some(lifecycle), Some(signals)) = (
            image.files.upgrade(),
            binding.transaction.upgrade(),
            binding.lifecycle.upgrade(),
            image.signals.upgrade(),
        ) else {
            return Rejected(StaleProcess);
        };
        // No registry/image mutex is held while taking the file table.
        // Ordinary operations use this same file-table -> transaction order.
        // Ordinary execution treats an authoritative-table poison as failure.
        // Do not let this inactive endpoint publish through an inconsistent
        // table after a failed ordinary update.
        let files = match files.lock() {
            Ok(files) => files,
            Err(_) => return Rejected(Backend(Errno::EIO)),
        };
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        if binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
            != image.revision
        {
            return Rejected(ChangedImage);
        }
        let lifecycle = lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        if binding.identity != target
            || !lifecycle.contains_process(target.tgid.as_raw(), target.generation)
        {
            return Rejected(StaleProcess);
        }
        if let Some(completion) = completion {
            let Some(child) = registry.lookup(completion.child) else {
                return Rejected(InvalidCompletion);
            };
            if child.parent != Some(target)
                || lifecycle.process_exit_status(
                    completion.child.tgid.as_raw(),
                    completion.child.generation,
                ) != Some(reverie::ExitStatus::Exited(i32::from(completion.status)))
            {
                return Rejected(InvalidCompletion);
            }
        }
        // Serialize the run-local terminal latch through the entire commit.
        // This also prevents a different process from publishing after the
        // first failed readiness transaction. No Tool call can occur here.
        let mut failure = registry.failure.lock().unwrap_or_else(|p| p.into_inner());
        if failure.is_some() {
            return Rejected(Terminal);
        }
        let mut process = signals.lock().unwrap_or_else(|p| p.into_inner());
        let signal = event.signal();
        let action = process
            .dispositions
            .get(&signal)
            .copied()
            .unwrap_or_default();
        let disposition = if action.is_ignored() {
            PublicationDisposition::Ignored
        } else if action.handler == libc::SIG_DFL as u64 {
            PublicationDisposition::Default
        } else {
            PublicationDisposition::Caught
        };
        let mut receipt = PublicationReceipt {
            process: target,
            image: image.revision,
            signal,
            pending_generation: process.pending_generation(signal),
            change: PendingChange::Suppressed,
            disposition,
        };
        if signal == libc::SIGCHLD && action.is_ignored() {
            return ProcessPublication::Committed(receipt);
        }
        let matching = process
            .signalfd_masks
            .iter()
            .filter_map(|(&fd, mask)| mask.contains(signal).then_some(fd))
            .collect::<Vec<_>>();
        if matching.iter().any(|fd| !files.files.contains_key(fd)) {
            return Rejected(Backend(Errno::EBADF));
        }
        receipt.change = match process
            .shared_pending
            .enqueue(event, receipt.pending_generation)
        {
            Ok(true) => PendingChange::Queued,
            Ok(false) => PendingChange::Coalesced,
            Err(errno) => return Rejected(Backend(errno)),
        };
        // Release queue/lifecycle locks, retaining descriptor+transaction
        // ownership across only bounded, nonblocking eventfd readiness I/O.
        drop(process);
        drop(lifecycle);
        for fd in matching {
            if let Err(raw) = set_signalfd_ready(&files.files[&fd], true) {
                let committed = PublicationFailure {
                    receipt,
                    errno: Errno::new(i32::try_from(-raw).unwrap_or(libc::EIO)),
                };
                *failure = Some(committed.clone());
                return ProcessPublication::FailedAfterCommit(committed);
            }
        }
        ProcessPublication::Committed(receipt)
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::super::ElfExecutor;
    use super::super::native_loaded_state;
    use super::*;
    use crate::GuestMemory;
    use crate::SyscallRequest;
    use crate::runtime::SyscallExecutor;
    use crate::signal::KernelSigaction;
    use crate::signal::KernelSigset;

    fn executor() -> ElfExecutor {
        ElfExecutor::new(native_loaded_state(std::path::Path::new("/tmp")), false)
    }

    fn identity(executor: &ElfExecutor) -> SignalProcessId {
        executor.signal_task_identity().unwrap().process
    }

    fn alarm(target: SignalProcessId) -> SignalEvent {
        let mut info = [0; reverie::SIGNAL_INFO_SIZE];
        info[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
        info[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
        SignalEvent::new(
            libc::SIGALRM,
            info,
            reverie::SignalTarget::Process { pid: target.tgid },
        )
        .unwrap()
    }

    fn call(executor: &mut ElfExecutor, memory: &GuestMemory, number: i64, args: [u64; 6]) -> i64 {
        executor.execute(&SyscallRequest::new(number as u64, args), memory)
    }

    fn signalfd(executor: &mut ElfExecutor, memory: &mut GuestMemory) -> i32 {
        let mut mask = KernelSigset::default();
        mask.insert(libc::SIGALRM);
        memory.write(0x80, &mask.to_bytes()).unwrap();
        let fd = call(
            executor,
            memory,
            libc::SYS_signalfd4,
            [u64::MAX, 0x80, 8, libc::SFD_NONBLOCK as u64, 0, 0],
        );
        assert!(fd >= 3, "signalfd setup: {fd}");
        fd as i32
    }

    fn ready(executor: &ElfExecutor, fd: i32) -> bool {
        let files = executor.file_table.lock().unwrap();
        let mut poll = libc::pollfd {
            fd: files.files[&fd].as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(unsafe { libc::poll(&mut poll, 1, 0) } >= 0);
        poll.revents & libc::POLLIN != 0
    }

    fn receipt(outcome: ProcessPublication) -> PublicationReceipt {
        match outcome {
            ProcessPublication::Committed(receipt) => receipt,
            other => panic!("publication did not commit: {other:?}"),
        }
    }

    #[test]
    fn inactive_publication_refuses_poisoned_file_table_without_effects() {
        let executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let files = executor.file_table.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = files.lock().unwrap();
                panic!("controlled authoritative-table poison");
            })
            .join()
            .is_err()
        );
        for _ in 0..2 {
            assert!(matches!(
                control.publish_alarm(id, alarm(id)),
                ProcessPublication::Rejected(PublicationRejection::Backend(errno))
                    if errno == Errno::EIO
            ));
        }
        let process = executor.state.process_signals.lock().unwrap();
        assert_eq!(
            process
                .shared_pending
                .pending_mask(&process.pending_generations)
                .to_bytes(),
            KernelSigset::default().to_bytes()
        );
        assert!(executor.signal_registry.failure.lock().unwrap().is_none());
        assert!(executor.file_table.is_poisoned());
    }

    #[test]
    fn inactive_publication_alarm_preserves_masks_dispositions_and_coalesces() {
        for handler in [libc::SIG_DFL as u64, libc::SIG_IGN as u64, 0x1234] {
            for blocked in [false, true] {
                let executor = executor();
                let id = identity(&executor);
                let control = executor.signal_registry.control();
                executor
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .dispositions
                    .insert(
                        libc::SIGALRM,
                        KernelSigaction {
                            handler,
                            ..Default::default()
                        },
                    );
                if blocked {
                    executor
                        .state
                        .thread_signals
                        .lock()
                        .blocked
                        .insert(libc::SIGALRM);
                }
                let before_thread = executor.state.thread_signals.lock().clone();
                let first = receipt(control.publish_alarm(id, alarm(id)));
                assert_eq!(first.change, PendingChange::Queued);
                let second = receipt(control.publish_alarm(id, alarm(id)));
                assert_eq!(second.change, PendingChange::Coalesced);
                assert_eq!(first.image, second.image);
                assert_eq!(first.pending_generation, 0);
                assert_eq!(
                    first.disposition,
                    match handler {
                        0 => PublicationDisposition::Default,
                        1 => PublicationDisposition::Ignored,
                        _ => PublicationDisposition::Caught,
                    }
                );
                assert_eq!(*executor.state.thread_signals.lock(), before_thread);
                assert_eq!(executor.state.logical_clock_ns, 0);
                assert_eq!(
                    executor
                        .state
                        .process_signals
                        .lock()
                        .unwrap()
                        .shared_pending
                        .take_matching(
                            {
                                let mut mask = KernelSigset::default();
                                mask.insert(libc::SIGALRM);
                                mask
                            },
                            &[0; 65]
                        ),
                    Some(alarm(id))
                );
            }
        }
    }

    #[test]
    fn inactive_publication_rejects_bad_event_reused_process_and_closed_run() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut info = alarm(id).siginfo();
        info[127] = 1;
        let invalid = SignalEvent::new(
            libc::SIGALRM,
            info,
            reverie::SignalTarget::Process { pid: id.tgid },
        )
        .unwrap();
        assert_eq!(
            control.publish_alarm(id, invalid),
            ProcessPublication::Rejected(PublicationRejection::InvalidEvent)
        );
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        executor.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        // Numeric reuse in the same lifecycle cannot revive the old generation.
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .register(1, 1, 1, true);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::StaleProcess)
        );
        let signals = Arc::downgrade(&executor.state.process_signals);
        let files = Arc::downgrade(&executor.file_table);
        drop(executor);
        assert!(signals.upgrade().is_none());
        assert!(files.upgrade().is_none());
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Closed)
        );
    }

    #[test]
    fn inactive_publication_resolves_current_exec_image_and_surviving_thread() {
        let mut leader = executor();
        let mut worker = leader.thread_child(2).unwrap();
        let id = identity(&leader);
        let control = leader.signal_registry.control();
        leader.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        leader.release_files_on_exit();
        drop(leader);
        receipt(control.publish_alarm(id, alarm(id)));
        assert_eq!(
            worker
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
        worker.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::StaleProcess)
        );
        drop(worker);

        let mut executor = executor();
        let control = executor.signal_registry.control();
        let id = identity(&executor);
        let first = receipt(control.publish_alarm(id, alarm(id)));
        let old = executor.state.process_signals.clone();
        executor.replace_after_exec(native_loaded_state(std::path::Path::new("/tmp")));
        // Exec preserves pending SIGALRM but resets the image binding.
        let second = receipt(control.publish_alarm(id, alarm(id)));
        assert_ne!(first.image, second.image);
        assert_eq!(second.change, PendingChange::Coalesced);
        assert_eq!(identity(&executor), id);
        assert!(!Arc::ptr_eq(&old, &executor.state.process_signals));
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
        receipt(control.publish_alarm(id, alarm(id)));
        assert!(old.lock().unwrap().shared_pending.contains(libc::SIGALRM));
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
    }

    #[test]
    fn inactive_publication_aliases_all_consumers_and_ignore_update_readiness() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let original = signalfd(&mut executor, &mut memory);
        let duplicate = call(
            &mut executor,
            &memory,
            libc::SYS_dup,
            [original as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(duplicate > original);
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_close,
                [original as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        for consumer in 0..3 {
            receipt(control.publish_alarm(id, alarm(id)));
            assert!(ready(&executor, duplicate));
            match consumer {
                0 => assert_eq!(
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                        .event,
                    alarm(id)
                ),
                1 => {
                    assert_eq!(
                        call(
                            &mut executor,
                            &memory,
                            libc::SYS_read,
                            [duplicate as u64, 0x100, 128, 0, 0, 0]
                        ),
                        128
                    );
                    let mut signo = [0; 4];
                    memory.read(0x100, &mut signo).unwrap();
                    assert_eq!(i32::from_ne_bytes(signo), libc::SIGALRM);
                }
                _ => assert_eq!(
                    call(
                        &mut executor,
                        &memory,
                        libc::SYS_rt_sigtimedwait,
                        [0x80, 0x100, 0, 8, 0, 0]
                    ),
                    i64::from(libc::SIGALRM)
                ),
            }
            assert!(!ready(&executor, duplicate));
        }
        receipt(control.publish_alarm(id, alarm(id)));
        let ignored = KernelSigaction {
            handler: libc::SIG_IGN as u64,
            ..Default::default()
        };
        memory.write(0x200, &ignored.encode()).unwrap();
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_rt_sigaction,
                [libc::SIGALRM as u64, 0x200, 0, 8, 0, 0]
            ),
            0
        );
        assert!(!ready(&executor, duplicate));
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        let later = receipt(control.publish_alarm(id, alarm(id)));
        assert_eq!(later.pending_generation, 1);
        assert!(ready(&executor, duplicate));
        // Closing an alias must not cause the private publisher to use its old
        // local descriptor snapshot or to write into a subsequently reused fd.
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_close,
                [duplicate as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        receipt(control.publish_alarm(id, alarm(id)));
        assert!(executor.file_table.lock().unwrap().files.is_empty());
    }

    #[test]
    fn inactive_publication_preflight_and_postcommit_failure_are_distinct() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        let carrier = executor
            .file_table
            .lock()
            .unwrap()
            .files
            .remove(&fd)
            .unwrap();
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Backend(Errno::EBADF))
        );
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        // Deliberate backing-carrier corruption: real readiness write fails
        // EBADF after queue insertion. No production mutator installs this.
        executor
            .file_table
            .lock()
            .unwrap()
            .files
            .insert(fd, std::fs::File::open("/dev/null").unwrap());
        let ProcessPublication::FailedAfterCommit(failure) = control.publish_alarm(id, alarm(id))
        else {
            panic!("missing postcommit failure")
        };
        assert_eq!(failure.errno, Errno::EBADF);
        assert_eq!(failure.receipt.change, PendingChange::Queued);
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGALRM)
        );
        assert_eq!(
            *executor.signal_registry.failure.lock().unwrap(),
            Some(failure)
        );
        executor
            .file_table
            .lock()
            .unwrap()
            .files
            .insert(fd, carrier);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Terminal)
        );
    }

    #[test]
    fn inactive_publication_concurrent_dequeue_keeps_readiness_equal_to_pending() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        for _ in 0..64 {
            receipt(control.publish_alarm(id, alarm(id)));
            let start = std::sync::Barrier::new(2);
            let (published, removed) = std::thread::scope(|scope| {
                let publisher = scope.spawn(|| {
                    start.wait();
                    receipt(control.publish_alarm(id, alarm(id)))
                });
                let consumer = scope.spawn(|| {
                    start.wait();
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                });
                (publisher.join().unwrap(), consumer.join().unwrap())
            });
            assert_eq!(removed.event, alarm(id));
            let queued_after_remove = published.change == PendingChange::Queued;
            assert_eq!(
                executor
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .shared_pending
                    .contains(libc::SIGALRM),
                queued_after_remove
            );
            assert_eq!(ready(&executor, fd), queued_after_remove);
            if queued_after_remove {
                assert_eq!(
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                        .event,
                    alarm(id)
                );
            }
            assert!(!ready(&executor, fd));
        }
    }

    #[test]
    fn inactive_publication_child_requires_committed_generation_and_parent() {
        let mut parent = executor();
        let id = identity(&parent);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let child_id = identity(&child);
        let control = parent.signal_registry.control();
        let completion = || CommittedChildExit {
            parent: id,
            child: child_id,
            status: 23,
            uid: 0,
            user_time: 11,
            system_time: 13,
        };
        assert_eq!(
            control.publish_child_exit(completion()),
            ProcessPublication::Rejected(PublicationRejection::InvalidCompletion)
        );
        child.retire_current_thread(reverie::ExitStatus::Exited(23), false);
        let first = receipt(control.publish_child_exit(completion()));
        assert_eq!(first.change, PendingChange::Queued);
        let pending = parent.take_pending_signal_for_delivery().unwrap().unwrap();
        let info = pending.event.siginfo();
        assert_eq!(pending.event.signal(), libc::SIGCHLD);
        assert_eq!(
            i32::from_ne_bytes(info[8..12].try_into().unwrap()),
            libc::CLD_EXITED
        );
        assert_eq!(i32::from_ne_bytes(info[16..20].try_into().unwrap()), 2);
        assert_eq!(i32::from_ne_bytes(info[24..28].try_into().unwrap()), 23);
        assert_eq!(i64::from_ne_bytes(info[32..40].try_into().unwrap()), 11);
        assert_eq!(i64::from_ne_bytes(info[40..48].try_into().unwrap()), 13);
        let mut wrong = completion();
        wrong.child.generation += 1;
        assert_eq!(
            control.publish_child_exit(wrong),
            ProcessPublication::Rejected(PublicationRejection::InvalidCompletion)
        );
        let other = parent.fork_child(3, false, false).unwrap();
        let mut wrong = completion();
        wrong.parent = identity(&other);
        assert_eq!(
            control.publish_child_exit(wrong),
            ProcessPublication::Rejected(PublicationRejection::InvalidCompletion)
        );
        parent
            .state
            .process_signals
            .lock()
            .unwrap()
            .dispositions
            .insert(
                libc::SIGCHLD,
                KernelSigaction {
                    handler: libc::SIG_IGN as u64,
                    ..Default::default()
                },
            );
        assert_eq!(
            receipt(control.publish_child_exit(completion())).change,
            PendingChange::Suppressed
        );
        assert!(
            parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        assert!(
            parent.state.children.is_empty(),
            "publication must not manufacture waitability"
        );
    }
}
