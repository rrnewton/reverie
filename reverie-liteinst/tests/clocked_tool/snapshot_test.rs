use std::arch::global_asm;

global_asm!(
    include_str!("snapshot_redzone.s"),
    r#"
    .text
    .global snapshot_test_capture
    .hidden snapshot_test_capture
    .type snapshot_test_capture,@function
snapshot_test_capture:
    sub rsp, 136
    push 0x247
    popfq
    .irp offset,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128
    mov qword ptr [rsp - \offset], 123456
    .endr
    mov [rsp - 8], rsi
    mov rax, 0x123456789abcdef
    mov [rdi + 136], rax
    snapshot_redzone_and_flags "rdi", "rdi + 128"
    mov rax, [rdi + 136]
    lea rsp, [rsp + 136]
    ret
    .size snapshot_test_capture, .-snapshot_test_capture
"#
);

#[derive(Default)]
#[repr(C)]
struct Snapshot {
    redzone: [u64; 16],
    flags: u64,
    original_rax: u64,
}

unsafe extern "C" {
    fn snapshot_test_capture(output: *mut Snapshot, sentinel: u64) -> u64;
}

fn capture(sentinel: u64) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let returned_rax = unsafe { snapshot_test_capture(&mut snapshot, sentinel) };
    assert_eq!(snapshot.original_rax, 0x123456789abcdef);
    assert_eq!(returned_rax, snapshot.original_rax);
    assert_eq!(snapshot.flags, 0x247);
    snapshot
}

#[test]
fn captures_original_top_word_not_flags() {
    let sentinel = 0x13579bdf2468ace0;
    let snapshot = capture(sentinel);
    assert_eq!(snapshot.redzone[0], sentinel);
    assert_eq!(snapshot.redzone[1..], [123456; 15]);
    assert_ne!(snapshot.redzone[0], snapshot.flags);
}

#[test]
fn exact_comparison_rejects_changed_top_word() {
    let original = capture(0x13579bdf2468ace0);
    let changed = capture(0x13579bdf2468ace1);
    assert_eq!(original.redzone[1..], changed.redzone[1..]);
    assert!(std::panic::catch_unwind(|| assert_eq!(original.redzone, changed.redzone)).is_err());
}
