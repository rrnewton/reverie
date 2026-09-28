import ctypes
import errno
import fcntl
import os
import platform


PATHS = [
    "/proc/uptime",
    "/proc/loadavg",
    "/proc/version",
    "/proc/filesystems",
    "/proc/mounts",
    "/proc/self/mounts",
    "/proc/self/mountinfo",
    "/proc/stat",
    "/proc/meminfo",
    "/proc/cpuinfo",
    "/proc/locks",
    "/proc/self/stat",
    "/proc/self/status",
    "/proc/self/cmdline",
    "/proc/vmstat",
    "/proc/sys/kernel/osrelease",
    "/proc/self/maps",
]

libc = ctypes.CDLL(None, use_errno=True)
libc.pwrite.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_longlong]
libc.pwrite.restype = ctypes.c_ssize_t
payload = ctypes.create_string_buffer(b"x")


def name(error):
    return errno.errorcode.get(error, str(error))


def pwrite_result(descriptor, count, offset):
    ctypes.set_errno(0)
    result = libc.pwrite(descriptor, payload, count, offset)
    error = ctypes.get_errno()
    return f"result={result} errno={error}({name(error)})"


def open_result(path, flags):
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        return f"result=-1 errno={error.errno}({name(error.errno)})"
    try:
        status = fcntl.fcntl(descriptor, fcntl.F_GETFL)
        mode = os.fstat(descriptor).st_mode
        return f"result=fd F_GETFL={status:#x} mode={mode:#o}"
    finally:
        os.close(descriptor)


print(f"kernel={platform.release()} euid={os.geteuid()}")
for path in PATHS:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC)
    try:
        print(
            f"pwrite path={path} count1_offset0={pwrite_result(descriptor, 1, 0)} "
            f"count0_offset0={pwrite_result(descriptor, 0, 0)} "
            f"count0_offset-1={pwrite_result(descriptor, 0, -1)}"
        )
    finally:
        os.close(descriptor)

direct_flags = [
    ("rdonly_directory", os.O_RDONLY | os.O_DIRECTORY),
    ("wronly_directory", os.O_WRONLY | os.O_DIRECTORY),
    ("rdwr_directory", os.O_RDWR | os.O_DIRECTORY),
    ("path_directory", os.O_PATH | os.O_DIRECTORY),
    ("rdonly_trunc", os.O_RDONLY | os.O_TRUNC),
    ("wronly_trunc", os.O_WRONLY | os.O_TRUNC),
    ("rdwr_trunc", os.O_RDWR | os.O_TRUNC),
    ("rdonly_direct", os.O_RDONLY | os.O_DIRECT),
    ("rdonly_nonblock", os.O_RDONLY | os.O_NONBLOCK),
    ("rdonly_append", os.O_RDONLY | os.O_APPEND),
    ("rdonly_sync", os.O_RDONLY | os.O_SYNC),
    ("rdonly_dsync", os.O_RDONLY | os.O_DSYNC),
    ("rdonly_nofollow", os.O_RDONLY | os.O_NOFOLLOW),
    ("path_ignored", os.O_PATH | os.O_TRUNC | os.O_DIRECT | os.O_APPEND | os.O_SYNC | os.O_NONBLOCK),
    ("path_nofollow", os.O_PATH | os.O_NOFOLLOW),
]
for path in PATHS:
    for label, flags in direct_flags:
        print(f"open path={path} flags={label} {open_result(path, flags)}")

source = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    procfd = f"/proc/self/fd/{source}"
    for label, flags in [
        ("rdonly_trunc", os.O_RDONLY | os.O_TRUNC),
        ("rdonly_direct", os.O_RDONLY | os.O_DIRECT),
        ("rdonly_directory", os.O_RDONLY | os.O_DIRECTORY),
        ("path_directory", os.O_PATH | os.O_DIRECTORY),
        ("path_nofollow", os.O_PATH | os.O_NOFOLLOW),
        ("rdonly_status", os.O_RDONLY | os.O_NONBLOCK | os.O_APPEND | os.O_SYNC),
    ]:
        print(f"procfd flags={label} {open_result(procfd, flags)}")
finally:
    os.close(source)

for access_label, access in [("rdonly", os.O_RDONLY), ("rdwr", os.O_RDWR)]:
    descriptor = os.open("/proc/self/loginuid", access | os.O_CLOEXEC)
    try:
        print(
            f"loginuid access={access_label} F_GETFL={fcntl.fcntl(descriptor, fcntl.F_GETFL):#x} "
            f"count1_offset0={pwrite_result(descriptor, 1, 0)} "
            f"count0_offset-1={pwrite_result(descriptor, 0, -1)}"
        )
    finally:
        os.close(descriptor)

try:
    with open("/proc/sys/vm/memfd_noexec", encoding="ascii") as setting:
        print(f"vm.memfd_noexec={setting.read().strip()}")
except OSError as error:
    print(f"vm.memfd_noexec=unavailable errno={error.errno}({name(error.errno)})")
