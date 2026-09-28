import collections
import ctypes
import errno
import fcntl
import os
import runpy


# Preserve and execute the complete v3-v8 native oracle chain first.
# The local v6 copy labels and skips only its credential-sensitive EACCES rows
# when invoked as uid 0; every credential-independent row still runs.
v8 = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "native-open-matrix-v8.py")
)
policy = v8["policy"]


def open_errno(path, flags):
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as error:
        return error.errno
    os.close(descriptor)
    return None


def require_open_errno(path, flags, expected, label):
    actual = open_errno(path, flags)
    if actual != expected:
        raise AssertionError(
            f"{label}: path={path} flags={flags:#x} policy={policy} "
            f"errno={actual} expected={expected}"
        )
    return "success" if actual is None else errno.errorcode[actual]


create_directory = os.O_RDONLY | os.O_CREAT | os.O_DIRECTORY
policy_probe = open_errno("/proc/self/status", create_directory | os.O_CLOEXEC)
if policy == "EarlyEinval":
    if policy_probe != errno.EINVAL:
        raise AssertionError(
            f"stale policy classification: policy={policy} probe={policy_probe}"
        )
elif policy == "LegacyLookup":
    if policy_probe != errno.ENOTDIR:
        raise AssertionError(
            f"stale policy classification: policy={policy} probe={policy_probe}"
        )
else:
    raise AssertionError(f"unsupported create-directory policy {policy!r}")
print(f"v9-current-create-directory-policy={policy} errno={policy_probe}")


def existing_create_directory_expected(exclusive):
    if policy == "EarlyEinval":
        return errno.EINVAL
    return errno.EEXIST if exclusive else errno.ENOTDIR


def negative_fdinfo_create_directory_expected():
    if policy == "EarlyEinval":
        return errno.EINVAL
    # Linux v6.3 fs/proc/fd.c returns ENOENT directly for a missing or malformed
    # numeric final component, before namei's generic O_EXCL create fallback.
    return errno.ENOENT


def fdinfo_directory_create_directory_expected():
    if policy == "EarlyEinval":
        return errno.EINVAL
    # With a trailing slash the resolved object is the fdinfo directory itself.
    return errno.EISDIR


mount_counts = collections.Counter()
for exclusive in [False, True]:
    for nofollow in [False, True]:
        flags = create_directory
        if exclusive:
            flags |= os.O_EXCL
        if nofollow:
            flags |= os.O_NOFOLLOW
        expected = existing_create_directory_expected(exclusive)
        mount_counts[
            require_open_errno(
                "/proc/mounts",
                flags,
                expected,
                f"v9-mounts-create-directory-exclusive={exclusive}-nofollow={nofollow}",
            )
        ] += 1

# O_NOFOLLOW still rejects the /proc/mounts symlink when the create-directory
# combination is absent.  This control prevents the legacy correction above
# from accidentally normalizing ordinary symlink lookup.
mount_counts[
    require_open_errno(
        "/proc/mounts",
        os.O_RDONLY | os.O_NOFOLLOW,
        errno.ELOOP,
        "v9-mounts-ordinary-nofollow-control",
    )
] += 1
print(
    f"v9-mounts-create-directory total={sum(mount_counts.values())} "
    f"results={dict(sorted(mount_counts.items()))}"
)


