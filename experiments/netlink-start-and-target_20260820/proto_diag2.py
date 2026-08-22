#!/usr/bin/env python3
"""Same UNIX_DIAG dump, received two ways, printing the actual inode values.

argv[1] == "recvmsg" -> socket.recvmsg(), which reaches detcore's
                        handle_sock_diag_recvmsg (io.rs:1325).
argv[1] == "recvfrom" -> socket.recv(), which lowers to the recvfrom syscall and
                         does NOT consult the sock_diag flag.
"""
import socket
import struct
import sys

NETLINK_SOCK_DIAG = 4
SOCK_DIAG_BY_FAMILY = 20
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_DONE, NLMSG_ERROR = 3, 2
UDIAG_SHOW_NAME = 0x01
UNIX_DIAG_INO_OFFSET = 4

how = sys.argv[1]

held = []
for i in range(3):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind("\0hermit-compat-sockdiag-%d" % i)
    s.listen(1)
    held.append(s)

nl = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_SOCK_DIAG)
nl.bind((0, 0))
req = struct.pack("=IHHII", 40, SOCK_DIAG_BY_FAMILY, NLM_F_REQUEST | NLM_F_DUMP, 1, 0)
req += struct.pack("=BBHIIIII", socket.AF_UNIX, 0, 0, 0xFFFFFFFF, 0, UDIAG_SHOW_NAME, 0, 0)
nl.send(req)


def get():
    if how == "recvmsg":
        data, _anc, _flags, _addr = nl.recvmsg(65536)
        return data
    return nl.recv(65536)


blob = b""
while True:
    chunk = get()
    blob += chunk
    done, off = False, 0
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
        b = off + 16
        (ino,) = struct.unpack("<I", blob[b + UNIX_DIAG_INO_OFFSET : b + UNIX_DIAG_INO_OFFSET + 4])
        inodes.append(ino)
    off += (nlmsg_len + 3) & ~3

if ndiag == 0:
    raise SystemExit("no diag messages; nothing tested")
print("how=%s diag_messages=%d inodes=%s" % (how, ndiag, ",".join(str(i) for i in inodes)))
