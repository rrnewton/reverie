// Native execution controls for trampoline tails. Every run lives in a fresh,
// bounded child so a machine-code fault remains an actual failing process.
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use iced_x86::BlockEncoder;
use iced_x86::BlockEncoderOptions;
use iced_x86::InstructionBlock;
use liteinst2::scanner;
use liteinst2::trampoline::ExecutableTrampoline;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::TrampolinePlan;
const SITE: u64 = 0x180000000;
const TARGET: u64 = 0x110000000;
const PAGE: usize = 4096;
static HOOKS: AtomicU64 = AtomicU64::new(0);
static FAULT_FD: AtomicI32 = AtomicI32::new(-1);
unsafe extern "C" fn hook(_: *mut HookContext) {
    HOOKS.fetch_add(1, Ordering::Relaxed);
}
// The observation handler does not recover. SA_RESETHAND restores the default
// action, and the same signal is queued for default termination on handler exit.
unsafe extern "C" fn fault(sig: i32, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    unsafe {
        let c = &*(ctx as *const libc::ucontext_t);
        let row = [
            0x4641525441494c31u64,
            sig as u64,
            (*info).si_code as u64,
            c.uc_mcontext.gregs[libc::REG_RIP as usize] as u64,
            c.uc_mcontext.gregs[libc::REG_RSP as usize] as u64,
            c.uc_mcontext.gregs[libc::REG_RAX as usize] as u64,
            (*info).si_addr() as u64,
            HOOKS.load(Ordering::Relaxed),
        ];
        libc::write(FAULT_FD.load(Ordering::Relaxed), row.as_ptr().cast(), 64);
        libc::syscall(
            libc::SYS_tgkill,
            libc::getpid(),
            libc::syscall(libc::SYS_gettid),
            sig,
        );
    }
}
core::arch::global_asm!(
    r#"
.text
.globl execute_control
.type execute_control,@function
execute_control:
    endbr64
    mov r11, rdi
    mov rdi, rsi
    xor eax, eax
    sub rsp, 8
    .byte 0x3e
    call r11
    add rsp, 8
    ret
.size execute_control, .-execute_control
"#
);
unsafe extern "C" {
    fn execute_control(entry: u64, argument: u64) -> u64;
}
unsafe fn code_mapping(address: u64, bytes: &[u8]) {
    unsafe {
        let p = libc::mmap(
            address as *mut _,
            PAGE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        );
        assert_ne!(
            p,
            libc::MAP_FAILED,
            "fixed mapping failed at {address:#x}: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(p as u64, address);
        assert!(bytes.len() < PAGE);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast(), bytes.len());
        assert_eq!(
            libc::mprotect(p, PAGE, libc::PROT_READ | libc::PROT_EXEC),
            0
        );
    }
}
fn disp(from_end: u64, to: u64) -> [u8; 4] {
    i32::try_from(i128::from(to) - i128::from(from_end))
        .unwrap()
        .to_le_bytes()
}
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn write_bytes(dir: &Path, name: &str, address: u64, bytes: &[u8]) {
    std::fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    std::fs::write(
        dir.join(format!("{name}.address")),
        format!("{address:#x}\n"),
    )
    .unwrap();
    std::fs::write(dir.join(format!("{name}.hex")), format!("{}\n", hex(bytes))).unwrap();
}
pub fn run(kind: &str, placement: &str, argument: u64, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    assert!(kind == "jcc" || kind == "call");
    assert!(argument <= 1);
    let pkru = unsafe {
        let c = core::arch::x86_64::__cpuid_count(7, 0);
        if c.ecx & (1 << 4) != 0 {
            core::arch::asm!("wrpkru",in("eax")0u32,in("ecx")0u32,in("edx")0u32,options(nostack));
            let v: u32;
            core::arch::asm!("rdpkru",in("ecx")0u32,out("eax")v,out("edx")_,options(nostack));
            assert_eq!(v, 0);
            Some(v)
        } else {
            None
        }
    };
    let fault_file = std::fs::File::create(dir.join("fault.bin")).unwrap();
    FAULT_FD.store(fault_file.as_raw_fd(), Ordering::Relaxed);
    unsafe {
        for sig in [libc::SIGTRAP, libc::SIGSEGV, libc::SIGILL, libc::SIGBUS] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = fault as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESETHAND;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(libc::sigaction(sig, &action, std::ptr::null_mut()), 0);
        }
    }
    let return_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join("observed-call-return.bin"))
        .unwrap();
    return_file.set_len(PAGE as u64).unwrap();
    let return_map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            PAGE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            return_file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(return_map, libc::MAP_FAILED);
    let mut source = Vec::new();
    let mut destination = vec![0xf3, 0x0f, 0x1e, 0xfa];
    let expected = if kind == "jcc" {
        source.extend_from_slice(&[0x85, 0xff, 0x0f, 0x84]);
        source.extend_from_slice(&disp(SITE + 8, TARGET));
        source.extend_from_slice(&[0xb8, 17, 0, 0, 0, 0xc3]);
        destination.extend_from_slice(&[0xb8, 34, 0, 0, 0, 0xc3]);
        if argument == 0 { 34 } else { 17 }
    } else {
        source.push(0xe8);
        source.extend_from_slice(&disp(SITE + 5, TARGET));
        source.extend_from_slice(&[0x83, 0xc0, 1, 0xc3]);
        destination.extend_from_slice(&[0x48, 0x8b, 0x04, 0x24, 0x48, 0xba]);
        destination.extend_from_slice(&(return_map as u64).to_le_bytes());
        destination.extend_from_slice(&[0x48, 0x89, 0x02, 0xb8, 68, 0, 0, 0, 0xc3]);
        69
    };
    unsafe {
        code_mapping(SITE, &source);
        code_mapping(TARGET, &destination);
    }
    write_bytes(dir, "original", SITE, &source);
    write_bytes(dir, "target", TARGET, &destination);
    let scan = scanner::InstructionScanner::default()
        .scan(&source, SITE)
        .unwrap();
    let plan = TrampolinePlan::from_scan(&scan, SITE, hook).unwrap();
    assert_eq!(plan.displaced_len(), if kind == "jcc" { 8 } else { 5 });
    let address = match placement {
        "direct" => SITE,
        "near" => SITE + 0x100000,
        "far-near-return" => SITE + 0x70000000,
        "far-far-return" => 0x400000000,
        _ => panic!("unknown placement"),
    };
    let mut metadata = format!(
        "kind={kind}\nplacement={placement}\nargument={argument}\nexpected={expected}\nsite={SITE:#x}\ntarget={TARGET:#x}\nentry={address:#x}\npkru={pkru:?}\ndisplaced={}\noriginal_return={:#x}\n",
        plan.displaced_len(),
        plan.return_address()
    );
    let _allocation = if placement != "direct" {
        let image = plan.emit_at(address).unwrap();
        let layout = image.layout();
        let start = layout.instrumentation_len + layout.restore_len;
        let end = start + layout.relocated_len;
        let instructions: Vec<_> = scan
            .instructions()
            .iter()
            .take_while(|i| i.address() < plan.return_address())
            .map(|i| *i.instruction())
            .collect();
        let block = BlockEncoder::encode(
            64,
            InstructionBlock::new(&instructions, address + start as u64),
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS
                | BlockEncoderOptions::RETURN_RELOC_INFOS,
        )
        .unwrap();
        // Near blocks must retain the encoder's original byte-for-byte layout.
        // Far blocks must execute correctly even when the encoder needs literals.
        // Do not require either implementation's particular literal-table layout.
        if placement == "near" {
            assert_eq!(block.code_buffer, &image.bytes()[start..end]);
        }
        metadata.push_str(&format!("instrumentation_len={}\nrestore_len={}\nrelocated_len={}\nreturn_len={}\nrelocated_start={:#x}\nreturn_jump={:#x}\n",layout.instrumentation_len,layout.restore_len,layout.relocated_len,layout.return_len,address+start as u64,address+end as u64));
        metadata.push_str(&format!(
            "unmodified_block_reloc_infos={:?}\nunmodified_block_instruction_offsets={:?}\n",
            block.reloc_infos, block.new_instruction_offsets
        ));
        for mapping in image.program_counter_mappings() {
            metadata.push_str(&format!(
                "pc_mapping={:#x}..{:#x}=>{:#x}\n",
                mapping.generated_start(),
                mapping.generated_end(),
                mapping.logical_address()
            ));
        }
        if placement == "near" {
            assert!(block.reloc_infos.is_empty());
            assert_eq!(layout.return_len, 5);
        } else {
            assert_eq!(block.reloc_infos.len(), 1);
            assert_eq!(
                layout.return_len,
                if placement == "far-near-return" {
                    5
                } else {
                    15
                }
            );
        }
        write_bytes(dir, "trampoline", address, image.bytes());
        write_bytes(
            dir,
            "relocated",
            address + start as u64,
            &image.bytes()[start..end],
        );
        write_bytes(dir, "return", address + end as u64, &image.bytes()[end..]);
        let executable = ExecutableTrampoline::allocate_at(&plan, address).unwrap();
        let actual =
            unsafe { std::slice::from_raw_parts(address as *const u8, image.bytes().len()) };
        assert_eq!(actual, image.bytes());
        Some(executable)
    } else {
        None
    };
    std::fs::write(dir.join("layout.txt"), metadata).unwrap();
    std::fs::write(
        dir.join("maps.txt"),
        std::fs::read("/proc/self/maps").unwrap(),
    )
    .unwrap();
    println!(
        "executing kind={kind} placement={placement} arg={argument} expected={expected} pkru={pkru:?}"
    );
    let actual = unsafe { execute_control(address, argument) };
    let hooks = HOOKS.load(Ordering::Relaxed);
    let ret = unsafe { std::ptr::read_volatile(return_map.cast::<u64>()) };
    println!("returned actual={actual} hooks={hooks} observed_call_return={ret:#x}");
    assert_eq!(actual, expected, "native/relocated result mismatch");
    assert_eq!(hooks, u64::from(placement != "direct"));
    if kind == "call" {
        let expected_return = if let Some(allocation) = &_allocation {
            let layout = allocation.layout();
            address
                + (layout.instrumentation_len + layout.restore_len) as u64
                + if placement == "near" { 5 } else { 6 }
        } else {
            SITE + 5
        };
        // Relocation changes the physical CALL return PC. This test preserves
        // that existing contract and checks that execution resumes successfully.
        assert_eq!(ret, expected_return);
        if let Some(allocation) = &_allocation {
            assert_eq!(
                allocation
                    .program_counter_mappings()
                    .iter()
                    .find_map(|mapping| mapping.translate(ret)),
                Some(plan.return_address())
            );
        }
    }
}
