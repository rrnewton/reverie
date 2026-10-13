// Source-only controls included inside the existing executor::tests module.
// They use public ab8 interfaces and native instances, without new lower support.
// The native arm is the oracle for return, copied bytes and remaining queue.

struct InotifyReadControlDir(PathBuf);

impl InotifyReadControlDir {
    fn new() -> Self {
        let temporary = PathBuf::from(std::env::var_os("TMPDIR").expect("slot TMPDIR is required"));
        assert!(temporary.is_absolute() && temporary.is_dir());
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = temporary.join(format!("inotify-read-contract-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for InotifyReadControlDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn inotify_read_control_instance() -> std::fs::File {
    // SAFETY: successful init1 returns a newly owned descriptor.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(fd >= 0, "native inotify setup failed");
    // SAFETY: this is the sole owner of the newly returned descriptor.
    unsafe { std::fs::File::from_raw_fd(fd) }
}

fn inotify_read_control_native(file: &std::fs::File, address: *mut libc::c_void, count: usize) -> i64 {
    // SAFETY: the caller retains file and a live mapping. A protected suffix is
    // intentional: the kernel performs copy_to_user rather than Rust access.
    let returned = unsafe { libc::read(file.as_raw_fd(), address, count) };
    if returned < 0 {
        io_error(std::io::Error::last_os_error())
    } else {
        returned as i64
    }
}

fn inotify_read_control_queued(file: &std::fs::File) -> i32 {
    let mut queued = -1_i32;
    // SAFETY: queued is a live writable int and file is retained.
    assert_eq!(unsafe { libc::ioctl(file.as_raw_fd(), libc::FIONREAD, &mut queued) }, 0);
    assert!(queued >= 0);
    queued
}

fn inotify_read_control_ready(file: &std::fs::File) -> i16 {
    let mut pollfd = libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live pollfd; timeout zero cannot wait for external activity.
    let returned = unsafe { libc::poll(&mut pollfd, 1, 0) };
    assert!((0..=1).contains(&returned));
    assert_eq!(pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL), 0);
    pollfd.revents
}

fn inotify_read_control_after(file: &std::fs::File) -> (i32, i16, i64, Vec<u8>) {
    let queued = inotify_read_control_queued(file);
    let ready = inotify_read_control_ready(file);
    let mut bytes = [0_u8; 4096];
    let returned = inotify_read_control_native(file, bytes.as_mut_ptr().cast(), bytes.len());
    let drained = if returned >= 0 {
        bytes[..returned as usize].to_vec()
    } else {
        assert_eq!(returned, negative_errno(libc::EAGAIN), "nonblocking drain setup failed");
        Vec::new()
    };
    assert_eq!(inotify_read_control_queued(file), 0, "drain must consume the bounded setup queue");
    assert_eq!(inotify_read_control_ready(file), 0);
    assert_eq!(
        inotify_read_control_native(file, bytes.as_mut_ptr().cast(), bytes.len()),
        negative_errno(libc::EAGAIN),
        "no additional event may be hidden after the drain"
    );
    (queued, ready, returned, drained)
}

fn inotify_read_control_guest_memory() -> GuestMemory {
    let mut memory = GuestMemory::new(0, 4 * PAGE_SIZE as usize).unwrap();
    memory.write(2 * PAGE_SIZE - 40, &[0xa5; 40]).unwrap();
    memory.enable_user_access();
    memory.map_user_range(PAGE_SIZE, PAGE_SIZE, false).unwrap();
    memory.map_user_range(2 * PAGE_SIZE, PAGE_SIZE, true).unwrap();
    memory
}

fn inotify_read_control_install(state: &mut LoadedStaticElf, instance: std::fs::File) -> i32 {
    let fd = insert_file_with_flags(state, instance, true, None);
    assert!(fd >= 3);
    fd as i32
}

#[test]
fn inotify_empty_fault_read_matches_native_error_and_queue() {
    let root = InotifyReadControlDir::new();
    let native = inotify_read_control_instance();
    let mediated = inotify_read_control_instance();
    let native_buffer = NativeFaultBuffer::new();
    let mut memory = inotify_read_control_guest_memory();
    let mut state = test_state(&root.0);
    let fd = inotify_read_control_install(&mut state, mediated);
    assert_eq!(inotify_read_control_queued(&native), 0);
    assert_eq!(inotify_read_control_queued(&state.files[&fd]), 0);

    let native_result = inotify_read_control_native(&native, native_buffer.invalid.cast(), 64);
    let mediated_result = syscall_result(
        &mut memory,
        &mut state,
        libc::SYS_read,
        [fd as u64, 2 * PAGE_SIZE, 64, 0, 0, 0],
    );
    let native_after = inotify_read_control_after(&native);
    let mediated_after = inotify_read_control_after(&state.files[&fd]);
    assert_eq!(
        (mediated_result, mediated_after),
        (native_result, native_after),
        "an empty nonblocking queue must retain native precedence over a mapped PROT_NONE destination"
    );
}

#[test]
fn inotify_partial_fault_read_matches_native_copy_and_consumption() {
    let root = InotifyReadControlDir::new();
    let native = inotify_read_control_instance();
    let mediated = inotify_read_control_instance();
    let witness = inotify_read_control_instance();
    let path = CString::new(root.0.as_os_str().as_bytes()).unwrap();
    let watch = |file: &std::fs::File| {
        // SAFETY: path is a live NUL-terminated controlled directory name.
        let wd = unsafe { libc::inotify_add_watch(file.as_raw_fd(), path.as_ptr(), libc::IN_CREATE) };
        assert!(wd >= 0, "native watch setup failed");
        assert_eq!(inotify_read_control_queued(file), 0);
        wd
    };
    let native_wd = watch(&native);
    let mediated_wd = watch(&mediated);
    let witness_wd = watch(&witness);
    assert_eq!(native_wd, mediated_wd, "identical fresh instance setup must use the same watch ID");

    // Distinct producer names avoid consecutive-event coalescing. The witness
    // verifies both complete records without consuming either measured queue.
    std::fs::File::create(root.0.join("alpha")).unwrap();
    std::fs::File::create(root.0.join("beta")).unwrap();
    let mut witnessed = [0_u8; 64];
    assert_eq!(
        inotify_read_control_native(&witness, witnessed.as_mut_ptr().cast(), witnessed.len()),
        64
    );
    for (offset, name) in [(0_usize, b"alpha\0".as_slice()), (32, b"beta\0".as_slice())] {
        assert_eq!(i32::from_ne_bytes(witnessed[offset..offset + 4].try_into().unwrap()), witness_wd);
        assert_eq!(u32::from_ne_bytes(witnessed[offset + 4..offset + 8].try_into().unwrap()), libc::IN_CREATE);
        assert_eq!(u32::from_ne_bytes(witnessed[offset + 12..offset + 16].try_into().unwrap()), 16);
        assert!(witnessed[offset + 16..offset + 32].starts_with(name));
    }
    assert_eq!(inotify_read_control_queued(&witness), 0);
    assert_eq!(inotify_read_control_queued(&native), 64);
    assert_eq!(inotify_read_control_queued(&mediated), 64);
    assert_eq!(inotify_read_control_ready(&native), libc::POLLIN);
    assert_eq!(inotify_read_control_ready(&mediated), libc::POLLIN);

    let native_buffer = NativeFaultBuffer::new();
    // SAFETY: invalid begins the protected second page; the preceding forty
    // bytes remain writable in the first page and have the guest's page offset.
    let native_address = unsafe { native_buffer.invalid.sub(40) };
    // SAFETY: this initializes only the forty live writable prefix bytes.
    unsafe { std::ptr::write_bytes(native_address, 0xa5, 40) };
    let mut memory = inotify_read_control_guest_memory();
    let mut state = test_state(&root.0);
    let fd = inotify_read_control_install(&mut state, mediated);

    let native_result = inotify_read_control_native(&native, native_address.cast(), 64);
    let mediated_result = syscall_result(
        &mut memory,
        &mut state,
        libc::SYS_read,
        [fd as u64, 2 * PAGE_SIZE - 40, 64, 0, 0, 0],
    );
    let mut native_prefix = [0_u8; 40];
    // SAFETY: read only the first page's writable prefix, never the protected suffix.
    unsafe { std::ptr::copy_nonoverlapping(native_address, native_prefix.as_mut_ptr(), 40) };
    let mut mediated_prefix = [0_u8; 40];
    memory.read(2 * PAGE_SIZE - 40, &mut mediated_prefix).unwrap();
    let native_after = inotify_read_control_after(&native);
    let mediated_after = inotify_read_control_after(&state.files[&fd]);
    assert_eq!(
        (mediated_result, mediated_prefix, mediated_after),
        (native_result, native_prefix, native_after),
        "the original count, negative-result copy effects and remaining native queue must agree"
    );
}
