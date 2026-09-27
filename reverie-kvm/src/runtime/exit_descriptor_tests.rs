/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipeObservation {
    WouldBlock,
    Eof,
    Byte(u8),
}

fn observe_pipe(reader: &File) -> PipeObservation {
    let mut byte = 0;
    // SAFETY: reader owns a nonblocking pipe and byte is writable for one byte.
    match unsafe { libc::read(reader.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) } {
        0 => PipeObservation::Eof,
        1 => PipeObservation::Byte(byte),
        -1 => {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EAGAIN),
                "host pipe read failed for a reason other than a live writer"
            );
            PipeObservation::WouldBlock
        }
        result => panic!("one-byte pipe read returned {result}"),
    }
}

fn nonblocking_pipe() -> (File, File) {
    let mut fds = [-1; 2];
    // SAFETY: fds has room for both descriptors returned by pipe2.
    assert_eq!(
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
        0
    );
    // SAFETY: successful pipe2 returned two distinct owned descriptors.
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

#[derive(Default)]
struct ExitDescriptorGlobal {
    reader: Option<File>,
    expected: Mutex<Option<(reverie::SignalBoundaryReceipt, PipeObservation)>>,
    receipts: Mutex<Vec<reverie::SignalBoundaryReceipt>>,
}

#[reverie::global_tool]
impl GlobalTool for ExitDescriptorGlobal {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _: Pid, _: ()) {}

    async fn on_backend_signal_boundary(
        &self,
        receipt: reverie::SignalBoundaryReceipt,
    ) -> std::result::Result<(), reverie::Error> {
        let (expected, pipe) = self.expected.lock().unwrap().take().unwrap();
        assert_eq!(receipt, expected, "the exact reserved boundary must settle");
        // This is the callback that can commit Detcore's Exit and admit the
        // next turn. An observation after finish_signal_boundary returns, or
        // in on_exit_thread, would miss a close performed too late.
        assert_eq!(
            observe_pipe(self.reader.as_ref().unwrap()),
            pipe,
            "descriptor visibility at the consuming boundary receipt"
        );
        self.receipts.lock().unwrap().push(receipt);
        Ok(())
    }
}

struct Fixture {
    backend: KvmBackend,
    executor: ElfExecutor,
    memory: GuestMemory,
    global: Arc<ExitDescriptorGlobal>,
    _failure: Arc<RunFailure>,
}

impl Fixture {
    fn new(stdin: bool) -> Self {
        let (reader, writer) = nonblocking_pipe();
        let mut state = crate::executor::native_loaded_state(&std::env::current_dir().unwrap());
        let reserved_stdin = if stdin {
            state.stdin = Some(writer.try_clone().unwrap());
            Some(writer)
        } else {
            assert!(state.insert_file(3, writer).is_empty());
            None
        };
        // These controls create a VM, as the neighboring runtime controls do,
        // but execute no guest instructions and need no compiled guest image.
        let backend = KvmBackend::new_with_stdin(0x10000, reserved_stdin)
            .expect("exit descriptor controls require /dev/kvm");
        let executor = ElfExecutor::new(state, false);
        let global = Arc::new(ExitDescriptorGlobal {
            reader: Some(reader),
            ..ExitDescriptorGlobal::default()
        });
        let failure = RunFailure::new(&global);
        executor
            .install_signal_control(reverie::BackendSignalControlMode::ToolControlled, &failure);
        assert_eq!(
            observe_pipe(global.reader.as_ref().unwrap()),
            PipeObservation::WouldBlock,
            "the executor starts with an open writer"
        );
        Self {
            backend,
            executor,
            memory: GuestMemory::new(0, 4096).unwrap(),
            global,
            _failure: failure,
        }
    }
}

fn reserve_boundary(executor: &ElfExecutor, sequence: u64) -> reverie::SignalDeliveryPermit {
    let permit = reverie::SignalDeliveryPermit {
        task: executor.signal_task_identity().unwrap(),
        site: None,
        sequence,
    };
    executor
        .backend_signal_control()
        .process
        .reserve_delivery(permit)
        .unwrap();
    assert_eq!(executor.owned_delivery_permit(), Some(permit));
    permit
}

fn settle_boundary(
    backend: &mut KvmBackend,
    executor: &mut ElfExecutor,
    global: &Arc<ExitDescriptorGlobal>,
    permit: reverie::SignalDeliveryPermit,
    outcome: reverie::SignalBoundaryOutcome,
    pipe: PipeObservation,
) {
    let receipt = reverie::SignalBoundaryReceipt { permit, outcome };
    let before = global.receipts.lock().unwrap().len();
    assert!(
        global
            .expected
            .lock()
            .unwrap()
            .replace((receipt, pipe))
            .is_none()
    );
    futures::executor::block_on(backend.finish_signal_boundary(executor, global.as_ref(), outcome))
        .unwrap();
    assert!(global.expected.lock().unwrap().is_none());
    assert_eq!(global.receipts.lock().unwrap().len(), before + 1);
    assert_eq!(executor.owned_delivery_permit(), None);
}

