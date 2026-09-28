import collections
import ctypes
import errno
import fcntl
import os
import runpy
import tempfile


# Retain every v6 native oracle before adding the v7 admission and transport
# matrices.  run_path returns its helpers and its single probed host policy.
base = runpy.run_path("ignored/kvm-syncfs-identity-v6/native-open-matrix.py")
raw_openat_error = base["raw_openat_error"]
raw_path_pointer = base["raw_path_pointer"]
AT_FDCWD = base["AT_FDCWD"]
policy = base["create_directory_policy"]

private_tmpfile = os.O_TMPFILE & ~os.O_DIRECTORY
invalid_tmpfile = [
    os.O_RDONLY | os.O_TMPFILE,
    private_tmpfile,
    os.O_WRONLY | private_tmpfile,
    os.O_WRONLY | os.O_TMPFILE | os.O_CREAT,
    os.O_RDWR | os.O_TMPFILE | os.O_CREAT,
    os.O_ACCMODE | os.O_TMPFILE | os.O_CREAT,
]
valid_controls = [
    os.O_WRONLY | os.O_TMPFILE,
    os.O_RDWR | os.O_TMPFILE,
    os.O_ACCMODE | os.O_TMPFILE,
    os.O_PATH | os.O_TMPFILE,
    os.O_RDONLY | os.O_DIRECTORY,
]

tmpfile_ordering = [("bad-pointer", AT_FDCWD, ctypes.c_void_p(-1), errno.EFAULT)]
for label, dir_fd, path, expected in [
    ("empty", AT_FDCWD, "", errno.ENOENT),
    ("invalid-dirfd", 123456, "missing-v7-open-precedence", errno.EBADF),
    ("missing", AT_FDCWD, "missing-v7-open-precedence", errno.ENOENT),
    ("missing-fdinfo", AT_FDCWD, "/proc/self/fdinfo/999999", errno.ENOENT),
    ("missing-procfd", AT_FDCWD, "/proc/self/fd/999999", errno.ENOENT),
    ("fixed", AT_FDCWD, "/proc/uptime", errno.ENOTDIR),
]:
    storage, pointer = raw_path_pointer(path)
    tmpfile_ordering.append((label, dir_fd, pointer, expected, storage))

ordinary = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC)
proc_source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    for label, path in [
        ("fdinfo", f"/proc/self/fdinfo/{ordinary}"),
        ("procfd", f"/proc/self/fd/{proc_source}"),
    ]:
        storage, pointer = raw_path_pointer(path)
        tmpfile_ordering.append((label, AT_FDCWD, pointer, errno.ENOTDIR, storage))

    tmpfile_counts = collections.Counter()
    for case in tmpfile_ordering:
        label, dir_fd, pointer, control_error = case[:4]
        for flags in invalid_tmpfile:
            actual = raw_openat_error(dir_fd, pointer, flags)
            if actual != errno.EINVAL:
                raise AssertionError(
                    f"v7-tmpfile-invalid case={label} flags={flags:#x} "
                    f"errno={actual} expected={errno.EINVAL}"
                )
            tmpfile_counts[errno.errorcode[actual]] += 1
        for flags in valid_controls:
            actual = raw_openat_error(dir_fd, pointer, flags)
            if actual != control_error:
                raise AssertionError(
                    f"v7-tmpfile-control case={label} flags={flags:#x} "
                    f"errno={actual} expected={control_error}"
                )
            tmpfile_counts[errno.errorcode[actual]] += 1
finally:
    os.close(proc_source)
    os.close(ordinary)

print(
    f"v7-global-tmpfile total={sum(tmpfile_counts.values())} "
    f"results={dict(sorted(tmpfile_counts.items()))}"
)


def policy_expected(context_error, exclusive=False):
    if policy == "EarlyEinval":
        return errno.EINVAL
    if context_error == errno.ENOTDIR and exclusive:
        return errno.EEXIST
    return context_error


create_directory_cases = [("bad-pointer", AT_FDCWD, ctypes.c_void_p(-1), errno.EFAULT)]
for label, dir_fd, path, legacy_error in [
    ("empty", AT_FDCWD, "", errno.ENOENT),
    ("invalid-dirfd", 123456, "missing-v7-open-precedence", errno.EBADF),
    ("missing", AT_FDCWD, "missing-v7-open-precedence", errno.ENOENT),
    ("missing-fdinfo", AT_FDCWD, "/proc/self/fdinfo/999999", errno.ENOENT),
    ("missing-procfd", AT_FDCWD, "/proc/self/fd/999999", errno.ENOENT),
    ("fixed", AT_FDCWD, "/proc/uptime", errno.ENOTDIR),
]:
    storage, pointer = raw_path_pointer(path)
    create_directory_cases.append((label, dir_fd, pointer, legacy_error, storage))

