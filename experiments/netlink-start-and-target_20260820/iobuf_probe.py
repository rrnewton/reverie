#!/usr/bin/env python3
"""Exercise recvmmsg and poll(...)=0 so a missing [iobuf] record means the check
did not observe them, not that nothing happened.

Both cases are issued through libc syscall() so the syscall NUMBER is under this
script's control rather than libc's wrapper choice.

argv[1] selects the case:
  recvmmsg  - receive 2 real datagrams into an mmsghdr batch, print their bytes
  poll0     - poll a never-ready fd with timeout 0, so it returns 0 while the
              kernel still rewrites revents on every pollfd entry
  recvmsg   - control: a path io-buffers is known to cover
"""
import ctypes
import os
import socket
import struct
import sys

libc = ctypes.CDLL("libc.so.6", use_errno=True)
NR = {"recvmmsg": 299, "recvmsg": 47, "poll": 7}
BUFSZ = 4096


class iovec(ctypes.Structure):
    _fields_ = [("iov_base", ctypes.c_void_p), ("iov_len", ctypes.c_size_t)]


class msghdr(ctypes.Structure):
    _fields_ = [
        ("msg_name", ctypes.c_void_p), ("msg_namelen", ctypes.c_uint32),
        ("msg_iov", ctypes.POINTER(iovec)), ("msg_iovlen", ctypes.c_size_t),
        ("msg_control", ctypes.c_void_p), ("msg_controllen", ctypes.c_size_t),
        ("msg_flags", ctypes.c_int),
    ]


class mmsghdr(ctypes.Structure):
    _fields_ = [("msg_hdr", msghdr), ("msg_len", ctypes.c_uint32)]


class pollfd(ctypes.Structure):
    _fields_ = [("fd", ctypes.c_int), ("events", ctypes.c_short),
                ("revents", ctypes.c_short)]


def sc(name, *args):
    ctypes.set_errno(0)
    return libc.syscall(ctypes.c_long(NR[name]), *args), ctypes.get_errno()


case = sys.argv[1]

if case == "recvmmsg":
    # A connected UDP pair on loopback, two datagrams already queued, so the
    # batch receive returns real bytes rather than blocking or returning empty.
    srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    srv.bind(("127.0.0.1", 0))
    cli = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    cli.sendto(b"alpha-payload", srv.getsockname())
    cli.sendto(b"beta-payload", srv.getsockname())

    count = 2
    bufs = [ctypes.create_string_buffer(BUFSZ) for _ in range(count)]
    iovs = (iovec * count)()
    batch = (mmsghdr * count)()
    ctypes.memset(ctypes.byref(batch), 0, ctypes.sizeof(batch))
    for i in range(count):
        iovs[i].iov_base = ctypes.cast(bufs[i], ctypes.c_void_p)
        iovs[i].iov_len = BUFSZ
        batch[i].msg_hdr.msg_iov = ctypes.pointer(iovs[i])
        batch[i].msg_hdr.msg_iovlen = 1

    rc, err = sc("recvmmsg", ctypes.c_int(srv.fileno()), batch,
                 ctypes.c_uint(count), ctypes.c_int(0), None)
    if rc < 0:
        raise SystemExit("recvmmsg failed errno %d" % err)
    got = [bytes(bufs[i].raw[:batch[i].msg_len]) for i in range(rc)]
    print("recvmmsg returned=%d payloads=%s" % (rc, b"|".join(got).decode()))

elif case == "poll0":
    # A pipe with nothing in it: POLLIN never fires, so poll returns 0. The
    # kernel still writes revents on EVERY entry, which is the output this case
    # is about. Poison revents first so a rewrite is observable.
    r1, w1 = os.pipe()
    r2, w2 = os.pipe()
    n = 2
    fds = (pollfd * n)()
    for i, fd in enumerate((r1, r2)):
        fds[i].fd = fd
        fds[i].events = 0x001  # POLLIN
        fds[i].revents = 0x7FFF  # poison
    before = [fds[i].revents for i in range(n)]
    rc, err = sc("poll", fds, ctypes.c_ulong(n), ctypes.c_int(0))
    after = [fds[i].revents for i in range(n)]
    print("poll returned=%d revents %s -> %s" % (rc, before, after))
    if rc != 0:
        raise SystemExit("expected poll to return 0, got %d" % rc)
    if before == after:
        raise SystemExit("kernel did not rewrite revents; case proves nothing")
    for fd in (r1, w1, r2, w2):
        os.close(fd)

elif case == "recvmsg":
    srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    srv.bind(("127.0.0.1", 0))
    cli = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    cli.sendto(b"control-payload", srv.getsockname())
    buf = ctypes.create_string_buffer(BUFSZ)
    iov = (iovec * 1)()
    iov[0].iov_base = ctypes.cast(buf, ctypes.c_void_p)
    iov[0].iov_len = BUFSZ
    m = msghdr()
    ctypes.memset(ctypes.byref(m), 0, ctypes.sizeof(m))
    m.msg_iov = iov
    m.msg_iovlen = 1
    rc, err = sc("recvmsg", ctypes.c_int(srv.fileno()), ctypes.byref(m), ctypes.c_int(0))
    if rc < 0:
        raise SystemExit("recvmsg failed errno %d" % err)
    print("recvmsg returned=%d payload=%s" % (rc, bytes(buf.raw[:rc]).decode()))

else:
    raise SystemExit("unknown case %s" % case)
