fn fionread_contract_call(executor: &mut ElfExecutor, memory: &GuestMemory, args: [u64; 6]) -> i64 {
    let raw = SyscallRequest::new(libc::SYS_ioctl as u64, args);
    let decoded = raw.into_syscall().unwrap();
    let transported = SyscallRequest::from_syscall(decoded);
    assert_eq!(transported, raw);
    executor.execute(&transported, memory)
}

#[test]
fn fionread_contract_full_bytes_and_fault_controls() {
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    state
        .files
        .insert(10, std::fs::File::open("/dev/null").unwrap());
    let mut executor = ElfExecutor::new(state, false);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    let before = vec![0xa5; 2 * PAGE_SIZE as usize];
    memory.write(0, &before).unwrap();
    for descriptor in [9, 9 + (1_u64 << 32)] {
        memory.write(0, &before).unwrap();
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [descriptor, libc::FIONREAD, 0x104, 0, 0, 0]
            ),
            0
        );
        let mut expected = before.clone();
        expected[0x104..0x108].copy_from_slice(&7_i32.to_ne_bytes());
        let mut actual = vec![0; before.len()];
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
    memory.write(0, &before).unwrap();
    memory.map_user_range(0, PAGE_SIZE, false).unwrap();
    memory.map_user_range(PAGE_SIZE, PAGE_SIZE, true).unwrap();
    memory.enable_user_access();
    for address in [PAGE_SIZE - 2, PAGE_SIZE, u64::MAX - 1] {
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [9, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::EFAULT)
        );
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [99, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::EBADF)
        );
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [10, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::ENOTTY)
        );
        let mut actual = vec![0; before.len()];
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, before);
    }
}

#[test]
fn fionread_contract_high_request_bits_match_native() {
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    let mut executor = ElfExecutor::new(state, false);
    let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [9, libc::FIONREAD | (1_u64 << 32), 0x100, 0, 0, 0]
        ),
        0
    );
    assert_eq!(read_struct::<i32>(&memory, 0x100), 7);
}

#[test]
fn fionread_contract_readonly_output_matches_native() {
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    let mut executor = ElfExecutor::new(state, false);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let before = vec![0xa5; PAGE_SIZE as usize];
    memory.write(0, &before).unwrap();
    memory.map_user_range(0, PAGE_SIZE, false).unwrap();
    memory.enable_user_access();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mprotect as u64,
                [0, PAGE_SIZE, libc::PROT_READ as u64, 0, 0, 0]
            ),
            &memory
        ),
        0
    );
    let result =
        fionread_contract_call(&mut executor, &memory, [9, libc::FIONREAD, 0x100, 0, 0, 0]);
    let mut actual = vec![0; before.len()];
    memory.read_raw(0, &mut actual).unwrap();
    eprintln!(
        "readonly result={result} changed_bytes={} output={:?}",
        actual
            .iter()
            .zip(&before)
            .filter(|(actual, before)| actual != before)
            .count(),
        &actual[0x100..0x104]
    );
    assert_eq!(result, negative_errno(libc::EFAULT));
    assert_eq!(actual, before);
}

#[test]
fn fionread_contract_synthetic_proc_matches_native() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    write_c_string(&mut memory, 0x100, "/proc/self/status");
    let descriptor = executor.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
        ),
        &memory,
    );
    assert!(descriptor >= 0);
    assert!(executor.state.proc_files.contains_key(&(descriptor as i32)));
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [descriptor as u64, libc::FIONREAD, 0x200, 0, 0, 0]
        ),
        0
    );
    assert_eq!(read_struct::<i32>(&memory, 0x200), 0);
}

