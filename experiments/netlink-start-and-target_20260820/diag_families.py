#!/usr/bin/env python3
"""Enumerate which address families this kernel's SOCK_DIAG subsystem answers
for, by asking it rather than by reading a table.

sock_diag_rcv_msg dispatches on the request's first byte. A family with no
registered handler comes back ENOENT (after a module-load attempt); a family
with a handler comes back either with SOCK_DIAG_BY_FAMILY messages or EINVAL
(handler present, request shape wrong for it). So ENOENT vs anything-else
separates "cannot meet this family" from "can".
"""
import socket
import struct
import sys

NETLINK_SOCK_DIAG = 4
SOCK_DIAG_BY_FAMILY = 20
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_ERROR, NLMSG_DONE = 2, 3

NAMES = {
    0: "AF_UNSPEC", 1: "AF_UNIX", 2: "AF_INET", 3: "AF_AX25", 4: "AF_IPX",
    5: "AF_APPLETALK", 10: "AF_INET6", 16: "AF_NETLINK", 17: "AF_PACKET",
    30: "AF_TIPC", 31: "AF_BLUETOOTH", 38: "AF_NFC", 40: "AF_VSOCK",
    43: "AF_SMC", 44: "AF_XDP", 45: "AF_MCTP",
}


def probe(family):
    nl = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_SOCK_DIAG)
    nl.settimeout(2.0)
    nl.bind((0, 0))
    # 16-byte nlmsghdr + a 40-byte body large enough for any diag request; the
    # first byte is what the dispatcher reads.
    body = struct.pack("=BBHIIIII", family, 0, 0, 0xFFFFFFFF, 0, 0xFF, 0, 0)
    body += b"\0" * (40 - len(body))
    nl.send(struct.pack("=IHHII", 16 + len(body), SOCK_DIAG_BY_FAMILY,
                        NLM_F_REQUEST | NLM_F_DUMP, 1, 0) + body)
    try:
        blob = nl.recv(65536)
    except socket.timeout:
        nl.close()
        return "TIMEOUT", 0, 0
    nl.close()
    if len(blob) < 16:
        return "SHORT", 0, 0
    nlmsg_len, nlmsg_type = struct.unpack("<IH", blob[0:6])
    if nlmsg_type == NLMSG_ERROR:
        (err,) = struct.unpack("<i", blob[16:20])
        return ("OK-EMPTY" if err == 0 else "ERR"), -err, 0
    if nlmsg_type == SOCK_DIAG_BY_FAMILY:
        # Count diag messages in this first chunk.
        n, off = 0, 0
        while off + 16 <= len(blob):
            ln, ty = struct.unpack("<IH", blob[off:off + 6])
            if ln < 16 or off + ln > len(blob):
                break
            if ty == SOCK_DIAG_BY_FAMILY:
                n += 1
            off += (ln + 3) & ~3
        return "MESSAGES", 0, n
    if nlmsg_type == NLMSG_DONE:
        return "OK-EMPTY", 0, 0
    return "TYPE%d" % nlmsg_type, 0, 0


import errno as E

print("%-4s %-14s %-10s %s" % ("fam", "name", "verdict", "detail"))
for family in range(0, 46):
    verdict, err, n = probe(family)
    if verdict == "ERR" and err == E.ENOENT:
        continue  # no handler registered: this family cannot be met
    name = NAMES.get(family, "AF_%d" % family)
    if verdict == "ERR":
        detail = "handler PRESENT, request rejected: %s" % E.errorcode.get(err, err)
    elif verdict == "MESSAGES":
        detail = "handler PRESENT, %d diag message(s) in first chunk" % n
    else:
        detail = "handler PRESENT, %s" % verdict
    print("%-4d %-14s %-10s %s" % (family, name, verdict, detail))
