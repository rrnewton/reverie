#!/usr/bin/env python3
"""Retrieve AF_INET socket diag records and print the host-assigned inode, both
from the BINARY sock_diag path and from the /proc/net/tcp TEXT path, so the two
can be compared.

Two runs of this under hermit with differing inodes is the leak; the two paths
disagreeing after a partial fix is the other failure mode worth watching for.
"""
import os
import socket
import struct
import sys

NETLINK_SOCK_DIAG = 4
SOCK_DIAG_BY_FAMILY = 20
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_ERROR, NLMSG_DONE = 2, 3
IPPROTO_TCP = 6
# struct inet_diag_msg: 4 header bytes + 48-byte inet_diag_sockid + expires,
# rqueue, wqueue, uid, then inode.
INET_DIAG_INO_OFFSET = 4 + 48 + 4 + 4 + 4 + 4  # = 68
TCP_ALL_STATES = 0xFFFFFFFF

family = socket.AF_INET6 if (len(sys.argv) > 1 and sys.argv[1] == "6") else socket.AF_INET
label = "inet6" if family == socket.AF_INET6 else "inet"

# Bind real listeners so the dump is populated.
held = []
for _ in range(3):
    s = socket.socket(family, socket.SOCK_STREAM)
    s.bind(("::1", 0) if family == socket.AF_INET6 else ("127.0.0.1", 0))
    s.listen(1)
    held.append(s)

nl = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_SOCK_DIAG)
nl.bind((0, 0))
# struct inet_diag_req_v2 = 4 + 4 + 48
req = struct.pack("=BBBBI", family, IPPROTO_TCP, 0, 0, TCP_ALL_STATES)
req += b"\0" * 48
nl.send(struct.pack("=IHHII", 16 + len(req), SOCK_DIAG_BY_FAMILY,
                    NLM_F_REQUEST | NLM_F_DUMP, 1, 0) + req)

blob = b""
while True:
    chunk = nl.recv(65536)
    if not chunk:
        break
    blob += chunk
    done, off = False, 0
    while off + 16 <= len(chunk):
        ln, ty = struct.unpack("<IH", chunk[off:off + 6])
        if ln < 16:
            break
        if ty in (NLMSG_DONE, NLMSG_ERROR):
            done = True
        off += (ln + 3) & ~3
    if done:
        break
nl.close()

inodes, err, off = [], None, 0
while off + 16 <= len(blob):
    nlmsg_len, nlmsg_type = struct.unpack("<IH", blob[off:off + 6])
    if nlmsg_len < 16 or off + nlmsg_len > len(blob):
        break
    if nlmsg_type == NLMSG_ERROR:
        (e,) = struct.unpack("<i", blob[off + 16:off + 20])
        if e:
            err = -e
    if nlmsg_type == SOCK_DIAG_BY_FAMILY:
        b = off + 16
        if b + INET_DIAG_INO_OFFSET + 4 <= off + nlmsg_len:
            inodes.append(struct.unpack(
                "<I", blob[b + INET_DIAG_INO_OFFSET:b + INET_DIAG_INO_OFFSET + 4])[0])
    off += (nlmsg_len + 3) & ~3

if err is not None:
    raise SystemExit("%s: sock_diag returned errno %d" % (label, err))
if not inodes:
    raise SystemExit("%s: no diag messages; nothing was tested" % label)

# The TEXT view of the same table, whose last-but-few column is the same inode.
text_path = "/proc/net/tcp6" if family == socket.AF_INET6 else "/proc/net/tcp"
text_inodes = []
try:
    with open(text_path) as fh:
        next(fh)
        for line in fh:
            fields = line.split()
            if len(fields) > 9:
                text_inodes.append(int(fields[9]))
except FileNotFoundError:
    text_inodes = None

print("%s binary sock_diag: n=%d nonzero=%d inodes=%s"
      % (label, len(inodes), sum(1 for i in inodes if i), ",".join(map(str, sorted(inodes)[:6]))))
if text_inodes is None:
    print("%s text %s: absent" % (label, text_path))
else:
    print("%s text %s: n=%d nonzero=%d inodes=%s"
          % (label, text_path, len(text_inodes), sum(1 for i in text_inodes if i),
             ",".join(map(str, sorted(text_inodes)[:6]))))
