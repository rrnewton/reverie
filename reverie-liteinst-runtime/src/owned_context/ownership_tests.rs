use std::cell::Cell;
use std::os::unix::process::ExitStatusExt;
use std::sync::Barrier;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU32;
use std::time::Duration;
use std::time::Instant;

use super::*;

struct Installed {
    state: Box<OwnedContext>,
    evidence: Box<Evidence>,
}

impl Installed {
    fn new(phase: u8) -> Self {
        assert!(STATE.with(|slot| slot.load(Ordering::Acquire)).is_null());
        assert!(EVIDENCE.with(|slot| slot.load(Ordering::Acquire)).is_null());
        assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), IDLE);
        let mut installed = Self {
            state: Box::new(OwnedContext {
                owner: 0,
                mappings: Vec::new(),
                readable: Vec::new(),
                writable: Vec::new(),
                vdso: None,
                signal_stack: 0..0,
                runtime_sp: 0,
                runtime_stack: 0..0,
                storage: ptr::null_mut(),
                image: None,
                registers: unsafe { core::mem::zeroed() },
                event: None,
                stepper: Some(Stepper::default()),
                precise_timer: true,
                syscalls: true,
                native: Some(native_step::NativeState::new(0).unwrap()),
                guest_mask: 0,
                subscriptions: crate::runtime::InstructionSubscriptions::default(),
                clocked: true,
                controls: Controls {
                    segments_and_permissions: [0; 3],
                    xcr0: 0,
                    pkru: 0,
                    cpuid: 0,
                    tsc: 0,
                },
                errno: ptr::null_mut(),
                saved_errno: 0,
                generation: 0,
            }),
            evidence: Box::new(Evidence {
                bytes: [0; 32 * (STORAGE_BYTES + 64)],
                used: 0,
            }),
        };
        installed.state.owner = syscall(libc::SYS_gettid, [0; 6]);
        STATE.with(|slot| slot.store(&mut *installed.state, Ordering::Release));
        EVIDENCE.with(|slot| slot.store(&mut *installed.evidence, Ordering::Release));
        PHASE.with(|slot| slot.store(phase, Ordering::Release));
        installed
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        STATE.with(|slot| slot.store(ptr::null_mut(), Ordering::Release));
        EVIDENCE.with(|slot| slot.store(ptr::null_mut(), Ordering::Release));
        PHASE.with(|slot| slot.store(IDLE, Ordering::Release));
    }
}

#[test]
fn distinct_threads_own_context_phase_frame_and_evidence() {
    let barrier = Barrier::new(2);
    let observations = std::thread::scope(|scope| {
        let workers: Vec<_> = [CAPTURED, RETURNING]
            .into_iter()
            .map(|phase| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut installed = Installed::new(phase);
                    let mut storage = Box::new(Storage([0; STORAGE_BYTES]));
                    installed.state.storage = &mut *storage;
                    installed.state.generation = u64::from(phase);
                    installed.evidence.used = usize::from(phase);
                    barrier.wait();
                    assert_eq!(
                        STATE.with(|slot| slot.load(Ordering::Acquire)),
                        &mut *installed.state as *mut OwnedContext
                    );
                    assert_eq!(
                        EVIDENCE.with(|slot| slot.load(Ordering::Acquire)),
                        &mut *installed.evidence as *mut Evidence
                    );
                    assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), phase);
                    assert_eq!(installed.state.generation, u64::from(phase));
                    assert_eq!(installed.evidence.used, usize::from(phase));
                    let result = (
                        installed.state.owner,
                        installed.state.storage as usize,
                        &*installed.state as *const OwnedContext as usize,
                        &*installed.evidence as *const Evidence as usize,
                    );
                    if phase == CAPTURED {
                        assert!(enter_runtime());
                        assert!(!enter_runtime());
                    }
                    barrier.wait();
                    if phase == RETURNING {
                        assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), RETURNING);
                    }
                    result
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let (first, second) = (observations[0], observations[1]);
    assert_ne!(first.0, second.0);
    assert_ne!(first.1, second.1);
    assert_ne!(first.2, second.2);
    assert_ne!(first.3, second.3);
}

