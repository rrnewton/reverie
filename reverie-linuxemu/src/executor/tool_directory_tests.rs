/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// The Tool's reserved directory (`LoadedStaticElf::tool_directory`): only an
// `O_DIRECTORY` open and a newfstatat of its exact path reach it, and only
// while that path still names it; every other path route to it, and every
// creation in it (through a final symbolic link too), is refused with
// `EACCES`, as procfs is, while the descriptors opened there work as any
// other.

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
    // A creation through a final symbolic link lands where the link points,
    // as on Linux. A dangling link into the directory, by an absolute or a
    // relative target, through a second link, or up from another directory,
    // is refused, by a path relative to the working directory or to a
    // descriptor; nothing is created there, though the directory is writable.
    std::os::unix::fs::symlink(
        format!("{}/x", f.exact()),
        f.root.0.join("dangling-absolute"),
    )
    .unwrap();
    std::os::unix::fs::symlink("reserved/x", f.root.0.join("dangling-relative")).unwrap();
    std::os::unix::fs::symlink("chain-second", f.root.0.join("chain-first")).unwrap();
    std::os::unix::fs::symlink(format!("{}/y", f.exact()), f.root.0.join("chain-second")).unwrap();
    std::fs::create_dir(f.root.0.join("deeper")).unwrap();
    std::os::unix::fs::symlink("../reserved/z", f.root.0.join("deeper/dangling-up")).unwrap();
    for (dirfd, spelling) in [
        (at_fdcwd, "dangling-absolute"),
        (at_fdcwd, "dangling-relative"),
        (at_fdcwd, "chain-first"),
        (at_fdcwd, "deeper/dangling-up"),
        (root as u64, "dangling-relative"),
        (root as u64, "chain-first"),
    ] {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        for flags in [
            libc::O_CREAT | libc::O_WRONLY,
            libc::O_CREAT | libc::O_RDWR | libc::O_TRUNC,
            libc::O_CREAT | libc::O_RDONLY,
        ] {
            assert_eq!(
                f.call(libc::SYS_openat, [dirfd, path, flags as u64, 0o600, 0, 0]),
                eacces,
                "create through {spelling} from {dirfd:#x} flags={flags:#x}"
            );
        }
        if dirfd == at_fdcwd {
            assert_eq!(
                f.call(libc::SYS_creat, [path, 0o600, 0, 0, 0, 0]),
                eacces,
                "creat through {spelling}"
            );
        }
        f.assert_reserved_untouched();
    }
    // A creating open of the directory itself, by any spelling, its exact
    // path included, is refused rather than answered as a directory.
    let exact = f.exact();
    for spelling in ["reserved", "link", "elsewhere/../reserved", exact.as_str()] {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        for flags in [
            libc::O_CREAT | libc::O_RDONLY,
            libc::O_CREAT | libc::O_WRONLY,
        ] {
            assert_eq!(
                f.call(
                    libc::SYS_openat,
                    [at_fdcwd, path, flags as u64, 0o600, 0, 0]
                ),
                eacces,
                "creating open of {spelling} flags={flags:#x}"
            );
        }
    }
    f.assert_reserved_untouched();
    // An O_PATH open creates nothing (Linux's build_open_how drops O_CREAT),
    // so it is a lookup like any other: what is missing is ENOENT, whatever
    // directory holds it.
    for spelling in ["link/new", "dangling-absolute", "reserved/new"] {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        assert_eq!(
            f.call(
                libc::SYS_openat,
                [
                    at_fdcwd,
                    path,
                    (libc::O_PATH | libc::O_CREAT) as u64,
                    0o600,
                    0,
                    0
                ],
            ),
            negative_errno(libc::ENOENT),
            "O_PATH|O_CREAT of {spelling}"
        );
    }
    f.assert_reserved_untouched();
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
    // statx is no stat the exception names.
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
        eacces
    );
    // An open of it as a directory reaches it; any other open does not. An
    // O_PATH open's O_CREAT is dropped first, as on Linux.
    let mut descriptors = Vec::new();
    for flags in [
        libc::O_RDONLY | libc::O_DIRECTORY,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        libc::O_PATH | libc::O_DIRECTORY,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CREAT,
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
fn tool_directory_admissions_fail_closed_once_its_path_is_replaced() {
    let mut f = ToolDirectoryFixture::new();
    let at_fdcwd = libc::AT_FDCWD as u64;
    let eacces = negative_errno(libc::EACCES);
    // The reserved directory moves away and another takes its path.
    std::fs::rename(&f.reserved, f.root.0.join("moved")).unwrap();
    std::fs::create_dir(&f.reserved).unwrap();
    let exact = f.exact();
    let path = f.path(TOOL_DIRECTORY_PATH, &exact);
    for flags in [
        libc::O_RDONLY | libc::O_DIRECTORY,
        libc::O_PATH | libc::O_DIRECTORY,
    ] {
        assert_eq!(
            f.call(libc::SYS_openat, [at_fdcwd, path, flags as u64, 0, 0, 0]),
            eacces,
            "exact open flags={flags:#x} of a replaced path"
        );
    }
    for flags in [0, libc::AT_SYMLINK_NOFOLLOW] {
        assert_eq!(
            f.call(
                libc::SYS_newfstatat,
                [at_fdcwd, path, TOOL_DIRECTORY_OUTPUT, flags as u64, 0, 0],
            ),
            eacces,
            "exact newfstatat flags={flags:#x} of a replaced path"
        );
    }
    // The reserved directory itself stays refused at its new path.
    let moved = f.path(TOOL_DIRECTORY_OTHER, "moved");
    assert_eq!(
        f.call(
            libc::SYS_openat,
            [
                at_fdcwd,
                moved,
                (libc::O_RDONLY | libc::O_DIRECTORY) as u64,
                0,
                0,
                0
            ],
        ),
        eacces
    );
    assert_eq!(
        f.call(
            libc::SYS_newfstatat,
            [at_fdcwd, moved, TOOL_DIRECTORY_OUTPUT, 0, 0, 0],
        ),
        eacces
    );
}

/// One creating open, run natively in `native_root` and through the executor
/// in the fixture's root, both holding the same tree: the result (a
/// descriptor or the errno) and which of `watched` exist afterwards, with
/// their modes.
type CreateOutcome = (Result<(), i32>, Vec<(String, Option<u32>)>);

fn create_outcome_native(root: &Path, path: &str, flags: i32, watched: &[&str]) -> CreateOutcome {
    let full = CString::new(root.join(path).as_os_str().as_bytes()).unwrap();
    // SAFETY: full is NUL-terminated; a returned descriptor is closed below.
    let fd = unsafe { libc::open(full.as_ptr(), flags | libc::O_CLOEXEC, 0o640) };
    let result = if fd >= 0 {
        // SAFETY: open returned a new owned descriptor.
        unsafe { libc::close(fd) };
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap())
    };
    (result, watched_state(root, watched))
}

