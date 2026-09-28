import collections
import ctypes
import errno
import fcntl
import os
import runpy
import stat


AT_FDCWD = -100

# Retain the complete pwrite, status-flag, and loginuid matrix from v3.  The
# evidence packet is self-contained: every run_path target lives beside this
# source in the v9 directory.
base = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "native-proc-matrix-v3.py")
)
paths = base["PATHS"]


def probe(path, flags, dir_fd=None):
    try:
        if dir_fd is None:
            descriptor = os.open(path, flags, 0o600)
        else:
            descriptor = os.open(path, flags, 0o600, dir_fd=dir_fd)
    except OSError as error:
        return error.errno, None, None
    try:
        return (
            None,
            fcntl.fcntl(descriptor, fcntl.F_GETFL),
            stat.S_IFMT(os.fstat(descriptor).st_mode),
        )
    finally:
        os.close(descriptor)


def check(path, flags, expected, label, dir_fd=None):
    if expected == errno.EACCES and os.geteuid() == 0:
        # These rows describe an unprivileged native caller. The guest's
        # deliberate DAC-independent protection is asserted separately; do not
        # pretend uid 0 supplies the same native oracle.
        return "SKIPPED_ROOT_CREDENTIAL"
    error, status, mode = probe(path, flags, dir_fd)
    if error != expected:
        raise AssertionError(
            f"{label}: path={path} flags={flags:#x} errno={error} expected={expected} "
            f"status={status} mode={mode} dir_fd={dir_fd}"
        )
    return "success" if error is None else errno.errorcode[error]


policy_error, _, _ = probe(
    "/proc/self/status", os.O_RDONLY | os.O_CREAT | os.O_DIRECTORY | os.O_CLOEXEC
)
if policy_error == errno.EINVAL:
    create_directory_policy = "EarlyEinval"
elif policy_error == errno.ENOTDIR:
    create_directory_policy = "LegacyLookup"
else:
    raise AssertionError(f"unsupported create-directory policy errno={policy_error}")
print(f"create-directory-policy={create_directory_policy} errno={policy_error}")


def create_directory_expected(path, flags):
    if create_directory_policy == "EarlyEinval":
        return errno.EINVAL
    if flags & os.O_EXCL:
        return errno.EEXIST
    # Linux v6.3 follows the existing /proc/mounts symlink far enough to reject
    # O_DIRECTORY with ENOTDIR.  O_NOFOLLOW does not replace that result with
    # ELOOP when O_CREAT is also present (fs/namei.c:3543).
    return errno.ENOTDIR


direct_counts = collections.Counter()
for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for directory in [0, os.O_DIRECTORY]:
                    flags = access | trunc | direct | directory | os.O_NOFOLLOW
                    if directory:
                        expected = errno.ENOTDIR
                    elif path == "/proc/mounts":
                        expected = errno.ELOOP
                    elif access != os.O_RDONLY or trunc:
                        expected = errno.EACCES
                    elif direct:
                        expected = errno.EINVAL
                    else:
                        expected = None
                    direct_counts[check(path, flags, expected, "direct-nofollow")] += 1

for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for nofollow in [0, os.O_NOFOLLOW]:
                    flags = access | os.O_CREAT | os.O_EXCL | trunc | direct | nofollow
                    direct_counts[
                        check(path, flags, errno.EEXIST, "direct-create-exclusive")
                    ] += 1

for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for exclusive in [0, os.O_EXCL]:
            for trunc in [0, os.O_TRUNC]:
                for direct in [0, os.O_DIRECT]:
                    for nofollow in [0, os.O_NOFOLLOW]:
                        flags = (
                            access
                            | os.O_CREAT
                            | os.O_DIRECTORY
                            | exclusive
                            | trunc
                            | direct
                            | nofollow
                        )
                        expected = create_directory_expected(path, flags)
                        direct_counts[
                            check(path, flags, expected, "direct-create-directory")
                        ] += 1