ordinary = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC)
proc_source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    for label, path in [
        ("fdinfo", f"/proc/self/fdinfo/{ordinary}"),
        ("procfd", f"/proc/self/fd/{proc_source}"),
    ]:
        storage, pointer = raw_path_pointer(path)
        create_directory_cases.append((label, AT_FDCWD, pointer, errno.ENOTDIR, storage))

    create_directory_counts = collections.Counter()
    for case in create_directory_cases:
        label, dir_fd, pointer, legacy_error = case[:4]
        for exclusive in [False, True]:
            flags = os.O_RDONLY | os.O_CREAT | os.O_DIRECTORY
            if exclusive:
                flags |= os.O_EXCL
            expected = policy_expected(legacy_error, exclusive)
            actual = raw_openat_error(dir_fd, pointer, flags)
            if actual != expected:
                raise AssertionError(
                    f"v7-create-directory case={label} exclusive={exclusive} "
                    f"policy={policy} errno={actual} expected={expected}"
                )
            create_directory_counts[errno.errorcode[actual]] += 1
finally:
    os.close(proc_source)
    os.close(ordinary)

print(
    f"v7-global-create-directory total={sum(create_directory_counts.values())} "
    f"results={dict(sorted(create_directory_counts.items()))}"
)


def raw_open_status(path_pointer, flags):
    ctypes.set_errno(0)
    descriptor = base["libc"].openat(AT_FDCWD, path_pointer, flags, 0o600)
    if descriptor < 0:
        return ctypes.get_errno(), None
    try:
        return None, fcntl.fcntl(descriptor, fcntl.F_GETFL)
    finally:
        os.close(descriptor)


ignored_opath_extras = [
    os.O_WRONLY,
    os.O_RDWR,
    os.O_ACCMODE,
    os.O_CREAT,
    os.O_EXCL,
    os.O_TRUNC,
    os.O_DIRECT,
    os.O_APPEND,
    os.O_SYNC,
    os.O_NONBLOCK,
    os.O_NOATIME,
    private_tmpfile,
]
opath_counts = collections.Counter()
with tempfile.TemporaryDirectory(prefix="reverie-v7-opath-") as directory:
    regular = os.path.join(directory, "ordinary")
    with open(regular, "wb") as output:
        output.write(b"x")
    missing_storage, missing_pointer = raw_path_pointer(os.path.join(directory, "missing"))
    regular_storage, regular_pointer = raw_path_pointer(regular)
    directory_storage, directory_pointer = raw_path_pointer(directory)
    for extra in ignored_opath_extras:
        flags = os.O_PATH | extra
        for label, pointer, expected in [
            ("bad-pointer", ctypes.c_void_p(-1), errno.EFAULT),
            ("missing", missing_pointer, errno.ENOENT),
        ]:
            actual = raw_openat_error(AT_FDCWD, pointer, flags)
            if actual != expected:
                raise AssertionError(
                    f"v7-opath-{label} extra={extra:#x} errno={actual} expected={expected}"
                )
            opath_counts[errno.errorcode[actual]] += 1
        for label, pointer in [
            ("regular", regular_pointer),
            ("directory", directory_pointer),
        ]:
            error, status = raw_open_status(pointer, flags)
            if error is not None or status & os.O_PATH == 0:
                raise AssertionError(
                    f"v7-opath-{label} extra={extra:#x} errno={error} status={status}"
                )
            ignored = (
                os.O_ACCMODE
                | os.O_CREAT
                | os.O_EXCL
                | os.O_TRUNC
                | os.O_DIRECT
                | os.O_APPEND
                | os.O_SYNC
                | os.O_NONBLOCK
                | os.O_NOATIME
                | private_tmpfile
            )
            if status & ignored:
                raise AssertionError(
                    f"v7-opath-{label} extra={extra:#x} retained={status & ignored:#x}"
                )
            opath_counts["success"] += 1

print(
    f"v7-ordinary-opath total={sum(opath_counts.values())} "
    f"results={dict(sorted(opath_counts.items()))}"
)