#[test]
fn capture_identity_rejects_foreign_owner_and_frame() {
    let mut installed = Installed::new(RETURNING);
    let mut stack = vec![0u8; 64 * 1024];
    let base = stack.as_mut_ptr() as usize;
    installed.state.signal_stack = base..base + stack.len();
    let entry = base + 48 * 1024;
    let info = (entry + 312) as *mut libc::siginfo_t;
    let context = (entry + 8) as *mut libc::c_void;
    let tid = installed.state.owner;
    assert!(capture_identity_matches(
        &installed.state,
        tid,
        entry,
        info,
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid + 1,
        entry,
        info,
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid,
        entry - 8,
        info,
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid,
        base,
        info,
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid,
        usize::MAX,
        info,
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid,
        entry,
        ptr::null_mut(),
        context
    ));
    assert!(!capture_identity_matches(
        &installed.state,
        tid,
        entry,
        info,
        ptr::null_mut()
    ));
}

#[test]
fn one_thread_phase_transitions_and_stale_dispatch_rejection() {
    let _installed = Installed::new(CAPTURED);
    assert!(enter_runtime());
    assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), RUNTIME);
    for stale in [IDLE, RUNTIME, RETURNING, FAILED, TERMINATING] {
        PHASE.with(|slot| slot.store(stale, Ordering::Release));
        assert!(!enter_runtime());
    }
    PHASE.with(|slot| slot.store(CAPTURED, Ordering::Release));
    assert!(enter_runtime());
    PHASE.with(|slot| slot.store(RETURNING, Ordering::Release));
    assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), RETURNING);
}

#[test]
fn fresh_host_thread_cannot_reuse_parent_ownership() {
    let _installed = Installed::new(RETURNING);
    std::thread::spawn(|| {
        assert!(!owned_state_published());
        assert!(!syscall_mode());
        assert!(STATE.with(|slot| slot.load(Ordering::Acquire)).is_null());
        assert!(EVIDENCE.with(|slot| slot.load(Ordering::Acquire)).is_null());
        assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), IDLE);
    })
    .join()
    .unwrap();
    assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), RETURNING);
}

#[repr(C)]
#[derive(Default)]
struct Probe {
    expected_entry: usize,
    entry: usize,
    state: usize,
    phase: u8,
}

unsafe extern "C" fn frame_probe(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
    _scope: u64,
    entry: usize,
) -> Continuation {
    assert_eq!(signal, libc::SIGTRAP);
    assert!(info.is_null());
    let probe = unsafe { &mut *context.cast::<Probe>() };
    probe.entry = entry;
    probe.state = STATE.with(|slot| slot.load(Ordering::Acquire)) as usize;
    probe.phase = PHASE.with(|slot| slot.load(Ordering::Acquire));
    Continuation::GUEST
}

reverie_preload::clocked_signal!(probe_entry, frame_probe, frame_entry);

#[unsafe(naked)]
unsafe extern "C" fn call_probe(_: *mut Probe) {
    core::arch::naked_asm!(
        "sub rsp, 8", "lea rax, [rsp - 8]", "mov [rdi], rax",
        "mov rdx, rdi", "mov edi, 5", "xor esi, esi", "call {entry}",
        "add rsp, 8", "ret", entry = sym probe_entry,
    );
}

#[test]
fn frame_entry_abi_carries_each_threads_original_stack_without_global_slot() {
    const CHILD: &str = "LITEINST_OWNED_THREAD_FRAME_ABI_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "owned_context::ownership_tests::frame_entry_abi_carries_each_threads_original_stack_without_global_slot", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        for phase in [CAPTURED, RETURNING] {
            let barrier = &barrier;
            scope.spawn(move || {
                let installed = Installed::new(phase);
                let mut probe = Probe::default();
                barrier.wait();
                unsafe { call_probe(&mut probe) };
                assert_eq!(probe.entry, probe.expected_entry);
                assert_eq!(probe.entry & 15, 8);
                assert_eq!(
                    probe.state,
                    &*installed.state as *const OwnedContext as usize
                );
                assert_eq!(probe.phase, phase);
                barrier.wait();
            });
        }
    });
}

#[derive(Clone, Copy)]
struct CaptureObservation {
    entry: usize,
    errno: i32,
    reenter: bool,
}

thread_local! {
    static OBSERVATION: Cell<Option<CaptureObservation>> = const { Cell::new(None) };
}

static FIRST_TID: AtomicI64 = AtomicI64::new(0);
static FIRST_ENTERED: AtomicU32 = AtomicU32::new(0);
static SECOND_ENTERED: AtomicU32 = AtomicU32::new(0);
static FIRST_OBSERVED: AtomicU32 = AtomicU32::new(0);

