/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// The Tool's reserved directory (`LoadedStaticElf::tool_directory`): only an
// `O_DIRECTORY` open and a stat of its exact path reach it; every other path
// route to it, and every creation in it, is refused with `EACCES`, as procfs
// is, while the descriptors opened there work as any other.

const TOOL_DIRECTORY_PATH: u64 = 0x100;
const TOOL_DIRECTORY_OTHER: u64 = 0x900;
const TOOL_DIRECTORY_EMPTY: u64 = 0xd00;
const TOOL_DIRECTORY_OUTPUT: u64 = 0x1000;

struct ToolDirectoryFixture {
    root: TestDir,
    memory: GuestMemory,
    state: LoadedStaticElf,
    reserved: PathBuf,
}

impl ToolDirectoryFixture {
    fn new() -> Self {
        let root = TestDir::new();
        let reserved = root.0.join("reserved");
        std::fs::create_dir(&reserved).unwrap();
        std::fs::create_dir(root.0.join("elsewhere")).unwrap();
        std::fs::write(root.0.join("file"), b"payload").unwrap();
        std::os::unix::fs::symlink(&reserved, root.0.join("link")).unwrap();
        let mut state = test_state(&root.0);
        state.tool_directory = Some(crate::elf::ToolDirectory::open(&reserved).unwrap());
        let memory = GuestMemory::new(0, 4 * PAGE_SIZE as usize).unwrap();
        Self {
            root,
            memory,
            state,
            reserved,
        }
    }

    fn call(&mut self, number: libc::c_long, args: [u64; 6]) -> i64 {
        syscall_result(&mut self.memory, &mut self.state, number, args)
    }

    fn path(&mut self, address: u64, path: &str) -> u64 {
        write_c_string(&mut self.memory, address, path);
        address
    }

    fn exact(&self) -> String {
        self.reserved.to_str().unwrap().to_owned()
    }