for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for creation in [0, os.O_CREAT, os.O_EXCL, os.O_CREAT | os.O_EXCL]:
            for trunc in [0, os.O_TRUNC]:
                for direct in [0, os.O_DIRECT]:
                    for nofollow in [0, os.O_NOFOLLOW]:
                        for directory in [0, os.O_DIRECTORY]:
                            flags = (
                                os.O_PATH
                                | access
                                | creation
                                | trunc
                                | direct
                                | nofollow
                                | directory
                            )
                            expected = errno.ENOTDIR if directory else None
                            direct_counts[
                                check(path, flags, expected, "direct-path")
                            ] += 1
                            if expected is None:
                                error, status, mode = probe(path, flags)
                                wanted = os.O_PATH | (os.O_NOFOLLOW if nofollow else 0)
                                if (
                                    error is not None
                                    or status & (os.O_PATH | os.O_NOFOLLOW) != wanted
                                ):
                                    raise AssertionError(
                                        f"direct-path-status path={path} flags={flags:#x} "
                                        f"errno={error} status={status} wanted={wanted:#x}"
                                    )
                                expected_mode = (
                                    stat.S_IFLNK
                                    if path == "/proc/mounts" and nofollow
                                    else stat.S_IFREG
                                )
                                if mode != expected_mode:
                                    raise AssertionError(
                                        f"direct-path-mode path={path} flags={flags:#x} "
                                        f"mode={mode:#x} expected={expected_mode:#x}"
                                    )

print(
    f"combined-direct total={sum(direct_counts.values())} "
    f"results={dict(sorted(direct_counts.items()))}"
)

source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    procfd = f"/proc/self/fd/{source}"
    procfd_counts = collections.Counter()
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for path_only in [0, os.O_PATH]:
                    flags = (
                        access
                        | trunc
                        | direct
                        | path_only
                        | os.O_DIRECTORY
                        | os.O_NOFOLLOW
                    )
                    procfd_counts[
                        check(procfd, flags, errno.ENOTDIR, "procfd-directory-nofollow")
                    ] += 1
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                flags = access | trunc | direct | os.O_NOFOLLOW
                procfd_counts[
                    check(procfd, flags, errno.ELOOP, "procfd-nofollow")
                ] += 1
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for nofollow in [0, os.O_NOFOLLOW]:
                    flags = access | os.O_CREAT | os.O_EXCL | trunc | direct | nofollow
                    procfd_counts[
                        check(procfd, flags, errno.EEXIST, "procfd-create-exclusive")
                    ] += 1
    print(
        f"combined-procfd total={sum(procfd_counts.values())} "
        f"results={dict(sorted(procfd_counts.items()))}"
    )
finally:
    os.close(source)


tmpfile_cases = [
    ("readonly", os.O_RDONLY | os.O_TMPFILE, errno.EINVAL),
    ("readonly-nofollow", os.O_RDONLY | os.O_TMPFILE | os.O_NOFOLLOW, errno.EINVAL),
    ("readonly-truncate", os.O_RDONLY | os.O_TMPFILE | os.O_TRUNC, errno.EINVAL),
    ("readonly-direct", os.O_RDONLY | os.O_TMPFILE | os.O_DIRECT, errno.EINVAL),
    ("readonly-exclusive", os.O_RDONLY | os.O_TMPFILE | os.O_EXCL, errno.EINVAL),
    (
        "readonly-all-extras",
        os.O_RDONLY
        | os.O_TMPFILE
        | os.O_NOFOLLOW
        | os.O_TRUNC
        | os.O_DIRECT
        | os.O_EXCL,
        errno.EINVAL,
    ),
    ("writeonly", os.O_WRONLY | os.O_TMPFILE, errno.ENOTDIR),
    ("readwrite", os.O_RDWR | os.O_TMPFILE, errno.ENOTDIR),
    ("path", os.O_PATH | os.O_TMPFILE, errno.ENOTDIR),
    (
        "path-all-extras",
        os.O_PATH
        | os.O_TMPFILE
        | os.O_NOFOLLOW
        | os.O_TRUNC
        | os.O_DIRECT
        | os.O_EXCL,
        errno.ENOTDIR,
    ),
]

