from pathlib import Path
import shutil
N=Path(__file__).resolve().parent
P=N.parent/'rdtsc-recovery-source-v3'
shutil.copytree(P/'source',N/'source',symlinks=True)
p=N/'source/reverie-kvm/tests/static_elf.rs'
s=p.read_text()
start=s.index('#[test]\nfn host_owned_timestamp_worker_keeps_native_execution()')
end=s.index('\nfn prefixed_timestamp_register_program',start)
old=s[start:end]
assert old.count('fn ')==1
helper='''fn host_owned_timestamp_program() -> Vec<u8> {
    fn append_root_read(code: &mut Vec<u8>, failures: &mut Vec<usize>) {
        code.extend_from_slice(&[0x0f, 0x31]); // rdtsc
        code.push(0x3d); // cmp eax, expected low word
        code.extend_from_slice(&(RDTSC_SENTINEL as u32).to_le_bytes());
        append_jne_failure(code, failures);
        code.extend_from_slice(&[0x81, 0xfa]); // cmp edx, expected high word
        code.extend_from_slice(&((RDTSC_SENTINEL >> 32) as u32).to_le_bytes());
        append_jne_failure(code, failures);
    }

    const CHILD_TID: u64 = LOAD_ADDRESS + 0x1800;
    const CHILD_RESULT: u64 = LOAD_ADDRESS + 0x1808;
    const CHILD_DONE: u64 = LOAD_ADDRESS + 0x1810;
    const CHILD_STACK: u64 = LOAD_ADDRESS + 0x1900;
    const CHILD_STACK_SIZE: u64 = 0x600;
    let flags = libc::CLONE_VM as u64
        | libc::CLONE_FS as u64
        | libc::CLONE_FILES as u64
        | libc::CLONE_SIGHAND as u64
        | libc::CLONE_THREAD as u64
        | libc::CLONE_SYSVSEM as u64
        | libc::CLONE_CHILD_SETTID as u64
        | libc::CLONE_CHILD_CLEARTID as u64;
    let mut code = Vec::new();
    let mut failures = Vec::new();
    append_root_read(&mut code, &mut failures);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[
        0xc7, 0x01, 0xff, 0xff, 0xff, 0x7f, // mov [rcx], nonzero sentinel
        0xb8, 0xb3, 0x01, 0x00, 0x00, // mov eax, SYS_clone3
        0x48, 0xbf, // movabs rdi, clone_args
    ]);
    let clone_args_operand = code.len();
    code.extend_from_slice(&0_u64.to_le_bytes());
    code.extend_from_slice(&[
        0xbe, 0x58, 0x00, 0x00, 0x00, // mov esi, sizeof(clone_args)
        0x0f, 0x05, // syscall
        0x85, 0xc0, // test eax, eax
        0x0f, 0x84, 0, 0, 0, 0, // jz child
    ]);
    let child_jump = code.len() - 4;
    code.extend_from_slice(&[0x0f, 0x88]); // js failure: clone must succeed
    failures.push(code.len());
    code.extend_from_slice(&0_i32.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x89, 0xc5]); // mov r13d, returned child tid
    code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    let wait = code.len();
    code.extend_from_slice(&[
        0x8b, 0x17, // mov edx, [rdi]
        0x85, 0xd2, // test edx, edx
        0x0f, 0x84, 0, 0, 0, 0, // jz joined
    ]);
    let joined_jump = code.len() - 4;
    code.extend_from_slice(&[
        0x31, 0xf6, // xor esi, esi: FUTEX_WAIT, current nonzero value in edx
        0x45, 0x31, 0xd2, // xor r10d, r10d: no timeout
        0xb8, 0xca, 0x00, 0x00, 0x00, // mov eax, SYS_futex
        0x0f, 0x05, // syscall
    ]);
    // A racing clear-TID may win before FUTEX_WAIT. Only Linux's normal
    // successful wake, changed-value refusal or signal interruption can retry.
    for result in [0_i32, -libc::EAGAIN, -libc::EINTR] {
        code.push(0x3d); // cmp eax, result
        code.extend_from_slice(&result.to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x84, 0, 0, 0, 0]); // je wait
        let operand = code.len() - 4;
        patch_stats_jump(&mut code, operand, wait);
    }
    code.push(0xe9); // jmp failure for any other futex result
    failures.push(code.len());
    code.extend_from_slice(&0_i32.to_le_bytes());
    let joined = code.len();
    patch_stats_jump(&mut code, joined_jump, joined);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_result
    code.extend_from_slice(&CHILD_RESULT.to_le_bytes());
    code.extend_from_slice(&[0x44, 0x39, 0x29]); // cmp [rcx], r13d
    append_jne_failure(&mut code, &mut failures);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_done
    code.extend_from_slice(&CHILD_DONE.to_le_bytes());
    code.extend_from_slice(&[0x81, 0x39, 0x34, 0x12, 0x00, 0x00]); // cmp [rcx], 0x1234
    append_jne_failure(&mut code, &mut failures);
    append_root_read(&mut code, &mut failures);
    append_stats_exit(&mut code, true);

    let child = code.len();
    patch_stats_jump(&mut code, child_jump, child);
    code.extend_from_slice(&[
        0x0f, 0x31, // actual Host-worker RDTSC: must retire without a Tool callback
        0xb8, 0xba, 0x00, 0x00, 0x00, // mov eax, SYS_gettid
        0x0f, 0x05, // syscall
        0x48, 0xb9, // movabs rcx, child_result
    ]);
    code.extend_from_slice(&CHILD_RESULT.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x01]); // mov [rcx], eax
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_done
    code.extend_from_slice(&CHILD_DONE.to_le_bytes());
    code.extend_from_slice(&[0xc7, 0x01, 0x34, 0x12, 0x00, 0x00]); // mov [rcx], 0x1234
    append_stats_exit(&mut code, false); // clear-TID and wake parent on real exit

    let failure = code.len();
    code.extend_from_slice(&[
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);
    for operand in failures {
        patch_stats_jump(&mut code, operand, failure);
    }
    while !code.len().is_multiple_of(8) {
        code.push(0);
    }
    let clone_args_address = LOAD_ADDRESS + code.len() as u64;
    code[clone_args_operand..clone_args_operand + 8]
        .copy_from_slice(&clone_args_address.to_le_bytes());
    let mut clone_args = [0_u8; 88];
    clone_args[0..8].copy_from_slice(&flags.to_le_bytes());
    clone_args[16..24].copy_from_slice(&CHILD_TID.to_le_bytes());
    clone_args[40..48].copy_from_slice(&CHILD_STACK.to_le_bytes());
    clone_args[48..56].copy_from_slice(&CHILD_STACK_SIZE.to_le_bytes());
    code.extend_from_slice(&clone_args);
    assert!(code.len() < 0x1000, "code must not overlap the shared data page");
    code
}

'''
new='''#[test]
fn host_owned_timestamp_worker_keeps_native_execution() {
    if !kvm_available("KVM Host-owned timestamp worker test") {
        return;
    }
    // No dynamic loader: only the two root instructions belong to this exact
    // callback oracle. The worker must execute its own RDTSC, publish its TID
    // and completion marker, then clear child_tid before the root can finish.
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend.set_thread_ownership(ThreadOwnership::Host);
    backend
        .install_static_elf(
            &static_elf(&host_owned_timestamp_program()),
            "/bin/timestamp-host-worker",
        )
        .unwrap();
'''+old[old.index('    let (log, status, stdout, stderr)'):]
p.write_text(s[:start]+helper+new+s[end:])
(N/'OLD-PTHREAD-FIXTURE.rs').write_text(old)
(N/'NEW-HOST-FIXTURE.rs').write_text(helper+new)
print('Prepared only',p)
