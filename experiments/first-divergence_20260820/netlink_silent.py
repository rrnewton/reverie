#!/usr/bin/env python3
"""Reproduce the exact netlink exchange the demo-6 controller performs, and
print the reply so two runs can be compared byte for byte.

Mirrors what the INFO log shows dtid 3 doing at syscalls 901-906:
    socket(16, 524291, 0)                       AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC
    bind(11, ..., 12)                           sockaddr_nl is 12 bytes
    sendto(11, ..., 20, 0, ..., 12)             16-byte nlmsghdr + 4-byte rtgenmsg
    recvmsg(11, ..., 0) = Ok(1468)              the RTM_NEWLINK dump

That is glibc's getifaddrs() enumerating interfaces. This script issues the same
RTM_GETLINK dump directly so the payload is visible instead of being hidden
behind a syscall return value that is identical in every run.
"""
import socket
import struct
import sys
import hashlib

NETLINK_ROUTE = 0
RTM_GETLINK = 18
NLM_F_REQUEST = 0x001
NLM_F_DUMP = 0x300

s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_ROUTE)
s.bind((0, 0))
# nlmsghdr{len,type,flags,seq,pid} + rtgenmsg{family} padded to 4
msg = struct.pack("=IHHII", 20, RTM_GETLINK, NLM_F_REQUEST | NLM_F_DUMP, 1, 0)
msg += struct.pack("=Bxxx", socket.AF_PACKET)
s.send(msg)

chunks = []
while True:
    data = s.recv(65536)
    chunks.append(data)
    # nlmsghdr type NLMSG_DONE == 3 terminates a dump
    if len(data) >= 6 and struct.unpack("=H", data[4:6])[0] == 3:
        break
    if len(data) < 20:
        break
s.close()

blob = b"".join(chunks)
print("total_bytes", len(blob))
pass  # deliberately NOT printed: the real case keeps these bytes internal
if len(sys.argv) > 1 and sys.argv[1] == "--hex":
    sys.stdout.write(blob.hex() + "\n")
