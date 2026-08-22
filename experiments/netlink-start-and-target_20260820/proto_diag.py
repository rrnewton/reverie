#!/usr/bin/env python3
"""Prototype: drive a POPULATED NETLINK_SOCK_DIAG AF_UNIX dump and print the
socket inode numbers the kernel returned.

detcore zeroes these in sanitize_sock_diag_inodes (detcore/src/sock_diag.rs).
When that runs, every inode is 0 and the output is deterministic. Without it the
inodes are a host-global kernel counter and differ on every run.

The sensitivity control is hermit's own --no-virtualize-metadata, which is the
real production gate at detcore/src/syscalls/io.rs:1322 -- not a test-only knob.
"""
import os
import socket
import struct
import sys

NETLINK_SOCK_DIAG = 4
SOCK_DIAG_BY_FAMILY = 20
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_DONE, NLMSG_ERROR = 3, 2
UDIAG_SHOW_NAME = 0x01
UNIX_DIAG_INO_OFFSET = 4  # udiag_ino within struct unix_diag_msg

# Bind AF_UNIX sockets so the dump is non-empty. Abstract namespace: no
# filesystem artifact, so nothing to clean up and no path determinism concern.
held = []
for i in range(3):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind("\0hermit-compat-sockdiag-%d" % i)
    s.listen(1)
    held.append(s)

nl = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_SOCK_DIAG)
nl.bind((0, 0))
# nlmsghdr(16) + unix_diag_req(24)
req = struct.pack("=IHHII", 40, SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 1, 0)
req += struct.pack(
    "=BBHIIIII",
    socket.AF_UNIX,  # sdiag_family
    0,               # sdiag_protocol
    0,               # pad
    0xFFFFFFFF,      # udiag_states: all
    0,               # udiag_ino
    UDIAG_SHOW_NAME, # udiag_show
    0, 0,            # udiag_cookie
)
nl.send(req)

blob = b""
while True:
    chunk = nl.recv(65536)
    blob += chunk
    done = False
    off = 0
    while off + 16 <= len(chunk):
        ln, ty = struct.unpack("<IH", chunk[off : off + 6])
        if ln < 16:
            break
        if ty in (NLMSG_DONE, NLMSG_ERROR):
            done = True
        off += (ln + 3) & ~3
    if done or not chunk:
        break
nl.close()

inodes, ndiag = [], 0
off = 0
while off + 16 <= len(blob):
    nlmsg_len, nlmsg_type = struct.unpack("<IH", blob[off : off + 6])
    if nlmsg_len < 16 or off + nlmsg_len > len(blob):
        break
    if nlmsg_type == SOCK_DIAG_BY_FAMILY:
        ndiag += 1
        body = off + 16
        if body + UNIX_DIAG_INO_OFFSET + 4 <= off + nlmsg_len:
            (ino,) = struct.unpack(
                "<I", blob[body + UNIX_DIAG_INO_OFFSET : body + UNIX_DIAG_INO_OFFSET + 4]
            )
            inodes.append(ino)
    off += (nlmsg_len + 3) & ~3

# Fail closed: a dump that returned no diag messages would make "all inodes are
# zero" vacuously true. That is the empty-corpus defect, so refuse it.
if ndiag == 0:
    raise SystemExit("no SOCK_DIAG_BY_FAMILY messages returned; nothing was tested")

print("unix_diag_messages_present=yes")
print("nonzero_inodes=%d" % sum(1 for i in inodes if i != 0))
