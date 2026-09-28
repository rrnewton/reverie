// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/tests/static_elf.rs lines 3649..3665; full file SHA256 b323b02a5fd334b40c857084e51c272720773e958fad38137aee90dd5c19d9de
fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

fn kvm_available(test: &str) -> bool {
    match Kvm::new() {
        Ok(_) => true,
        Err(error) if kvm_is_unavailable(&error) => {
            if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                panic!("{test} requires usable /dev/kvm: {error}");
            }
            eprintln!("skipping {test}: cannot open /dev/kvm: {error}");
            false
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/tests/static_elf.rs lines 7179..7181; full file SHA256 b323b02a5fd334b40c857084e51c272720773e958fad38137aee90dd5c19d9de
fn static_elf(code: &[u8]) -> Vec<u8> {
    static_elf_at(code, LOAD_ADDRESS)
}

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/tests/static_elf.rs lines 14008..14040; full file SHA256 b323b02a5fd334b40c857084e51c272720773e958fad38137aee90dd5c19d9de
#[path = "support/process_alarm_signals.rs"]
mod process_alarm_signals;

#[path = "support/parked_signals.rs"]
mod parked_signals;

#[path = "support/captured_write_signals.rs"]
mod captured_write_signals;

#[path = "support/timestamp_terminal.rs"]
mod timestamp_terminal;

#[path = "support/capture_identity.rs"]
mod capture_identity;

#[path = "support/cpuid_dispatch.rs"]
mod cpuid_dispatch;

#[path = "support/cpuid_terminal.rs"]
mod cpuid_terminal;

const RDTSC_SENTINEL: u64 = 0x1122_3344_5566_7788;
const RDTSCP_SENTINEL: u64 = 0x99aa_bbcc_ddee_ff00;
const RDTSCP_AUX_SENTINEL: u32 = 0x1357_9bdf;

#[derive(Debug, Default)]
struct TimestampLog {
    calls: Mutex<Vec<(Pid, Rdtsc)>>,
}

impl TimestampLog {
    fn calls(&self) -> Vec<(Pid, Rdtsc)> {
        self.calls

// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/tests/static_elf.rs lines 14102..14142; full file SHA256 b323b02a5fd334b40c857084e51c272720773e958fad38137aee90dd5c19d9de
fn timestamp_assertion_program() -> Vec<u8> {
    let mut code = Vec::new();
    let mut failure_patches = Vec::new();

    code.extend_from_slice(&[0x0f, 0x31]); // rdtsc
    code.push(0x3d); // cmp eax, imm32
    code.extend_from_slice(&(RDTSC_SENTINEL as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
    code.extend_from_slice(&((RDTSC_SENTINEL >> 32) as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);

    code.extend_from_slice(&[0x0f, 0x01, 0xf9]); // rdtscp
    code.push(0x3d); // cmp eax, imm32
    code.extend_from_slice(&(RDTSCP_SENTINEL as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, imm32
    code.extend_from_slice(&((RDTSCP_SENTINEL >> 32) as u32).to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);
    code.extend_from_slice(&[0x81, 0xf9]); // cmp ecx, imm32
    code.extend_from_slice(&RDTSCP_AUX_SENTINEL.to_le_bytes());
    append_jne_failure(&mut code, &mut failure_patches);

    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
    ]);
    let failure = code.len();
    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x0f, 0x05, // syscall
    ]);

    for patch in failure_patches {
        let displacement = i32::try_from(failure).unwrap() - i32::try_from(patch + 4).unwrap();
        code[patch..patch + 4].copy_from_slice(&displacement.to_le_bytes());
    }
    code
}

