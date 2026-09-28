// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/terminal_runtime_tests.rs lines 164..189; full file SHA256 15a483061017a4b13366695a7a507ee70f7ff6aad13ff4d5801b17953894b8a7
// These are the exact context predicates, so this control needs no KVM device.
// Returning ordinary injections remain supported; only a terminal operation
// can tail out of a timestamp callback without a syscall return transport.
#[test]
fn timestamp_exit_admission_preserves_returning_ordinary_injections() {
    let context = ProcessExecutionContext::Instruction;
    for (number, tail, ordinary) in [
        (libc::SYS_exit, true, true),
        (libc::SYS_exit_group, true, true),
        (libc::SYS_write, false, true),
        (libc::SYS_execve, false, false),
        (libc::SYS_fork, false, false),
    ] {
        let request = SyscallRequest::new(number as u64, [29, 0x100, 3, 0, 0, 0]);
        assert_eq!(
            context.tail_injection_allowed(&request),
            tail,
            "syscall={number}"
        );
        assert_eq!(
            context.ordinary_injection_allowed(&request),
            ordinary,
            "syscall={number}"
        );
    }
}

