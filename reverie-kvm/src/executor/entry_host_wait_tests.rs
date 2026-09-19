//! The unchanged-mapping fence must not wait for a retained kernel operand or
//! for allocation-only mmap file preparation. These controls execute the real
//! Host adapters; they do not change a mapping while the fence is held.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use futures::FutureExt;

use super::*;

type MmapReadObserver = Rc<dyn Fn()>;

thread_local! {
    static MMAP_READ_OBSERVER: RefCell<Option<MmapReadObserver>> = const { RefCell::new(None) };
}

pub(super) fn observe_mmap_file_read() {
    let observer = MMAP_READ_OBSERVER.with(|slot| slot.borrow().clone());
    if let Some(observer) = observer {
        observer();
    }
}

struct MmapObserverGuard(Option<MmapReadObserver>);

impl MmapObserverGuard {
    fn install(observer: MmapReadObserver) -> Self {
        Self(MMAP_READ_OBSERVER.with(|slot| slot.replace(Some(observer))))
    }
}

impl Drop for MmapObserverGuard {
    fn drop(&mut self) {
        let current = MMAP_READ_OBSERVER.with(|slot| slot.replace(self.0.take()));
        drop(current);
    }
}

struct FutexWaiter {
    handle: Option<JoinHandle<i64>>,
    // Both addresses belong to the retained Mapping until this worker joins.
    original_address: usize,
    requeued_address: usize,
}

impl FutexWaiter {
    fn join(&mut self) -> i64 {
        self.handle.take().unwrap().join().unwrap()
    }
}

impl Drop for FutexWaiter {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Rescue only: a failing assertion can precede or follow requeue.
            // Wake both possible queues, then retain the real worker through
            // join. Its finite kernel timeout also bounds the pre-queue race.
            for address in [self.original_address, self.requeued_address] {
                // SAFETY: the controller's memory view outlives this guard;
                // the aligned words are inside it, and no layout is changed.
                unsafe {
                    libc::syscall(libc::SYS_futex, address, libc::FUTEX_WAKE, 1);
                }
            }
            let _ = handle.join();
        }
    }
}

