import collections
import ctypes
import errno
import os
import runpy
import stat


# Preserve every v3-v7 oracle, then add the v8 live-fdinfo ordering matrix.
v7 = runpy.run_path("ignored/kvm-syncfs-identity-v7/native-open-matrix.py")
check = v7["base"]["check"]
policy = v7["policy"]

libc = ctypes.CDLL(None, use_errno=True)
libc.eventfd.argtypes = [ctypes.c_uint, ctypes.c_int]
libc.eventfd.restype = ctypes.c_int


class SigSet(ctypes.Structure):
    _fields_ = [("words", ctypes.c_ulong * 16)]


libc.sigemptyset.argtypes = [ctypes.POINTER(SigSet)]
libc.sigemptyset.restype = ctypes.c_int
libc.signalfd.argtypes = [ctypes.c_int, ctypes.POINTER(SigSet), ctypes.c_int]
libc.signalfd.restype = ctypes.c_int

signal_mask = SigSet()
if libc.sigemptyset(ctypes.byref(signal_mask)) != 0:
    raise AssertionError(f"sigemptyset failed errno={ctypes.get_errno()}")
signal_fd = libc.signalfd(-1, ctypes.byref(signal_mask), os.O_NONBLOCK | os.O_CLOEXEC)
if signal_fd < 0:
    raise AssertionError(f"signalfd failed errno={ctypes.get_errno()}")
event_fd = libc.eventfd(0, os.O_NONBLOCK | os.O_CLOEXEC)
if event_fd < 0:
    os.close(signal_fd)
    raise AssertionError(f"eventfd failed errno={ctypes.get_errno()}")

owned = [
    os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC),
    os.open("/dev/urandom", os.O_RDONLY | os.O_CLOEXEC),
    signal_fd,
    event_fd,
]
targets = [
    ("proc", owned[0]),
    ("random", owned[1]),
    ("signalfd", signal_fd),
    ("stdout", 1),
    ("anonymous-inode", event_fd),
]
plain_create_directory = os.O_RDONLY | os.O_CREAT | os.O_DIRECTORY
exclusive_create_directory = plain_create_directory | os.O_EXCL
create_directory_error = errno.EINVAL if policy == "EarlyEinval" else errno.ENOTDIR
exclusive_create_directory_error = errno.EINVAL if policy == "EarlyEinval" else errno.EEXIST
counts = collections.Counter()
try:
    for label, descriptor in targets:
        path = f"/proc/self/fdinfo/{descriptor}"
        metadata = os.stat(path)
        if (
            stat.S_IFMT(metadata.st_mode) != stat.S_IFREG
            or metadata.st_mode & 0o777 != 0o444
            or metadata.st_nlink != 1
            or metadata.st_size != 0
        ):
            raise AssertionError(
                f"fdinfo metadata label={label} mode={metadata.st_mode:#o} "
                f"nlink={metadata.st_nlink} size={metadata.st_size}"
            )
        for case, flags, expected in [
            ("read", os.O_RDONLY, None),
            ("directory", os.O_RDONLY | os.O_DIRECTORY, errno.ENOTDIR),
            ("tmpfile", os.O_WRONLY | os.O_TMPFILE, errno.ENOTDIR),
            ("create-directory", plain_create_directory, create_directory_error),
            (
                "create-exclusive-directory",
                exclusive_create_directory,
                exclusive_create_directory_error,
            ),
        ]:
            counts[check(path, flags, expected, f"v8-live-fdinfo-{label}-{case}")] += 1
finally:
    for descriptor in owned:
        os.close(descriptor)

print(
    f"v8-live-fdinfo total={sum(counts.values())} "
    f"results={dict(sorted(counts.items()))}"
)