#[test]
fn fionread_contract_protection_is_shared_but_snapshot_is_private() {
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    let mut executor = ElfExecutor::new(state, false);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    memory.map_user_range(0, 2 * PAGE_SIZE, false).unwrap();
    memory.enable_user_access();
    memory
        .write(0, &vec![0x5a; 2 * PAGE_SIZE as usize])
        .unwrap();
    let shared = memory.clone();
    let private = memory.snapshot().unwrap();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mprotect as u64,
                [0, PAGE_SIZE, libc::PROT_READ as u64, 0, 0, 0]
            ),
            &memory
        ),
        0
    );
    assert_eq!(
        fionread_contract_call(&mut executor, &shared, [9, libc::FIONREAD, 0x100, 0, 0, 0]),
        negative_errno(libc::EFAULT)
    );
    assert_eq!(
        fionread_contract_call(&mut executor, &private, [9, libc::FIONREAD, 0x100, 0, 0, 0]),
        0
    );
    let mut actual = vec![0; 2 * PAGE_SIZE as usize];
    memory.read_raw(0, &mut actual).unwrap();
    assert_eq!(actual, vec![0x5a; actual.len()]);
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mprotect as u64,
                [
                    0,
                    PAGE_SIZE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0
                ]
            ),
            &shared
        ),
        0
    );
    assert_eq!(
        fionread_contract_call(&mut executor, &memory, [9, libc::FIONREAD, 0x100, 0, 0, 0]),
        0
    );
}

#[test]
fn fionread_contract_mapping_remap_and_reuse_preserve_permissions() {
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    let memory_size = BOOT_RESERVED_END + 32 * PAGE_SIZE;
    state.mmap_next = BOOT_RESERVED_END + PAGE_SIZE;
    state.mmap_limit = memory_size;
    let mut executor = ElfExecutor::new(state, false);
    let memory = GuestMemory::new(0, memory_size as usize).unwrap();
    let mapping = executor.execute(
        &SyscallRequest::new(
            libc::SYS_mmap as u64,
            [
                0,
                2 * PAGE_SIZE,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
        ),
        &memory,
    );
    assert!(mapping > 0);
    memory.enable_user_access();
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [9, libc::FIONREAD, mapping as u64, 0, 0, 0]
        ),
        negative_errno(libc::EFAULT)
    );
    let destination = mapping as u64 + 8 * PAGE_SIZE;
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mremap as u64,
                [
                    mapping as u64,
                    2 * PAGE_SIZE,
                    3 * PAGE_SIZE,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                    destination,
                    0
                ]
            ),
            &memory
        ),
        destination as i64
    );
    for address in [mapping as u64, destination, destination + 2 * PAGE_SIZE] {
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [9, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::EFAULT)
        );
    }
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mremap as u64,
                [destination, 3 * PAGE_SIZE, PAGE_SIZE, 0, 0, 0]
            ),
            &memory
        ),
        destination as i64
    );
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [9, libc::FIONREAD, destination + PAGE_SIZE, 0, 0, 0]
        ),
        negative_errno(libc::EFAULT)
    );
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_munmap as u64,
                [destination, PAGE_SIZE, 0, 0, 0, 0]
            ),
            &memory
        ),
        0
    );
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mmap as u64,
                [
                    destination,
                    PAGE_SIZE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                    u64::MAX,
                    0
                ]
            ),
            &memory
        ),
        destination as i64
    );
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [9, libc::FIONREAD, destination, 0, 0, 0]
        ),
        0
    );
}

#[test]
fn fionread_contract_proc_offsets_follow_descriptor_aliases() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    write_c_string(&mut memory, 0x100, "/proc/self/status");
    let descriptor = executor.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
        ),
        &memory,
    );
    assert!(descriptor >= 0);
    let alias = executor.execute(
        &SyscallRequest::new(libc::SYS_dup as u64, [descriptor as u64, 0, 0, 0, 0, 0]),
        &memory,
    );
    assert!(alias >= 0);
    for position in [0, 1, 7, 17] {
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_lseek as u64,
                    [alias as u64, position, libc::SEEK_SET as u64, 0, 0, 0]
                ),
                &memory
            ),
            position as i64
        );
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [descriptor as u64, libc::FIONREAD, 0x200, 0, 0, 0]
            ),
            0
        );
        assert_eq!(read_struct::<i32>(&memory, 0x200), -(position as i32));
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_lseek as u64,
                    [descriptor as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]
                ),
                &memory
            ),
            position as i64
        );
    }
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(libc::SYS_close as u64, [descriptor as u64, 0, 0, 0, 0, 0]),
            &memory
        ),
        0
    );
    assert_eq!(
        fionread_contract_call(
            &mut executor,
            &memory,
            [alias as u64, libc::FIONREAD, 0x200, 0, 0, 0]
        ),
        0
    );
    assert_eq!(read_struct::<i32>(&memory, 0x200), -17);
}

