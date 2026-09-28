import collections
import errno
import fcntl
import os
import runpy
import stat


# Re-run and retain the complete v3 pwrite/open/loginuid matrix first.
base = runpy.run_path("ignored/kvm-syncfs-identity-v3/native-proc-matrix.py")
paths = base["PATHS"]


def probe(path, flags):
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as error:
        return error.errno, None, None
    try:
        return None, fcntl.fcntl(descriptor, fcntl.F_GETFL), stat.S_IFMT(os.fstat(descriptor).st_mode)
    finally:
        os.close(descriptor)


def check(path, flags, expected, label):
    error, status, mode = probe(path, flags)
    if error != expected:
        raise AssertionError(
            f"{label}: path={path} flags={flags:#x} errno={error} expected={expected} "
            f"status={status} mode={mode}"
        )
    return "success" if error is None else errno.errorcode[error]


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
                    direct_counts[check(path, flags, errno.EEXIST, "direct-create-exclusive")] += 1

for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for exclusive in [0, os.O_EXCL]:
            for trunc in [0, os.O_TRUNC]:
                for direct in [0, os.O_DIRECT]:
                    for nofollow in [0, os.O_NOFOLLOW]:
                        flags = access | os.O_CREAT | os.O_DIRECTORY | exclusive | trunc | direct | nofollow
                        direct_counts[check(path, flags, errno.EINVAL, "direct-create-directory")] += 1

for path in paths:
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for creation in [0, os.O_CREAT, os.O_EXCL, os.O_CREAT | os.O_EXCL]:
            for trunc in [0, os.O_TRUNC]:
                for direct in [0, os.O_DIRECT]:
                    for nofollow in [0, os.O_NOFOLLOW]:
                        for directory in [0, os.O_DIRECTORY]:
                            flags = os.O_PATH | access | creation | trunc | direct | nofollow | directory
                            expected = errno.ENOTDIR if directory else None
                            direct_counts[check(path, flags, expected, "direct-path")] += 1
                            if expected is None:
                                error, status, mode = probe(path, flags)
                                wanted = os.O_PATH | (os.O_NOFOLLOW if nofollow else 0)
                                if error is not None or status & (os.O_PATH | os.O_NOFOLLOW) != wanted:
                                    raise AssertionError(
                                        f"direct-path-status path={path} flags={flags:#x} "
                                        f"errno={error} status={status} wanted={wanted:#x}"
                                    )
                                expected_mode = stat.S_IFLNK if path == "/proc/mounts" and nofollow else stat.S_IFREG
                                if mode != expected_mode:
                                    raise AssertionError(
                                        f"direct-path-mode path={path} flags={flags:#x} "
                                        f"mode={mode:#x} expected={expected_mode:#x}"
                                    )

print(f"combined-direct total={sum(direct_counts.values())} results={dict(sorted(direct_counts.items()))}")

source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    procfd = f"/proc/self/fd/{source}"
    procfd_counts = collections.Counter()
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for path_only in [0, os.O_PATH]:
                    flags = access | trunc | direct | path_only | os.O_DIRECTORY | os.O_NOFOLLOW
                    procfd_counts[check(procfd, flags, errno.ENOTDIR, "procfd-directory-nofollow")] += 1
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                flags = access | trunc | direct | os.O_NOFOLLOW
                procfd_counts[check(procfd, flags, errno.ELOOP, "procfd-nofollow")] += 1
    for access in [os.O_RDONLY, os.O_WRONLY, os.O_RDWR]:
        for trunc in [0, os.O_TRUNC]:
            for direct in [0, os.O_DIRECT]:
                for nofollow in [0, os.O_NOFOLLOW]:
                    flags = access | os.O_CREAT | os.O_EXCL | trunc | direct | nofollow
                    procfd_counts[check(procfd, flags, errno.EEXIST, "procfd-create-exclusive")] += 1
    print(f"combined-procfd total={sum(procfd_counts.values())} results={dict(sorted(procfd_counts.items()))}")
finally:
    os.close(source)