tmpfile_counts = collections.Counter()
fdinfo_source = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC)
try:
    fdinfo = f"/proc/self/fdinfo/{fdinfo_source}"
    for path in [*paths, fdinfo]:
        for label, flags, expected in tmpfile_cases:
            tmpfile_counts[
                check(path, flags, expected, f"tmpfile-{label}")
            ] += 1
finally:
    os.close(fdinfo_source)

print(
    f"tmpfile-direct-and-fdinfo total={sum(tmpfile_counts.values())} "
    f"results={dict(sorted(tmpfile_counts.items()))}"
)


libc = ctypes.CDLL(None, use_errno=True)
libc.openat.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int, ctypes.c_uint]
libc.openat.restype = ctypes.c_int


def raw_openat_error(dir_fd, path_pointer, flags):
    ctypes.set_errno(0)
    descriptor = libc.openat(dir_fd, path_pointer, flags, 0o600)
    if descriptor >= 0:
        os.close(descriptor)
        return None
    return ctypes.get_errno()


def raw_path_pointer(path):
    storage = ctypes.create_string_buffer(os.fsencode(path) + b"\0")
    return storage, ctypes.cast(storage, ctypes.c_void_p)


readonly_tmpfile = os.O_RDONLY | os.O_TMPFILE
controls = [
    os.O_WRONLY | os.O_TMPFILE,
    os.O_PATH | os.O_TMPFILE,
    os.O_RDONLY | os.O_DIRECTORY,
]
ordering_counts = collections.Counter()
ordering_cases = []
ordering_cases.append(("bad-pointer", AT_FDCWD, ctypes.c_void_p(-1), errno.EFAULT))
for label, dir_fd, path, expected in [
    ("empty", AT_FDCWD, "", errno.ENOENT),
    ("invalid-dirfd", 123456, "missing-open-precedence", errno.EBADF),
    ("missing", AT_FDCWD, "missing-open-precedence", errno.ENOENT),
    ("missing-fdinfo", AT_FDCWD, "/proc/self/fdinfo/999999", errno.ENOENT),
    ("missing-procfd", AT_FDCWD, "/proc/self/fd/999999", errno.ENOENT),
    ("fixed", AT_FDCWD, "/proc/uptime", errno.ENOTDIR),
]:
    storage, pointer = raw_path_pointer(path)
    ordering_cases.append((label, dir_fd, pointer, expected, storage))

ordinary = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC)
proc_source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    for label, path in [
        ("fdinfo", f"/proc/self/fdinfo/{ordinary}"),
        ("procfd", f"/proc/self/fd/{proc_source}"),
    ]:
        storage, pointer = raw_path_pointer(path)
        ordering_cases.append((label, AT_FDCWD, pointer, errno.ENOTDIR, storage))

    for case in ordering_cases:
        label, dir_fd, pointer, control_error = case[:4]
        actual = raw_openat_error(dir_fd, pointer, readonly_tmpfile)
        if actual != errno.EINVAL:
            raise AssertionError(
                f"global-tmpfile path={label} readonly errno={actual} expected={errno.EINVAL}"
            )
        ordering_counts[errno.errorcode[actual]] += 1
        for flags in controls:
            actual = raw_openat_error(dir_fd, pointer, flags)
            if actual != control_error:
                raise AssertionError(
                    f"global-tmpfile path={label} control={flags:#x} "
                    f"errno={actual} expected={control_error}"
                )
            ordering_counts[errno.errorcode[actual]] += 1
finally:
    os.close(proc_source)
    os.close(ordinary)

print(
    f"global-tmpfile-ordering total={sum(ordering_counts.values())} "
    f"results={dict(sorted(ordering_counts.items()))}"
)