    fn assert_reserved_untouched(&self) {
        assert_eq!(std::fs::read_dir(&self.reserved).unwrap().count(), 0);
        assert_eq!(
            std::fs::metadata(&self.reserved)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            std::fs::metadata(self.root.0.join("elsewhere"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        );
        assert_eq!(std::fs::read(self.root.0.join("file")).unwrap(), b"payload");
    }
}

#[test]
fn tool_directory_refuses_every_other_path_route() {
    let mut f = ToolDirectoryFixture::new();
    let cwd = f.state.cwd.clone();
    let root_path = f.root.0.to_str().unwrap().to_owned();
    let root_path = f.path(TOOL_DIRECTORY_PATH, &root_path);
    let root = f.call(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            root_path,
            (libc::O_RDONLY | libc::O_DIRECTORY) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(root >= 3, "{root}");
    let at_fdcwd = libc::AT_FDCWD as u64;
    let eacces = negative_errno(libc::EACCES);
    // The directory by a symbolic link, relative to the working directory, by
    // a `..` and relative to a descriptor: each lookup resolves to it.
    for (dirfd, spelling) in [
        (at_fdcwd, "link"),
        (at_fdcwd, "reserved"),
        (at_fdcwd, "reserved/"),
        (at_fdcwd, "elsewhere/../reserved"),
        (root as u64, "reserved"),
        (root as u64, "link"),
    ] {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        let what = format!("{spelling} from {dirfd:#x}");
        for flags in [
            libc::O_RDONLY | libc::O_DIRECTORY,
            libc::O_RDONLY,
            libc::O_PATH | libc::O_DIRECTORY,
            libc::O_PATH,
        ] {
            assert_eq!(
                f.call(libc::SYS_openat, [dirfd, path, flags as u64, 0, 0, 0]),
                eacces,
                "open {what} flags={flags:#x}"
            );
        }
        assert_eq!(
            f.call(
                libc::SYS_openat,
                [
                    dirfd,
                    path,
                    (libc::O_TMPFILE | libc::O_RDWR) as u64,
                    0o600,
                    0,
                    0
                ],
            ),
            eacces,
            "O_TMPFILE in {what}"
        );
        assert_eq!(
            f.call(
                libc::SYS_newfstatat,
                [dirfd, path, TOOL_DIRECTORY_OUTPUT, 0, 0, 0]
            ),
            eacces,
            "stat {what}"
        );
        assert_eq!(
            f.call(
                libc::SYS_statx,
                [
                    dirfd,
                    path,
                    0,
                    libc::STATX_BASIC_STATS as u64,
                    TOOL_DIRECTORY_OUTPUT,
                    0
                ],
            ),
            eacces,
            "statx {what}"
        );
        for mode in [libc::W_OK, libc::R_OK | libc::X_OK, libc::F_OK] {
            assert_eq!(
                f.call(libc::SYS_faccessat2, [dirfd, path, mode as u64, 0, 0, 0]),
                eacces,
                "access {what} mode={mode}"
            );
        }
        assert_eq!(
            f.call(libc::SYS_fchmodat, [dirfd, path, 0o555, 0, 0, 0]),
            eacces,
            "chmod {what}"
        );
        assert_eq!(
            f.call(libc::SYS_fchownat, [dirfd, path, 0, 0, 0, 0]),
            eacces,
            "chown {what}"
        );
        assert_eq!(
            f.call(libc::SYS_utimensat, [dirfd, path, 0, 0, 0, 0]),
            eacces,
            "utimes {what}"
        );
        if !spelling.ends_with("link") {
            // readlink of the directory itself; a link is read as the link.
            assert_eq!(
                f.call(
                    libc::SYS_readlinkat,
                    [dirfd, path, TOOL_DIRECTORY_OUTPUT, 64, 0, 0]
                ),
                eacces,
                "readlink {what}"
            );
        }
        // A new name below it, by every call that creates one.
        let below = f.path(
            TOOL_DIRECTORY_OTHER,
            &format!("{}/new", spelling.trim_end_matches('/')),
        );
        let file = f.path(TOOL_DIRECTORY_EMPTY, "file");
        for (name, number, args) in [
            ("mkdirat", libc::SYS_mkdirat, [dirfd, below, 0o700, 0, 0, 0]),
            (
                "open O_CREAT",
                libc::SYS_openat,
                [
                    dirfd,
                    below,
                    (libc::O_CREAT | libc::O_WRONLY) as u64,
                    0o600,
                    0,
                    0,
                ],
            ),
            (
                "mknodat",
                libc::SYS_mknodat,
                [dirfd, below, (libc::S_IFIFO | 0o600) as u64, 0, 0, 0],
            ),
            (
                "symlinkat",
                libc::SYS_symlinkat,
                [file, dirfd, below, 0, 0, 0],
            ),
            (
                "linkat",
                libc::SYS_linkat,
                [at_fdcwd, file, dirfd, below, 0, 0],
            ),
            (
                "renameat",
                libc::SYS_renameat,
                [at_fdcwd, file, dirfd, below, 0, 0],
            ),
            ("unlinkat", libc::SYS_unlinkat, [dirfd, below, 0, 0, 0, 0]),
            (
                "rmdir",
                libc::SYS_unlinkat,
                [dirfd, below, libc::AT_REMOVEDIR as u64, 0, 0, 0],
            ),
        ] {
            assert_eq!(f.call(number, args), eacces, "{name} below {what}");
        }
        f.assert_reserved_untouched();
    }
    // The calls that name a path relative to the working directory only.
    for spelling in ["link", "reserved"] {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        let name = f.path(TOOL_DIRECTORY_OTHER, "user.reverie");
        for (call, number, args) in [
            (
                "statfs",
                libc::SYS_statfs,
                [path, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0],
            ),
            ("chdir", libc::SYS_chdir, [path, 0, 0, 0, 0, 0]),
            ("truncate", libc::SYS_truncate, [path, 0, 0, 0, 0, 0]),
            (
                "getxattr",
                libc::SYS_getxattr,
                [path, name, TOOL_DIRECTORY_OUTPUT, 64, 0, 0],
            ),
            (
                "listxattr",
                libc::SYS_listxattr,
                [path, TOOL_DIRECTORY_OUTPUT, 64, 0, 0, 0],
            ),
            (
                "setxattr",
                libc::SYS_setxattr,
                [path, name, TOOL_DIRECTORY_EMPTY, 1, 0, 0],
            ),
            (
                "removexattr",
                libc::SYS_removexattr,
                [path, name, 0, 0, 0, 0],
            ),
        ] {
            assert_eq!(f.call(number, args), eacces, "{call} {spelling}");
        }
        let watcher = f.call(
            libc::SYS_inotify_init1,
            [libc::IN_NONBLOCK as u64, 0, 0, 0, 0, 0],
        );
        assert!(watcher >= 0, "{watcher}");
        assert_eq!(
            f.call(
                libc::SYS_inotify_add_watch,
                [watcher as u64, path, libc::IN_ALL_EVENTS as u64, 0, 0, 0]
            ),
            eacces,
            "inotify_add_watch {spelling}"
        );
        assert_eq!(f.call(libc::SYS_close, [watcher as u64, 0, 0, 0, 0, 0]), 0);
    }
    // The working directory never moved into it.
    assert_eq!(f.state.cwd, cwd);
    // The directory itself is neither removed, renamed nor replaced.
    let reserved = f.path(TOOL_DIRECTORY_PATH, "reserved");
    let elsewhere = f.path(TOOL_DIRECTORY_OTHER, "elsewhere");
    for (call, number, args) in [
        (
            "rmdir",
            libc::SYS_unlinkat,
            [at_fdcwd, reserved, libc::AT_REMOVEDIR as u64, 0, 0, 0],
        ),
        (
            "unlink",
            libc::SYS_unlinkat,
            [at_fdcwd, reserved, 0, 0, 0, 0],
        ),
        (
            "rename from",
            libc::SYS_renameat,
            [at_fdcwd, reserved, at_fdcwd, elsewhere, 0, 0],
        ),
        (
            "rename over",
            libc::SYS_renameat,
            [at_fdcwd, elsewhere, at_fdcwd, reserved, 0, 0],
        ),
        (
            "exchange",
            libc::SYS_renameat2,
            [
                at_fdcwd,
                elsewhere,
                at_fdcwd,
                reserved,
                libc::RENAME_EXCHANGE as u64,
                0,
            ],
        ),
    ] {
        assert_eq!(f.call(number, args), eacces, "{call} the directory itself");
    }
    assert!(f.reserved.is_dir() && f.root.0.join("elsewhere").is_dir());
    f.assert_reserved_untouched();
    // A symbolic link to it is not the directory: removing it is an ordinary
    // unlink of the link.
    let link = f.path(TOOL_DIRECTORY_PATH, "link");
    assert_eq!(f.call(libc::SYS_unlinkat, [at_fdcwd, link, 0, 0, 0, 0]), 0);
    assert!(f.reserved.is_dir() && !f.root.0.join("link").exists());
    // An ordinary directory beside it on the same file system is unaffected.
    let elsewhere = f.path(TOOL_DIRECTORY_PATH, "elsewhere");
    let opened = f.call(
        libc::SYS_openat,
        [
            at_fdcwd,
            elsewhere,
            (libc::O_RDONLY | libc::O_DIRECTORY) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(opened >= 3, "{opened}");
    assert_eq!(
        f.call(
            libc::SYS_statfs,
            [elsewhere, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0]
        ),
        0
    );
    let new = f.path(TOOL_DIRECTORY_OTHER, "elsewhere/new");
    assert_eq!(
        f.call(libc::SYS_mkdirat, [at_fdcwd, new, 0o700, 0, 0, 0]),
        0
    );
    assert!(f.root.0.join("elsewhere/new").is_dir());
}

#[test]
fn tool_directory_admits_only_its_exact_directory_open_and_stat() {
    let mut f = ToolDirectoryFixture::new();
    let at_fdcwd = libc::AT_FDCWD as u64;
    let eacces = negative_errno(libc::EACCES);
    let exact = f.exact();
    let path = f.path(TOOL_DIRECTORY_PATH, &exact);
    let identity = std::fs::metadata(&f.reserved).unwrap();
    // A stat by the exact path reaches it, following a final link or not.
    for (number, args) in [
        (
            libc::SYS_newfstatat,
            [at_fdcwd, path, TOOL_DIRECTORY_OUTPUT, 0, 0, 0],
        ),
        (
            libc::SYS_newfstatat,
            [
                at_fdcwd,
                path,
                TOOL_DIRECTORY_OUTPUT,
                libc::AT_SYMLINK_NOFOLLOW as u64,
                0,
                0,
            ],
        ),
        (libc::SYS_stat, [path, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0]),
    ] {
        assert_eq!(f.call(number, args), 0);
        let stat: libc::stat = read_struct(&f.memory, TOOL_DIRECTORY_OUTPUT);
        assert_eq!((stat.st_dev, stat.st_ino), (identity.dev(), identity.ino()));
    }
    assert_eq!(
        f.call(
            libc::SYS_statx,
            [
                at_fdcwd,
                path,
                0,
                libc::STATX_BASIC_STATS as u64,
                TOOL_DIRECTORY_OUTPUT,
                0
            ],
        ),
        0
    );
    let statx: libc::statx = read_struct(&f.memory, TOOL_DIRECTORY_OUTPUT);
    assert_eq!(statx.stx_ino, identity.ino());
    // An open of it as a directory reaches it; any other open does not.
    let mut descriptors = Vec::new();
    for flags in [
        libc::O_RDONLY | libc::O_DIRECTORY,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        libc::O_PATH | libc::O_DIRECTORY,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
    ] {
        let fd = f.call(libc::SYS_openat, [at_fdcwd, path, flags as u64, 0, 0, 0]);
        assert!(fd >= 3, "flags={flags:#x}: {fd}");
        descriptors.push((fd as u64, flags & libc::O_PATH != 0));
    }
    for flags in [libc::O_RDONLY, libc::O_PATH] {
        assert_eq!(
            f.call(libc::SYS_openat, [at_fdcwd, path, flags as u64, 0, 0, 0]),
            eacces,
            "flags={flags:#x}"
        );
    }
    assert_eq!(
        f.call(
            libc::SYS_openat,
            [
                at_fdcwd,
                path,
                (libc::O_TMPFILE | libc::O_RDWR) as u64,
                0o600,
                0,
                0
            ],
        ),
        eacces
    );
    // The other path calls by the exact path stay refused.
    let name = f.path(TOOL_DIRECTORY_OTHER, "user.reverie");
    for (call, number, args) in [
        (
            "statfs",
            libc::SYS_statfs,
            [path, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0],
        ),
        ("chdir", libc::SYS_chdir, [path, 0, 0, 0, 0, 0]),
        (
            "access",
            libc::SYS_faccessat2,
            [at_fdcwd, path, libc::R_OK as u64, 0, 0, 0],
        ),
        (
            "getxattr",
            libc::SYS_getxattr,
            [path, name, TOOL_DIRECTORY_OUTPUT, 64, 0, 0],
        ),
        (
            "chmod",
            libc::SYS_fchmodat,
            [at_fdcwd, path, 0o555, 0, 0, 0],
        ),
    ] {
        assert_eq!(f.call(number, args), eacces, "{call} by the exact path");
    }
    let below = f.path(TOOL_DIRECTORY_OTHER, &format!("{exact}/new"));
    assert_eq!(
        f.call(libc::SYS_mkdirat, [at_fdcwd, below, 0o700, 0, 0, 0]),
        eacces
    );
    // Its descriptors work as any other: the calls that name them are not
    // refused, but a working directory in it is, and so is every path
    // looked up from it that resolves to it or creates a name in it.
    let empty = f.path(TOOL_DIRECTORY_EMPTY, "");
    for (fd, path_only) in descriptors {
        assert_eq!(
            f.call(libc::SYS_fstat, [fd, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0]),
            0
        );
        let stat: libc::stat = read_struct(&f.memory, TOOL_DIRECTORY_OUTPUT);
        assert_eq!((stat.st_dev, stat.st_ino), (identity.dev(), identity.ino()));
        assert_eq!(
            f.call(
                libc::SYS_newfstatat,
                [
                    fd,
                    empty,
                    TOOL_DIRECTORY_OUTPUT,
                    libc::AT_EMPTY_PATH as u64,
                    0,
                    0
                ],
            ),
            0
        );
        if !path_only {
            assert_eq!(
                f.call(libc::SYS_fstatfs, [fd, TOOL_DIRECTORY_OUTPUT, 0, 0, 0, 0]),
                0
            );
            let listed = f.call(
                libc::SYS_getdents64,
                [fd, TOOL_DIRECTORY_OUTPUT, 512, 0, 0, 0],
            );
            assert!(listed > 0, "{listed}");
            assert_eq!(f.call(libc::SYS_fchdir, [fd, 0, 0, 0, 0, 0]), eacces);
        }
        let dot = f.path(TOOL_DIRECTORY_OTHER, ".");
        assert_eq!(
            f.call(
                libc::SYS_openat,
                [
                    fd,
                    dot,
                    (libc::O_RDONLY | libc::O_DIRECTORY) as u64,
                    0,
                    0,
                    0
                ],
            ),
            eacces
        );
        let new = f.path(TOOL_DIRECTORY_OTHER, "new");
        assert_eq!(f.call(libc::SYS_mkdirat, [fd, new, 0o700, 0, 0, 0]), eacces);
        assert_eq!(
            f.call(
                libc::SYS_openat,
                [fd, new, (libc::O_CREAT | libc::O_RDWR) as u64, 0o600, 0, 0],
            ),
            eacces
        );
        // Its parent is an ordinary directory.
        let parent = f.path(TOOL_DIRECTORY_OTHER, "..");
        let opened = f.call(
            libc::SYS_openat,
            [
                fd,
                parent,
                (libc::O_RDONLY | libc::O_DIRECTORY) as u64,
                0,
                0,
                0,
            ],
        );
        assert!(opened >= 3, "{opened}");
        assert_eq!(f.call(libc::SYS_close, [opened as u64, 0, 0, 0, 0, 0]), 0);
        assert_eq!(f.call(libc::SYS_close, [fd, 0, 0, 0, 0, 0]), 0);
    }
    f.assert_reserved_untouched();
}

#[test]
fn tool_directory_reservation_needs_an_absolute_ordinary_directory() {
    let root = TestDir::new();
    std::fs::write(root.0.join("file"), b"").unwrap();
    let errno = |path: &Path| {
        crate::elf::ToolDirectory::open(path)
            .unwrap_err()
            .raw_os_error()
    };
    assert_eq!(errno(Path::new("relative")), Some(libc::EINVAL));
    assert_eq!(errno(Path::new("/proc")), Some(libc::EINVAL));
    assert_eq!(errno(&root.0.join("file")), Some(libc::ENOTDIR));
    assert_eq!(errno(&root.0.join("missing")), Some(libc::ENOENT));
    let reserved = crate::elf::ToolDirectory::open(&root.0).unwrap();
    assert_eq!(reserved.path(), root.0.as_path());
}
