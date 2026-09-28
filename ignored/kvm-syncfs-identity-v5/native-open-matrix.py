import collections
import errno
import os
import runpy


# Retain the complete v4 pwrite, status-flag, combined direct-open, and
# proc-fd precedence matrix, then extend it with O_TMPFILE access ordering.
base = runpy.run_path("ignored/kvm-syncfs-identity-v4/native-open-matrix.py")
paths = base["paths"]

cases = [
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


def check_tmpfile(path, label, flags, expected):
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as error:
        if error.errno != expected:
            raise AssertionError(
                f"tmpfile path={path} case={label} flags={flags:#x} "
                f"errno={error.errno} expected={expected}"
            ) from error
        return errno.errorcode[error.errno]
    else:
        os.close(descriptor)
        raise AssertionError(
            f"tmpfile path={path} case={label} flags={flags:#x} unexpectedly succeeded"
        )


tmpfile_counts = collections.Counter()
for path in paths:
    for label, flags, expected in cases:
        tmpfile_counts[check_tmpfile(path, label, flags, expected)] += 1

source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    fdinfo = f"/proc/self/fdinfo/{source}"
    for label, flags, expected in cases:
        tmpfile_counts[check_tmpfile(fdinfo, label, flags, expected)] += 1
finally:
    os.close(source)

print(
    f"tmpfile-direct-and-fdinfo total={sum(tmpfile_counts.values())} "
    f"results={dict(sorted(tmpfile_counts.items()))}"
)
