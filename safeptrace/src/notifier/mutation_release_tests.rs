// Native original-child effects; no fabricated source stamp or owner transition.
fn mutation_register_words(r: &crate::Regs) -> [u64; 27] {
    [
        r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx, r.rdx,
        r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base, r.gs_base, r.ds,
        r.es, r.fs, r.gs,
    ]
}

#[test]
fn actual_register_effect_unwind_releases_original_mutation() {
    let (cleanup, stopped) = child_stop();
    let regs = stopped
        .getregs()
        .expect("original ptracer register capture");
    let mut actual = None;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _reservation = stopped.1.event().source_control(stopped.pid()).unwrap();
        let iov = libc::iovec {
            iov_base: (&regs as *const crate::Regs).cast_mut().cast(),
            iov_len: std::mem::size_of_val(&regs),
        };
        actual = Some(unsafe {
            libc::ptrace(
                libc::PTRACE_SETREGSET,
                stopped.pid().as_raw(),
                libc::NT_PRSTATUS,
                &iov,
            )
        });
        std::panic::panic_any("543 intentional unwind after actual SETREGSET");
    }));
    let after = stopped.setregs(&regs);
    let readback = stopped.getregs().map(|r| mutation_register_words(&r));
    let inactive = stopped.1.event().event().source.lock().mutation.is_none();
    let start = Instant::now();
    let settled = cleanup.cleanup();
    let elapsed = start.elapsed();
    eprintln!(
        "543 native unwind: actual={actual:?}, subsequent={after:?}, inactive={inactive}, cleanup={settled:?}, elapsed={elapsed:?}"
    );
    settled.expect("original native unwind child custody");
    assert!(elapsed <= Duration::from_secs(2));
    assert_eq!(actual, Some(0));
    assert_eq!(
        unwound.unwrap_err().downcast_ref::<&str>(),
        Some(&"543 intentional unwind after actual SETREGSET")
    );
    assert!(after.is_ok());
    assert_eq!(readback.unwrap(), mutation_register_words(&regs));
    assert!(inactive);
}

#[cfg(feature = "memory")]
#[test]
fn actual_write_fault_error_releases_original_mutation() {
    use reverie_memory::AddrMut;
    use reverie_memory::MemoryAccess;
    use reverie_memory::RemoteIoVec;
    let (cleanup, mut stopped) = child_stop();
    let regs = stopped
        .getregs()
        .expect("original ptracer register capture");
    let remote = RemoteIoVec::new(AddrMut::from_raw(1).unwrap(), 8).unwrap();
    let actual = stopped.write_native_user_vectored(
        stopped.pid().as_raw(),
        &[io::IoSlice::new(b"fault543")],
        &[remote],
    );
    let after = stopped.setregs(&regs);
    let readback = stopped.getregs().map(|r| mutation_register_words(&r));
    let inactive = stopped.1.event().event().source.lock().mutation.is_none();
    let start = Instant::now();
    let settled = cleanup.cleanup();
    let elapsed = start.elapsed();
    eprintln!(
        "543 native error: actual={actual:?}, subsequent={after:?}, inactive={inactive}, cleanup={settled:?}, elapsed={elapsed:?}"
    );
    settled.expect("original native error child custody");
    assert!(elapsed <= Duration::from_secs(2));
    assert_eq!(actual, Err(Errno::EFAULT));
    assert!(after.is_ok());
    assert_eq!(readback.unwrap(), mutation_register_words(&regs));
    assert!(inactive);
}
