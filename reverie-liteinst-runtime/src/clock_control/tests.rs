use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::ExecutableTrampoline;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::TrampolinePlan;

static OBSERVED_RETURN: AtomicU64 = AtomicU64::new(0);

#[unsafe(naked)]
unsafe extern "C" fn capture_return(_context: *mut HookContext) {
    core::arch::naked_asm!(
        "mov rax, [rsp]",
        "mov [rip + {observed}], rax",
        "ret",
        observed = sym OBSERVED_RETURN,
    );
}

#[test]
fn generated_callback_return_is_restore_start_not_guest_tail() {
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let code = [0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xc3];
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), mapping.cast(), code.len()) };
    assert_eq!(
        unsafe { libc::mprotect(mapping, 4096, libc::PROT_READ | libc::PROT_EXEC) },
        0
    );
    let address = mapping as u64;
    let scan = InstructionScanner::default()
        .scan_prefix(&code, address, 5)
        .unwrap();
    let plan = TrampolinePlan::from_scan(&scan, address, capture_return).unwrap();
    let trampoline = ExecutableTrampoline::allocate(&plan).unwrap();
    let witness = super::callback_return_pc(&trampoline).unwrap();
    let invoke: unsafe extern "C" fn() = unsafe { std::mem::transmute(trampoline.address()) };
    unsafe { invoke() };
    assert_eq!(OBSERVED_RETURN.load(Ordering::Relaxed), witness);
    assert_eq!(
        witness,
        trampoline.address() + trampoline.layout().instrumentation_len as u64
    );
    assert_eq!(
        trampoline.relocated_tail_address(),
        witness + trampoline.layout().restore_len as u64
    );
    assert_ne!(witness, trampoline.relocated_tail_address());
    println!(
        "live-return={witness:#x} restore-start={witness:#x} guest-tail={:#x}",
        trampoline.relocated_tail_address()
    );
    assert_eq!(unsafe { libc::munmap(mapping, 4096) }, 0);
}

static DISABLE_CALLS: AtomicU64 = AtomicU64::new(0);
static ENABLE_CALLS: AtomicU64 = AtomicU64::new(0);

#[unsafe(naked)]
unsafe extern "C" fn mock_gate() {
    core::arch::naked_asm!(
        "cmp eax, 186",
        "jne 2f",
        "mov eax, 7",
        "ret",
        "2:",
        "cmp eax, 16",
        "jne 5f",
        "cmp esi, 0x2401",
        "jne 3f",
        "lock inc qword ptr [rip + {disable}]",
        "xor eax, eax",
        "ret",
        "3:",
        "cmp esi, 0x2400",
        "jne 5f",
        "lock inc qword ptr [rip + {enable}]",
        "xor eax, eax",
        "ret",
        "5:",
        "mov rax, -22",
        "ret",
        disable = sym DISABLE_CALLS,
        enable = sym ENABLE_CALLS,
    );
}

#[test]
fn paused_nested_and_transferred_ownership_use_exact_controls() {
    std::thread::spawn(|| {
        unsafe extern "C" {
            fn reverie_liteinst_domain_depth() -> u64;
        }
        let state = super::control();
        let enter = super::reverie_liteinst_clock_enter;
        let leave = super::reverie_liteinst_clock_leave;
        state
            .gate
            .store(mock_gate as *const () as u64, Ordering::Relaxed);
        state.owner.store(7, Ordering::Relaxed);
        state.ready.store(1, Ordering::Release);
        let counts = || {
            (
                DISABLE_CALLS.load(Ordering::Relaxed),
                ENABLE_CALLS.load(Ordering::Relaxed),
            )
        };

        let outer = unsafe { enter(0) };
        let inner = unsafe { enter(0) };
        assert_eq!((outer, inner), (0, 0));
        assert_eq!(unsafe { reverie_liteinst_domain_depth() }, 2);
        unsafe {
            leave(inner, 1, 0);
            leave(outer, 1, 0)
        };
        assert_eq!(counts(), (0, 0));
        assert_eq!(state.running.load(Ordering::Relaxed), 0);

        state.running.store(1, Ordering::Relaxed);
        let outer = unsafe { enter(0) };
        let inner = unsafe { enter(0) };
        assert_eq!((outer, inner), (1, 0));
        assert_eq!(counts(), (1, 0));
        unsafe { leave(inner, 1, 0) };
        assert_eq!(state.running.load(Ordering::Relaxed), 0);
        unsafe { leave(outer, 0, 0) };
        assert_eq!(counts(), (1, 1));
        assert_eq!(state.running.load(Ordering::Relaxed), 1);
        assert_eq!(unsafe { reverie_liteinst_domain_depth() }, 0);

        let outer = unsafe { enter(0) };
        unsafe { leave(outer, 2, 0x1234) };
        assert_eq!(unsafe { reverie_liteinst_domain_depth() }, 1);
        let unrelated = unsafe { enter(0x5678) };
        let nested_match = unsafe { enter(0x1234) };
        assert_eq!((unrelated, nested_match), (0, 0));
        assert_eq!(state.pending_token.load(Ordering::Relaxed), 1);
        unsafe {
            leave(nested_match, 1, 0);
            leave(unrelated, 1, 0)
        };
        assert_eq!(counts(), (2, 1));
        let adopted = unsafe { enter(0x1234) };
        assert_eq!(adopted, 1);
        assert_eq!(unsafe { reverie_liteinst_domain_depth() }, 1);
        assert_eq!(state.pending_token.load(Ordering::Relaxed), 0);
        assert_eq!(state.pending_witness.load(Ordering::Relaxed), 0);
        unsafe { leave(adopted, 0, 0) };
        assert_eq!(counts(), (2, 2));

        state.running.store(2, Ordering::Relaxed);
        let stopping = unsafe { enter(0) };
        assert_eq!(stopping, 0);
        assert_eq!(state.running.load(Ordering::Relaxed), 0);
        unsafe { leave(stopping, 1, 0) };
        assert_eq!(counts(), (3, 2));
        assert_eq!(unsafe { reverie_liteinst_domain_depth() }, 0);
        state.ready.store(0, Ordering::Release);
    })
    .join()
    .unwrap();
}