fn exit_syscall(executor: &mut ElfExecutor, memory: &GuestMemory, group: bool) -> ProcessExit {
    let number = if group {
        libc::SYS_exit_group
    } else {
        libc::SYS_exit
    };
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(number as u64, [37, 0, 0, 0, 0, 0]),
            memory
        ),
        0
    );
    let exit = executor.take_exit().unwrap();
    assert_eq!(exit.status, ExitStatus::Exited(37));
    assert_eq!(exit.group, group);
    exit
}

#[test]
fn exit_and_exit_group_receipts_observe_descriptor_eof() {
    for group in [false, true] {
        let mut f = Fixture::new(false);
        let permit = reserve_boundary(&f.executor, 7);
        let exit = exit_syscall(&mut f.executor, &f.memory, group);
        settle_boundary(
            &mut f.backend,
            &mut f.executor,
            &f.global,
            permit,
            exit.signal_boundary_outcome(),
            PipeObservation::Eof,
        );
    }
}

#[test]
fn fatal_terminal_receipt_observes_descriptor_eof() {
    let mut f = Fixture::new(false);
    let permit = reserve_boundary(&f.executor, 7);
    f.executor.force_signal_exit(libc::SIGTERM);
    let exit = f.executor.take_exit().unwrap();
    assert_eq!(exit.status.into_raw(), libc::SIGTERM);
    assert!(exit.group);
    settle_boundary(
        &mut f.backend,
        &mut f.executor,
        &f.global,
        permit,
        exit.signal_boundary_outcome(),
        PipeObservation::Eof,
    );
}

#[test]
fn terminal_receipt_releases_executor_and_backend_stdin_owners() {
    let mut f = Fixture::new(true);
    let permit = reserve_boundary(&f.executor, 7);
    let exit = exit_syscall(&mut f.executor, &f.memory, true);
    settle_boundary(
        &mut f.backend,
        &mut f.executor,
        &f.global,
        permit,
        exit.signal_boundary_outcome(),
        PipeObservation::Eof,
    );
}

#[test]
fn returning_signal_receipts_preserve_open_descriptors() {
    for outcome in [
        reverie::SignalBoundaryOutcome::Caught,
        reverie::SignalBoundaryOutcome::NoHandler,
    ] {
        let mut f = Fixture::new(false);
        let permit = reserve_boundary(&f.executor, 7);
        settle_boundary(
            &mut f.backend,
            &mut f.executor,
            &f.global,
            permit,
            outcome,
            PipeObservation::WouldBlock,
        );
        f.memory.write(0x100, b"r").unwrap();
        assert_eq!(
            f.executor.execute(
                &SyscallRequest::new(libc::SYS_write as u64, [3, 0x100, 1, 0, 0, 0]),
                &f.memory,
            ),
            1,
            "a returning boundary must preserve the usable guest descriptor"
        );
        assert_eq!(
            observe_pipe(f.global.reader.as_ref().unwrap()),
            PipeObservation::Byte(b'r')
        );
    }
}

#[test]
fn thread_exit_receipt_preserves_the_live_shared_files_owner() {
    let mut f = Fixture::new(false);
    let mut sibling = f.executor.thread_child(2).unwrap();
    let permit = reserve_boundary(&f.executor, 7);
    let exit = exit_syscall(&mut f.executor, &f.memory, false);
    settle_boundary(
        &mut f.backend,
        &mut f.executor,
        &f.global,
        permit,
        exit.signal_boundary_outcome(),
        PipeObservation::WouldBlock,
    );
    f.memory.write(0x100, b"s").unwrap();
    assert_eq!(
        sibling.execute(
            &SyscallRequest::new(libc::SYS_write as u64, [3, 0x100, 1, 0, 0, 0]),
            &f.memory,
        ),
        1,
        "the live CLONE_FILES sibling must retain its usable writer"
    );
    assert_eq!(
        observe_pipe(f.global.reader.as_ref().unwrap()),
        PipeObservation::Byte(b's')
    );
    let permit = reserve_boundary(&sibling, 8);
    let exit = exit_syscall(&mut sibling, &f.memory, false);
    settle_boundary(
        &mut f.backend,
        &mut sibling,
        &f.global,
        permit,
        exit.signal_boundary_outcome(),
        PipeObservation::Eof,
    );
}