fn watched_state(root: &Path, watched: &[&str]) -> Vec<(String, Option<u32>)> {
    watched
        .iter()
        .map(|name| {
            let mode = std::fs::symlink_metadata(root.join(name))
                .ok()
                .map(|metadata| metadata.permissions().mode() & 0o7777);
            ((*name).to_owned(), mode)
        })
        .collect()
}

/// The tree both sides of [`creating_through_a_final_link_follows_it_as_linux_does`]
/// hold: an ordinary target directory, links into it (dangling and not, by
/// relative and absolute targets), a 40-link and a 41-link chain.
fn final_link_tree(root: &Path) {
    let link =
        |target: &str, name: &str| std::os::unix::fs::symlink(target, root.join(name)).unwrap();
    std::fs::create_dir(root.join("target-dir")).unwrap();
    std::fs::write(root.join("existing"), b"payload").unwrap();
    std::fs::set_permissions(
        root.join("existing"),
        std::fs::Permissions::from_mode(0o604),
    )
    .unwrap();
    link("target-dir/new", "to-new");
    link(
        root.join("target-dir/new-absolute").to_str().unwrap(),
        "to-new-absolute",
    );
    link("existing", "to-existing");
    link("target-dir/new-excl", "to-new-excl");
    link("target-dir/new-nofollow", "to-new-nofollow");
    link("no-such-directory/new", "to-missing-directory");
    link("target-dir", "to-directory");
    for index in 0..40 {
        let next = if index == 39 {
            "target-dir/chained".to_owned()
        } else {
            format!("forty-{}", index + 1)
        };
        link(&next, &format!("forty-{index}"));
    }
    for index in 0..41 {
        let next = if index == 40 {
            "target-dir/too-far".to_owned()
        } else {
            format!("forty-one-{}", index + 1)
        };
        link(&next, &format!("forty-one-{index}"));
    }
}

