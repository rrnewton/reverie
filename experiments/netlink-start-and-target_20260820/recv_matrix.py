#!/usr/bin/env python3
"""Retrieve the SAME populated NETLINK_SOCK_DIAG reply through every kernel
receive syscall that can be issued on a socket fd, and report which ones return
sanitized (zeroed) socket inode numbers and which return raw host values.

Each method gets its own socket and its own dump request, because a dump is
consumed by reading it.

Raw syscalls are issued via libc syscall() so the exact syscall number is under
this script's control, not libc's wrapper choice. On x86_64 there is no `recv`
syscall: recv(2) is a libc wrapper over recvfrom with a NULL address.
"""
import ctypes
import os
import socket
import struct
import sys

libc = ctypes.CDLL("libc.so.6", use_errno=True)

NR = {
    "read": 0,
    "readv": 19,
    "recvfrom": 45,
    "recvmsg": 47,
    "preadv": 295,
    "recvmmsg": 299,
    "preadv2": 327,
    "pread64": 17,
}

NETLINK_SOCK_DIAG, SOCK_DIAG_BY_FAMILY = 4, 20
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_ERROR, NLMSG_DONE = 2, 3
UDIAG_SHOW_NAME, UNIX_DIAG_INO_OFFSET = 0x01, 4
BUFSZ = 65536


class iovec(ctypes.Structure):
    _fields_ = [("iov_base", ctypes.c_void_p), ("iov_len", ctypes.c_size_t)]


class msghdr(ctypes.Structure):
    _fields_ = [
        ("msg_name", ctypes.c_void_p),
        ("msg_namelen", ctypes.c_uint32),
        ("msg_iov", ctypes.POINTER(iovec)),
        ("msg_iovlen", ctypes.c_size_t),
        ("msg_control", ctypes.c_void_p),
        ("msg_controllen", ctypes.c_size_t),
        ("msg_flags", ctypes.c_int),
    ]


class mmsghdr(ctypes.Structure):
    _fields_ = [("msg_hdr", msghdr), ("msg_len", ctypes.c_uint32)]


def open_dump():
    """A bound NETLINK_SOCK_DIAG socket with a UNIX_DIAG dump already requested."""
    nl = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_SOCK_DIAG)
    nl.bind((0, 0))
    nl.send(
        struct.pack("=IHHII", 40, SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 1, 0)
        + struct.pack("=BBHIIIII", socket.AF_UNIX, 0, 0, 0xFFFFFFFF, 0, UDIAG_SHOW_NAME, 0, 0)
    )
    return nl


def sc(name, *args):
    ctypes.set_errno(0)
    rc = libc.syscall(ctypes.c_long(NR[name]), *args)
    return rc, ctypes.get_errno()


def via_read(fd, buf):
    return sc("read", ctypes.c_int(fd), buf, ctypes.c_size_t(BUFSZ))


def via_readv(fd, buf):
    iov = (iovec * 1)()
    iov[0].iov_base = ctypes.cast(buf, ctypes.c_void_p)
    iov[0].iov_len = BUFSZ
    return sc("readv", ctypes.c_int(fd), iov, ctypes.c_int(1))


def via_preadv(fd, buf):
    iov = (iovec * 1)()
    iov[0].iov_base = ctypes.cast(buf, ctypes.c_void_p)
    iov[0].iov_len = BUFSZ
    return sc("preadv", ctypes.c_int(fd), iov, ctypes.c_int(1), ctypes.c_long(0))


def via_preadv2(fd, buf):
    iov = (iovec * 1)()
    iov[0].iov_base = ctypes.cast(buf, ctypes.c_void_p)
    iov[0].iov_len = BUFSZ
    return sc("preadv2", ctypes.c_int(fd), iov, ctypes.c_int(1), ctypes.c_long(0),
              ctypes.c_long(0), ctypes.c_int(0))


def via_pread64(fd, buf):
    return sc("pread64", ctypes.c_int(fd), buf, ctypes.c_size_t(BUFSZ), ctypes.c_long(0))


def via_recvfrom(fd, buf):
    return sc("recvfrom", ctypes.c_int(fd), buf, ctypes.c_size_t(BUFSZ),
              ctypes.c_int(0), None, None)


def _msghdr(buf):
    iov = (iovec * 1)()
    iov[0].iov_base = ctypes.cast(buf, ctypes.c_void_p)
    iov[0].iov_len = BUFSZ
    m = msghdr()
    ctypes.memset(ctypes.byref(m), 0, ctypes.sizeof(m))
    m.msg_iov = iov
    m.msg_iovlen = 1
    return m, iov


def via_recvmsg(fd, buf):
    m, _iov = _msghdr(buf)
    return sc("recvmsg", ctypes.c_int(fd), ctypes.byref(m), ctypes.c_int(0))


def via_recvmmsg(fd, buf):
    m, _iov = _msghdr(buf)
    mm = (mmsghdr * 1)()
    ctypes.memset(ctypes.byref(mm), 0, ctypes.sizeof(mm))
    mm[0].msg_hdr = m
    rc, err = sc("recvmmsg", ctypes.c_int(fd), mm, ctypes.c_uint(1),
                 ctypes.c_int(0), None)
    return (mm[0].msg_len if rc > 0 else rc), err


METHODS = [
    ("read", via_read),
    ("readv", via_readv),
    ("preadv", via_preadv),
    ("preadv2", via_preadv2),
    ("pread64", via_pread64),
    ("recvfrom", via_recvfrom),
    ("recvmsg", via_recvmsg),
    ("recvmmsg", via_recvmmsg),
]


def inodes_in(blob, n):
    out, off = [], 0
    while off + 16 <= n:
        ln, ty = struct.unpack("<IH", blob[off:off + 6])
        if ln < 16 or off + ln > n:
            break
        if ty == SOCK_DIAG_BY_FAMILY:
            b = off + 16
            if b + UNIX_DIAG_INO_OFFSET + 4 <= off + ln:
                out.append(struct.unpack(
                    "<I", blob[b + UNIX_DIAG_INO_OFFSET:b + UNIX_DIAG_INO_OFFSET + 4])[0])
        off += (ln + 3) & ~3
    return out


held = []
for i in range(3):
    u = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    u.bind("\0hermit-recvmatrix-%d" % i)
    u.listen(1)
    held.append(u)

for name, fn in METHODS:
    nl = open_dump()
    buf = ctypes.create_string_buffer(BUFSZ)
    rc, err = fn(nl.fileno(), buf)
    if rc < 0:
        print("%-9s ERRNO=%s(%d)" % (name, os.strerror(err), err))
        nl.close()
        continue
    inos = inodes_in(bytes(buf.raw), rc)
    nz = [i for i in inos if i != 0]
    print("%-9s bytes=%-5d diag_msgs=%-2d nonzero_inodes=%-2d %s"
          % (name, rc, len(inos), len(nz),
             ("SANITIZED" if inos and not nz else
              ("BYPASS first=%d" % nz[0]) if nz else "no-diag-messages")))
    nl.close()