fdinfo_counts = collections.Counter()
target = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    live_fdinfo = f"/proc/self/fdinfo/{target}"
    fdinfo_paths = [
        ("live", live_fdinfo, lambda exclusive: existing_create_directory_expected(exclusive)),
        (
            "missing-numeric",
            "/proc/self/fdinfo/999999",
            lambda _exclusive: negative_fdinfo_create_directory_expected(),
        ),
        (
            "malformed-text",
            "/proc/self/fdinfo/not-a-fd",
            lambda _exclusive: negative_fdinfo_create_directory_expected(),
        ),
        (
            "malformed-leading-zero",
            "/proc/self/fdinfo/03",
            lambda _exclusive: negative_fdinfo_create_directory_expected(),
        ),
        (
            "trailing-slash",
            "/proc/self/fdinfo/",
            lambda _exclusive: fdinfo_directory_create_directory_expected(),
        ),
    ]
    for path_label, path, expected_for_exclusive in fdinfo_paths:
        for exclusive in [False, True]:
            for nofollow in [False, True]:
                flags = create_directory
                if exclusive:
                    flags |= os.O_EXCL
                if nofollow:
                    flags |= os.O_NOFOLLOW
                expected = expected_for_exclusive(exclusive)
                fdinfo_counts[
                    require_open_errno(
                        path,
                        flags,
                        expected,
                        f"v9-fdinfo-{path_label}-exclusive={exclusive}-nofollow={nofollow}",
                    )
                ] += 1

    readable_fdinfo = os.open(live_fdinfo, os.O_RDONLY | os.O_CLOEXEC)
    try:
        status = fcntl.fcntl(readable_fdinfo, fcntl.F_GETFL)
        if status & os.O_ACCMODE != os.O_RDONLY:
            raise AssertionError(
                f"live fdinfo is not O_RDONLY: fd={readable_fdinfo} F_GETFL={status:#x}"
            )
        contents = os.read(readable_fdinfo, 4096)
        if not contents:
            raise AssertionError(f"live fdinfo was not readable: path={live_fdinfo}")

        syscall_numbers = {
            "x86_64": 18,
            "amd64": 18,
            "aarch64": 68,
            "arm64": 68,
            "riscv64": 68,
        }
        machine = os.uname().machine.lower()
        try:
            pwrite64_syscall = syscall_numbers[machine]
        except KeyError as error:
            raise AssertionError(
                f"raw pwrite64 oracle has no syscall number for architecture {machine!r}"
            ) from error

        libc = ctypes.CDLL(None, use_errno=True)
        libc.syscall.restype = ctypes.c_long
        payload = ctypes.create_string_buffer(b"x")
        valid_pointer = ctypes.cast(payload, ctypes.c_void_p)
        bad_pointer = ctypes.c_void_p(-1)

        def require_pwrite64_errno(label, descriptor, pointer, count, offset, expected):
            ctypes.set_errno(0)
            result = libc.syscall(
                ctypes.c_long(pwrite64_syscall),
                ctypes.c_int(descriptor),
                pointer,
                ctypes.c_size_t(count),
                ctypes.c_longlong(offset),
            )
            actual = ctypes.get_errno()
            if result != -1 or actual != expected:
                raise AssertionError(
                    f"{label}: result={result} errno={actual} expected=-1/{expected}"
                )
            return errno.errorcode[actual]

        pwrite_counts = collections.Counter()
        for label, descriptor, pointer, count, offset, expected in [
            ("one-byte-offset-zero", readable_fdinfo, valid_pointer, 1, 0, errno.ESPIPE),
            ("zero-byte-offset-zero", readable_fdinfo, valid_pointer, 0, 0, errno.ESPIPE),
            ("one-byte-negative-offset", readable_fdinfo, valid_pointer, 1, -1, errno.EINVAL),
            ("zero-byte-negative-offset", readable_fdinfo, bad_pointer, 0, -1, errno.EINVAL),
            ("bad-pointer-offset-zero", readable_fdinfo, bad_pointer, 1, 0, errno.ESPIPE),
            ("invalid-fd-offset-zero", -1, valid_pointer, 1, 0, errno.EBADF),
            ("invalid-fd-negative-offset", -1, valid_pointer, 1, -1, errno.EINVAL),
        ]:
            pwrite_counts[
                require_pwrite64_errno(
                    f"v9-fdinfo-pwrite64-{label}",
                    descriptor,
                    pointer,
                    count,
                    offset,
                    expected,
                )
            ] += 1
    finally:
        os.close(readable_fdinfo)
finally:
    os.close(target)

print(
    f"v9-fdinfo-create-directory total={sum(fdinfo_counts.values())} "
    f"results={dict(sorted(fdinfo_counts.items()))}"
)
print(
    f"v9-fdinfo-pwrite64 architecture={machine} syscall={pwrite64_syscall} "
    f"total={sum(pwrite_counts.values())} results={dict(sorted(pwrite_counts.items()))}"
)
