// Copyright (c) Meta Platforms, Inc. and affiliates.
// All rights reserved.
//
// This source code is licensed under the BSD-style license found in the
// LICENSE file in the root directory of this source tree.

// Guest paths below an fd link (`/proc/self/fd/<n>/<rest>`) resolve below
// the guest descriptor they name, with native Linux answers, and never
// reach the supervisor process's own descriptor table. The supervisor
// plants a secret at a known descriptor number the guest does not hold:
// before the executor parsed fd-link prefixes, the literal spelling was
// forwarded to a host openat2 and read that descriptor (or answered
// ENOENT/ELOOP by supervisor occupancy timing).

/// Open `path` in the guest at guest address 0x100 with `flags`.
fn guest_open(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    path: &str,
    flags: libc::c_int,
) -> i64 {
    write_c_string(memory, 0x100, path);
    syscall_result(
        memory,
        state,
        libc::SYS_openat,
        [libc::AT_FDCWD as u64, 0x100, flags as u64, 0, 0, 0],
    )
}

/// Read up to 64 bytes from guest descriptor `fd` at guest address 0x300.
fn guest_read(memory: &mut GuestMemory, state: &mut LoadedStaticElf, fd: i64) -> Vec<u8> {
    let count = syscall_result(
        memory,
        state,
        libc::SYS_read,
        [fd as u64, 0x300, 64, 0, 0, 0],
    );
    assert!(count >= 0, "read from guest fd {fd}: {count}");
    let mut bytes = vec![0u8; count as usize];
    memory.read(0x300, &mut bytes).unwrap();
    bytes
}

/// Stat `path` in the guest (result only; the stat buffer is at 0x400).
fn guest_stat(memory: &mut GuestMemory, state: &mut LoadedStaticElf, path: &str) -> i64 {
    write_c_string(memory, 0x100, path);
    syscall_result(
        memory,
        state,
        libc::SYS_newfstatat,
        [libc::AT_FDCWD as u64, 0x100, 0x400, 0, 0, 0],
    )
}

#[test]
fn fd_link_subpaths_resolve_below_the_guest_descriptor() {
    let root = TestDir::new();
    std::fs::create_dir(root.0.join("sentinel")).unwrap();
    std::fs::write(root.0.join("sentinel/marker"), b"guest-marker").unwrap();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();

    let dir = guest_open(&mut memory, &mut state, "sentinel", libc::O_RDONLY);
    assert!(dir >= 0, "open sentinel dir: {dir}");
    let opened = guest_open(
        &mut memory,
        &mut state,
        &format!("/proc/self/fd/{dir}/marker"),
        libc::O_RDONLY,
    );
    assert!(opened >= 0, "open below dir fd link: {opened}");
    assert_eq!(guest_read(&mut memory, &mut state, opened), b"guest-marker");
    assert_eq!(
        guest_stat(
            &mut memory,
            &mut state,
            &format!("/proc/self/fd/{dir}/marker")
        ),
        0
    );

    // The same through an O_PATH directory descriptor: the kernel
    // resolves the magic link to the directory and continues below it.
    let dir = guest_open(
        &mut memory,
        &mut state,
        "sentinel",
        libc::O_PATH | libc::O_DIRECTORY,
    );
    assert!(dir >= 0, "open sentinel dir O_PATH: {dir}");
    let opened = guest_open(
        &mut memory,
        &mut state,
        &format!("/proc/self/fd/{dir}/marker"),
        libc::O_RDONLY,
    );
    assert!(opened >= 0, "open below O_PATH dir fd link: {opened}");
    assert_eq!(guest_read(&mut memory, &mut state, opened), b"guest-marker");

    // Below a regular-file descriptor the kernel answers ENOTDIR.
    let file = guest_open(
        &mut memory,
        &mut state,
        "sentinel/marker",
        libc::O_RDONLY,
    );
    assert!(file >= 0, "open marker file: {file}");
    assert_eq!(
        guest_open(
            &mut memory,
            &mut state,
            &format!("/proc/self/fd/{file}/marker"),
            libc::O_RDONLY
        ),
        negative_errno(libc::ENOTDIR)
    );
}

#[test]
fn fd_link_spellings_never_reach_the_supervisors_descriptors() {
    let root = TestDir::new();
    std::fs::write(root.0.join("supervisor-secret"), b"supervisor-secret").unwrap();
    // Plant the secret at a known supervisor (this process) descriptor
    // number the guest does not hold: the first free number in a range
    // far above anything the guest allocates in these tests (other
    // tests in this process may hold any fixed number).
    let planted = (4000..4010)
        .find(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1)
        .expect("no free supervisor descriptor to plant at");
    let planted_fd = planted;
    let secret = std::fs::File::open(root.0.join("supervisor-secret")).unwrap();
    assert_eq!(
        unsafe { libc::dup2(secret.as_raw_fd(), planted_fd) },
        planted_fd
    );
    // Owns only the planted number; `secret` keeps its own descriptor.
    let _planted =
        unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(planted_fd) };

    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    for spelling in [
        format!("/proc/self/fd/{planted_fd}"),
        format!("/proc/self/fd/{planted_fd}/anything"),
        format!("/proc/1/fd/{planted_fd}"),
        format!("/proc/424242/fd/{planted_fd}"),
        format!("/proc/424242/fd/{planted_fd}/anything"),
    ] {
        assert_eq!(
            guest_open(&mut memory, &mut state, &spelling, libc::O_RDONLY),
            negative_errno(libc::ENOENT),
            "{spelling} reached the supervisor's descriptor {planted_fd}"
        );
        assert_eq!(
            guest_stat(&mut memory, &mut state, &spelling),
            negative_errno(libc::ENOENT),
            "{spelling} reached the supervisor's descriptor {planted_fd}"
        );
    }
}