#[test]
fn creating_through_a_final_link_follows_it_as_linux_does() {
    // While a directory is reserved, the executor follows a creating open's
    // final link itself, one held directory at a time. Away from the
    // reserved directory, each answer must be Linux's: the same result and
    // the same files, with the same modes, as a native open in a copy of the
    // tree.
    let mut f = ToolDirectoryFixture::new();
    final_link_tree(&f.root.0);
    let native = TestDir::new();
    final_link_tree(&native.0);
    // Linux applies the caller's umask; the executor applies the guest's.
    // SAFETY: umask has no preconditions; it is restored before returning.
    let previous = unsafe { libc::umask(f.state.umask) };
    let watched = [
        "target-dir/new",
        "target-dir/new-absolute",
        "existing",
        "target-dir/new-excl",
        "target-dir/new-nofollow",
        "target-dir/chained",
        "target-dir/too-far",
    ];
    let cases = [
        ("to-new", libc::O_CREAT | libc::O_WRONLY),
        ("to-new-absolute", libc::O_CREAT | libc::O_RDWR),
        ("to-existing", libc::O_CREAT | libc::O_WRONLY),
        ("to-new-excl", libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY),
        (
            "to-new-nofollow",
            libc::O_CREAT | libc::O_NOFOLLOW | libc::O_WRONLY,
        ),
        ("to-missing-directory", libc::O_CREAT | libc::O_WRONLY),
        ("to-directory", libc::O_CREAT | libc::O_RDONLY),
        ("forty-0", libc::O_CREAT | libc::O_WRONLY),
        ("forty-one-0", libc::O_CREAT | libc::O_WRONLY),
    ];
    let mut outcomes = Vec::new();
    for (spelling, flags) in cases {
        let path = f.path(TOOL_DIRECTORY_PATH, spelling);
        let fd = f.call(
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, path, flags as u64, 0o640, 0, 0],
        );
        let guest = if fd >= 0 {
            assert_eq!(f.call(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]), 0);
            Ok(())
        } else {
            Err(-fd as i32)
        };
        let guest = (guest, watched_state(&f.root.0, &watched));
        let native = create_outcome_native(&native.0, spelling, flags, &watched);
        outcomes.push((spelling, guest.0, native.0));
        assert_eq!(guest, native, "{spelling} flags={flags:#x}");
    }
    // SAFETY: restores the umask saved above.
    unsafe { libc::umask(previous) };
    // The answers the cases were chosen for, which both sides gave.
    let expected = [
        ("to-new", Ok(())),
        ("to-new-absolute", Ok(())),
        ("to-existing", Ok(())),
        ("to-new-excl", Err(libc::EEXIST)),
        ("to-new-nofollow", Err(libc::ELOOP)),
        ("to-missing-directory", Err(libc::ENOENT)),
        ("to-directory", Err(libc::EISDIR)),
        ("forty-0", Ok(())),
        ("forty-one-0", Err(libc::ELOOP)),
    ];
    for ((spelling, guest, _), (name, answer)) in outcomes.iter().zip(expected) {
        assert_eq!((*spelling, *guest), (name, answer));
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