#[unsafe(naked)]
unsafe extern "C" fn overlap_enter() -> u64 {
    core::arch::naked_asm!(
        "push r12", "push r13", "sub rsp, 16",
        "mov qword ptr [rsp], 5", "mov qword ptr [rsp + 8], 0",
        "mov eax, 186", "syscall", "cmp rax, [rip + {first_tid}]", "jne 2f",
        "mov dword ptr [rip + {first_entered}], 1",
        "lea r12, [rip + {second_entered}]", "mov r13d, 1", "jmp 3f",
        "2:", "mov dword ptr [rip + {second_entered}], 1",
        "lea rdi, [rip + {second_entered}]", "mov esi, 129", "mov edx, 1",
        "mov eax, 202", "syscall",
        "lea r12, [rip + {first_observed}]", "mov r13d, 2",
        "3:", "cmp dword ptr [r12], 1", "je 4f",
        "mov rdi, r12", "mov esi, 128", "xor edx, edx", "mov r10, rsp",
        "mov eax, 202", "syscall", "cmp dword ptr [r12], 1", "je 4f",
        "mov edi, 125", "mov eax, 231", "syscall", "ud2",
        "4:", "mov rax, r13", "add rsp, 16", "pop r13", "pop r12", "ret",
        first_tid = sym FIRST_TID, first_entered = sym FIRST_ENTERED,
        second_entered = sym SECOND_ENTERED, first_observed = sym FIRST_OBSERVED,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn overlap_leave(_: u64) {
    core::arch::naked_asm!("ret");
}

static OVERLAP_SCOPE: reverie_preload::clock_boundary::SignalScope =
    reverie_preload::clock_boundary::SignalScope {
        enter: overlap_enter,
        leave: overlap_leave,
    };

#[unsafe(naked)]
unsafe extern "C" fn call_production_entry(_: usize, _: usize) {
    core::arch::naked_asm!(
        "push r12", "mov r12, rsp", "mov rax, rsi", "mov rsp, rdi",
        "sub rsp, 448", "mov rdx, rsp", "lea rsi, [rsp + 304]", "add rsi, rax",
        "mov edi, 5", "call {entry}", "mov rsp, r12", "pop r12", "ret",
        entry = sym signal_entry,
    );
}

struct CaptureFixture {
    installed: Installed,
    stack: Vec<u8>,
    errno: Box<i32>,
    top: usize,
}

impl CaptureFixture {
    fn new() -> Self {
        let mut installed = Installed::new(RETURNING);
        let mut stack = vec![0u8; 1024 * 1024];
        let base = stack.as_mut_ptr() as usize;
        let top = (base + stack.len()) & !15;
        let mut errno = Box::new(731);
        installed.state.signal_stack = base..base + stack.len();
        installed.state.errno = &mut *errno;
        OBSERVATION.with(|slot| {
            slot.set(Some(CaptureObservation {
                entry: top - 456,
                errno: *errno,
                reenter: false,
            }))
        });
        Self {
            installed,
            stack,
            errno,
            top,
        }
    }

    fn invoke(&self, info_offset: usize) {
        assert_eq!(self.stack.len(), 1024 * 1024);
        assert_eq!(*self.errno, 731);
        unsafe { call_production_entry(self.top, info_offset) };
        assert!(OBSERVATION.with(|slot| slot.get()).is_none());
    }
}

struct UnreadableErrno(*mut libc::c_void);

impl UnreadableErrno {
    fn new() -> Self {
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                4096,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED);
        Self(address)
    }
}

impl Drop for UnreadableErrno {
    fn drop(&mut self) {
        assert_eq!(unsafe { libc::munmap(self.0, 4096) }, 0);
    }
}

pub(super) fn observe_capture(
    state: &mut OwnedContext,
    entry: usize,
    saved_errno: i32,
) -> Option<Continuation> {
    let observation = OBSERVATION.with(|slot| slot.take())?;
    assert_eq!(entry, observation.entry);
    assert_eq!(saved_errno, observation.errno);
    assert_eq!(state.owner, syscall(libc::SYS_gettid, [0; 6]));
    assert_eq!(PHASE.with(|slot| slot.load(Ordering::Acquire)), RETURNING);
    if state.owner == FIRST_TID.load(Ordering::Acquire) {
        assert_eq!(SECOND_ENTERED.load(Ordering::Acquire), 1);
        FIRST_OBSERVED.store(1, Ordering::Release);
        assert!(
            syscall(
                libc::SYS_futex,
                [(&raw const FIRST_OBSERVED) as u64, 129, 1, 0, 0, 0]
            ) >= 0
        );
    }
    if observation.reenter {
        let mut nested_stack = vec![0u8; 1024 * 1024];
        let base = nested_stack.as_mut_ptr() as usize;
        let top = (base + nested_stack.len()) & !15;
        let unreadable = UnreadableErrno::new();
        state.signal_stack = base..base + nested_stack.len();
        state.errno = unreadable.0.cast();
        PHASE.with(|slot| slot.store(CAPTURED, Ordering::Release));
        assert!(enter_runtime());
        eprintln!("outer production capture observed; nested phase is RUNTIME");
        unsafe { call_production_entry(top, 0) };
        panic!("production entry accepted same-thread reentry");
    }
    Some(Continuation::GUEST)
}

const CONTROL_CHILD: &str = "LITEINST_OWNED_PRODUCTION_CAPTURE_CHILD";

fn run_capture_child(test: &str, mode: &str) -> std::process::Output {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("owned_context::ownership_tests::{test}"),
            "--nocapture",
        ])
        .env(CONTROL_CHILD, mode)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    println!(
        "CONTROL {mode}: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !timed_out,
        "capture child exceeded 20-second host control bound"
    );
    output
}

