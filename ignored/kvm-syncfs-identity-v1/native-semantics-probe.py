import ctypes
import errno
import fcntl
import mmap
import os


libc = ctypes.CDLL(None, use_errno=True)
libc.mmap.restype = ctypes.c_void_p
libc.mmap.argtypes = [
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_long,
]
libc.mprotect.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
libc.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]


def result(name, call):
    ctypes.set_errno(0)
    value = call()
    error = ctypes.get_errno()
    print(f"{name}: result={value} errno={error} ({errno.errorcode.get(error, 'NONE')})")
    return value


proc = os.open("/proc/uptime", os.O_RDONLY | os.O_CLOEXEC)
try:
    result("real-proc-write", lambda: libc.write(proc, ctypes.c_char_p(b"x"), 1))
    result("real-proc-pwrite", lambda: libc.pwrite(proc, ctypes.c_char_p(b"x"), 1, 0))
    result("real-proc-ftruncate", lambda: libc.ftruncate(proc, 0))
    result("real-proc-fallocate", lambda: libc.fallocate(proc, 0, 0, 1))
    result("real-proc-fchmod", lambda: libc.fchmod(proc, 0o666))
    try:
        reopened = os.open(f"/proc/self/fd/{proc}", os.O_RDWR | os.O_CLOEXEC)
    except OSError as error:
        print(f"real-proc-rdwr-reopen: result=-1 errno={error.errno} ({errno.errorcode[error.errno]})")
    else:
        print(f"real-proc-rdwr-reopen: result={reopened} errno=0 (NONE)")
        os.close(reopened)
finally:
    os.close(proc)

proc_path = os.open("/proc/uptime", os.O_PATH | os.O_CLOEXEC)
try:
    result("real-proc-opath-fchmod", lambda: libc.fchmod(proc_path, 0o666))
finally:
    os.close(proc_path)

writer = os.memfd_create("sealed-probe", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
os.write(writer, b"x" * mmap.PAGESIZE)
expected_seals = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL
fcntl.fcntl(writer, fcntl.F_ADD_SEALS, expected_seals)
reader = os.open(f"/proc/self/fd/{writer}", os.O_RDONLY | os.O_CLOEXEC)
read_write = os.open(f"/proc/self/fd/{writer}", os.O_RDWR | os.O_CLOEXEC)
try:
    print(f"sealed-reader-flags: {fcntl.fcntl(reader, fcntl.F_GETFL) & os.O_ACCMODE}")
    print(f"sealed-reader-seals: {fcntl.fcntl(reader, fcntl.F_GET_SEALS)}")
    result("sealed-reader-ftruncate", lambda: libc.ftruncate(reader, 0))

    shared_read = result(
        "sealed-reader-shared-read-mmap",
        lambda: libc.mmap(None, mmap.PAGESIZE, mmap.PROT_READ, mmap.MAP_SHARED, reader, 0),
    )
    if shared_read != ctypes.c_void_p(-1).value:
        result(
            "sealed-reader-shared-mprotect-write",
            lambda: libc.mprotect(shared_read, mmap.PAGESIZE, mmap.PROT_READ | mmap.PROT_WRITE),
        )
        libc.munmap(shared_read, mmap.PAGESIZE)

    result(
        "sealed-rdwr-shared-write-mmap",
        lambda: libc.mmap(
            None,
            mmap.PAGESIZE,
            mmap.PROT_READ | mmap.PROT_WRITE,
            mmap.MAP_SHARED,
            read_write,
            0,
        ),
    )
finally:
    os.close(read_write)
    os.close(reader)
    os.close(writer)