#[test]
fn fionread_contract_readonly_boundaries_preserve_bytes_and_error_order() {
    use reverie::syscalls::Addr;
    use reverie::syscalls::AddrMut;
    use reverie::syscalls::MemoryAccess;
    let root = TestDir::new();
    std::fs::write(root.0.join("payload"), b"abcdefg").unwrap();
    let mut state = test_state(&root.0);
    state
        .files
        .insert(9, std::fs::File::open(root.0.join("payload")).unwrap());
    state
        .files
        .insert(10, std::fs::File::open("/dev/null").unwrap());
    let mut executor = ElfExecutor::new(state, false);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    memory.map_user_range(0, 2 * PAGE_SIZE, false).unwrap();
    memory.enable_user_access();
    let before = vec![0xa5; 2 * PAGE_SIZE as usize];
    memory.write(0, &before).unwrap();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mprotect as u64,
                [PAGE_SIZE, PAGE_SIZE, libc::PROT_READ as u64, 0, 0, 0]
            ),
            &memory
        ),
        0
    );
    for address in [
        PAGE_SIZE - 3,
        PAGE_SIZE - 2,
        PAGE_SIZE - 1,
        PAGE_SIZE,
        PAGE_SIZE + 1,
    ] {
        for (descriptor, expected) in [(9, libc::EFAULT), (99, libc::EBADF), (10, libc::ENOTTY)] {
            assert_eq!(
                fionread_contract_call(
                    &mut executor,
                    &memory,
                    [descriptor, libc::FIONREAD, address, 0, 0, 0]
                ),
                negative_errno(expected)
            );
            let mut actual = vec![0; before.len()];
            memory.read_raw(0, &mut actual).unwrap();
            assert_eq!(actual, before);
        }
    }
    let mut copied = [0; 8];
    assert_eq!(
        MemoryAccess::read(
            &memory,
            Addr::from_raw(PAGE_SIZE as usize).unwrap(),
            &mut copied
        )
        .unwrap(),
        8
    );
    assert_eq!(copied, [0xa5; 8]);
    assert_eq!(
        MemoryAccess::write(
            &mut memory,
            AddrMut::from_raw(PAGE_SIZE as usize).unwrap(),
            &[0x5a; 8]
        )
        .unwrap(),
        8
    );
    memory.zero(PAGE_SIZE, 8).unwrap();
    memory.read_raw(PAGE_SIZE, &mut copied).unwrap();
    assert_eq!(copied, [0; 8]);
}

#[test]
fn fionread_contract_proc_directory_and_readonly_errors_preserve_bytes() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    memory.map_user_range(0, 2 * PAGE_SIZE, false).unwrap();
    memory.enable_user_access();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_mprotect as u64,
                [PAGE_SIZE, PAGE_SIZE, libc::PROT_READ as u64, 0, 0, 0]
            ),
            &memory
        ),
        0
    );
    for (path, expected) in [("/proc", libc::ENOTTY), ("/proc/self/status", libc::EFAULT)] {
        write_c_string(&mut memory, 0x100, path);
        let descriptor = executor.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
            ),
            &memory,
        );
        assert!(descriptor >= 0);
        memory
            .write(PAGE_SIZE, &vec![0x5a; PAGE_SIZE as usize])
            .unwrap();
        assert_eq!(
            fionread_contract_call(
                &mut executor,
                &memory,
                [descriptor as u64, libc::FIONREAD, PAGE_SIZE, 0, 0, 0]
            ),
            negative_errno(expected)
        );
        let mut actual = vec![0; PAGE_SIZE as usize];
        memory.read_raw(PAGE_SIZE, &mut actual).unwrap();
        assert_eq!(actual, vec![0x5a; PAGE_SIZE as usize]);
    }
}