#[test]
fn queued_host_futex_retains_operands_while_unchanged_mapping_fence_completes() {
    const WORD: u64 = 0x1_0000;
    const SECOND: u64 = WORD + 4;
    const TIMEOUT: u64 = WORD + 16;
    let memory = GuestMemory::new(WORD, PAGE_SIZE as usize).unwrap();
    memory.map_user_range(WORD, PAGE_SIZE, false).unwrap();
    memory.enable_user_access();
    let timeout = libc::timespec {
        tv_sec: 5,
        tv_nsec: 0,
    };
    // SAFETY: the bytes cover the initialized native timespec passed to the
    // actual host futex adapter, within the retained guest memory allocation.
    let timeout_bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&timeout).cast::<u8>(),
            std::mem::size_of::<libc::timespec>(),
        )
    };
    memory.write_raw(TIMEOUT, timeout_bytes).unwrap();
    let owners = memory.test_mapping_owners();
    let gate = memory.entry_gate();
    let original_address = memory.host_address() as usize;
    let requeued_address = original_address + (SECOND - WORD) as usize;
    assert_eq!(original_address % std::mem::align_of::<u32>(), 0);
    assert_eq!(requeued_address % std::mem::align_of::<u32>(), 0);
    assert_eq!(owners(), 1);
    let waiting_memory = memory.clone();
    let (finished, completion) = mpsc::channel();
    let mut waiter = FutexWaiter {
        handle: Some(std::thread::spawn(move || {
            // Exactly one adapter invocation, with no EINTR or timeout retry.
            let result = futex(
                &waiting_memory,
                &[WORD, libc::FUTEX_WAIT as u64, 0, TIMEOUT, 0, 0],
            );
            finished.send(result).unwrap();
            result
        })),
        original_address,
        requeued_address,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // Zero wakes, at most one requeue. Returning one proves a real queued
        // kernel waiter and leaves it asleep at SECOND without completing it.
        let moved = futex(
            &memory,
            &[WORD, libc::FUTEX_CMP_REQUEUE as u64, 0, 1, SECOND, 0],
        );
        assert!(moved == 0 || moved == 1, "requeue returned {moved}");
        if moved == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "waiter never entered kernel queue"
        );
        std::thread::yield_now();
    }
    assert_eq!(completion.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(!waiter.handle.as_ref().unwrap().is_finished());
    // Controller view + waiter view + actual retained word and timeout. The
    // operand admission tokens have retired; their Mapping owners have not.
    assert_eq!(owners(), 4);
    assert_eq!(gate.test_state().copies, 0);
    let closed = gate
        .try_close()
        .unwrap()
        .unwrap()
        .finish()
        .now_or_never()
        .expect("unchanged-mapping fence waited for the kernel futex")
        .unwrap();
    assert!(gate.test_state().closed);
    assert_eq!(gate.test_state().copies, 0);
    assert_eq!(owners(), 4);
    assert_eq!(completion.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(!waiter.handle.as_ref().unwrap().is_finished());
    drop(closed);
    assert_eq!(
        futex(&memory, &[SECOND, libc::FUTEX_WAKE as u64, 1, 0, 0, 0]),
        1
    );
    assert_eq!(completion.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
    assert_eq!(waiter.join(), 0);
    assert_eq!(owners(), 1);
    assert!(gate.pending_failure().is_none());
    assert_eq!(
        futex(&memory, &[SECOND, libc::FUTEX_WAKE as u64, 1, 0, 0, 0]),
        0
    );
    drop(memory);
    assert_eq!(owners(), 0);
}

struct MmapReader {
    release: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<i64>>,
}

impl MmapReader {
    fn finish(&mut self) -> i64 {
        self.release.take().unwrap().send(()).unwrap();
        self.handle.take().unwrap().join().unwrap()
    }
}

impl Drop for MmapReader {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn allocation_held_mmap_file_read_does_not_hold_unchanged_mapping_fence() {
    let mut state = native_loaded_state(std::path::Path::new("/"));
    let expected_address = state.mmap_base;
    let memory = GuestMemory::new(0, state.mmap_limit as usize).unwrap();
    memory.enable_user_access();
    let expected: Vec<u8> = (0..PAGE_SIZE).map(|i| (i * 17 + 3) as u8).collect();
    // SAFETY: the fixed name is nul-terminated. A successful returned fd is
    // immediately owned by File and follows the ordinary mmap read path.
    let raw = unsafe { libc::memfd_create(c"entry-mmap-wait".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    file.write_all(&expected).unwrap();
    let fd = insert_file_with_flags(&mut state, file, false, None);
    assert_eq!(fd, 3);
    let gate = memory.entry_gate();
    let owners = memory.test_mapping_owners();
    let mut reading_memory = memory.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reader_reads = reads.clone();
    let (prepared, preparation) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (completed, completion) = mpsc::channel();
    let mut reader = MmapReader {
        release: Some(release),
        handle: Some(std::thread::spawn(move || {
            let _observer = MmapObserverGuard::install(Rc::new(move || {
                assert_eq!(reader_reads.fetch_add(1, Ordering::SeqCst), 0);
                prepared.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }));
            // This wrapper owns the actual allocation_guard through mmap's
            // file.read_at loop, and only later performs admitted byte copies.
            let result = match execute_basic_syscall(
                &mut reading_memory,
                &mut state,
                &SyscallRequest::new(
                    libc::SYS_mmap as u64,
                    [
                        0,
                        PAGE_SIZE,
                        (libc::PROT_READ | libc::PROT_WRITE) as u64,
                        libc::MAP_PRIVATE as u64,
                        fd as u64,
                        0,
                    ],
                ),
            ) {
                SyscallAction::Continue {
                    result,
                    segment: None,
                } => result,
                _ => panic!("mmap did not retain its ordinary syscall disposition"),
            };
            completed.send(result).unwrap();
            result
        })),
    };
    preparation.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(completion.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(!reader.handle.as_ref().unwrap().is_finished());
    // The wrapper's extra owner is still live with its allocation guard.
    assert_eq!(owners(), 3);
    assert_eq!(gate.test_state().copies, 0);
    let closed = gate
        .try_close()
        .unwrap()
        .unwrap()
        .finish()
        .now_or_never()
        .expect("fence waited for allocation-held file preparation")
        .unwrap();
    assert!(gate.test_state().closed);
    assert_eq!(gate.test_state().copies, 0);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(completion.try_recv(), Err(mpsc::TryRecvError::Empty));
    // Reopen before releasing the actual file read and demanding the later
    // reservation/population completion; those byte copies need admission.
    drop(closed);
    assert_eq!(reader.finish(), expected_address as i64);
    assert_eq!(
        completion.recv_timeout(Duration::from_secs(2)).unwrap(),
        expected_address as i64
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(owners(), 1);
    let mut actual = vec![0; PAGE_SIZE as usize];
    memory.user().read(expected_address, &mut actual).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        memory.reservation_kind(expected_address),
        Some(RegionKind::Mmap)
    );
    assert!(gate.pending_failure().is_none());
    drop(memory);
    assert_eq!(owners(), 0);
}