fn bound_child_core_files() {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
}

fn assert_capture_refusal(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(126), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("owned-context") && stderr.contains("predicate"),
        "{stderr}"
    );
}

#[test]
fn production_entry_preserves_overlapping_thread_activations() {
    if std::env::var_os(CONTROL_CHILD).is_none() {
        let output = run_capture_child(
            "production_entry_preserves_overlapping_thread_activations",
            "overlap",
        );
        assert!(output.status.success(), "{output:?}");
        return;
    }
    bound_child_core_files();
    unsafe { reverie_preload::clock_boundary::register_signal_scope(&OVERLAP_SCOPE).unwrap() };
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let fixture = CaptureFixture::new();
            FIRST_TID.store(fixture.installed.state.owner, Ordering::Release);
            fixture.invoke(0);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while FIRST_ENTERED.load(Ordering::Acquire) == 0 {
            assert!(
                Instant::now() < deadline,
                "first production entry never reached its scope"
            );
            std::thread::yield_now();
        }
        let second = scope.spawn(|| CaptureFixture::new().invoke(0));
        first.join().unwrap();
        second.join().unwrap();
    });
    assert_eq!(FIRST_OBSERVED.load(Ordering::Acquire), 1);
}

#[test]
fn production_capture_refuses_identity_and_phase_before_errno() {
    let Ok(mode) = std::env::var(CONTROL_CHILD) else {
        for mode in ["phase", "owner", "frame", "valid", "valid-unreadable"] {
            let output = run_capture_child(
                "production_capture_refuses_identity_and_phase_before_errno",
                mode,
            );
            match mode {
                "valid" => assert!(output.status.success(), "{output:?}"),
                "valid-unreadable" => {
                    assert_eq!(output.status.signal(), Some(libc::SIGSEGV), "{output:?}")
                }
                _ => assert_capture_refusal(&output),
            }
        }
        return;
    };
    bound_child_core_files();
    let mut fixture = CaptureFixture::new();
    let unreadable = UnreadableErrno::new();
    if mode != "valid" {
        fixture.installed.state.errno = unreadable.0.cast();
    }
    match mode.as_str() {
        "phase" => PHASE.with(|slot| slot.store(CAPTURED, Ordering::Release)),
        "owner" => fixture.installed.state.owner += 1,
        "frame" | "valid" | "valid-unreadable" => (),
        _ => panic!("unknown capture control {mode}"),
    }
    fixture.invoke(usize::from(mode == "frame") * 8);
}

#[test]
fn production_entry_refuses_same_thread_reentry() {
    if std::env::var_os(CONTROL_CHILD).is_none() {
        let output = run_capture_child("production_entry_refuses_same_thread_reentry", "reentry");
        assert_capture_refusal(&output);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("outer production capture observed; nested phase is RUNTIME")
        );
        return;
    }
    bound_child_core_files();
    let fixture = CaptureFixture::new();
    OBSERVATION.with(|slot| {
        slot.set(Some(CaptureObservation {
            reenter: true,
            ..slot.get().unwrap()
        }))
    });
    fixture.invoke(0);
}
