#!/usr/bin/env python3
"""Prototype: canonical, deterministic summary of an RTM_GETLINK dump.

Masks exactly the two fields measured to leak host state
(IFLA_INET6_CACHEINFO.tstamp and .reachable_time) and nothing else, so any
NEW nondeterminism anywhere in the reply changes the digest.

HERMIT_NETLINK_UNMASK=1 disables the mask: that is the sensitivity control,
and it must make the digest vary run to run.
"""
import hashlib
import os
import socket
import struct
import sys

NETLINK_ROUTE = 0
RTM_GETLINK = 18
NLM_F_REQUEST, NLM_F_DUMP = 0x001, 0x300
NLMSG_DONE = 3
IFLA_AF_SPEC = 26
IFLA_INET6_CACHEINFO = 5

UNMASK = os.environ.get("HERMIT_NETLINK_UNMASK") == "1"


def dump():
    s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, NETLINK_ROUTE)
    s.bind((0, 0))
    s.send(
        struct.pack("=IHHII", 20, RTM_GETLINK, NLM_F_REQUEST | NLM_F_DUMP, 1, 0)
        + struct.pack("=Bxxx", socket.AF_PACKET)
    )
    blob = b""
    while True:
        chunk = s.recv(65536)
        blob += chunk
        if len(chunk) >= 6 and struct.unpack("=H", chunk[4:6])[0] == NLMSG_DONE:
            break
        if len(chunk) < 20:
            break
    s.close()
    return blob


def attrs(buf, start, end):
    """Yield (type, payload_start, payload_end) for rtattrs in [start,end)."""
    off = start
    while off + 4 <= end:
        rta_len, rta_type = struct.unpack("<HH", buf[off : off + 4])
        if rta_len < 4 or off + rta_len > end:
            return
        yield rta_type, off + 4, off + rta_len
        off += (rta_len + 3) & ~3


def mask_cacheinfo(buf):
    """Zero tstamp and reachable_time inside every IFLA_INET6_CACHEINFO.

    Returns (masked_bytes, list_of_masked_byte_offsets). Located structurally by
    walking the attribute tree, not by a hardcoded offset.
    """
    out = bytearray(buf)
    masked = []
    off = 0
    while off + 16 <= len(buf):
        nlmsg_len, nlmsg_type = struct.unpack("<IH", buf[off : off + 6])
        if nlmsg_len < 16 or off + nlmsg_len > len(buf):
            break
        if nlmsg_type == 16:  # RTM_NEWLINK
            body = off + 16 + 16  # nlmsghdr + ifinfomsg
            end = off + nlmsg_len
            for t, ps, pe in attrs(buf, body, end):
                if t != IFLA_AF_SPEC:
                    continue
                for fam, fs, fe in attrs(buf, ps, pe):
                    if fam != socket.AF_INET6:
                        continue
                    for a, s2, e2 in attrs(buf, fs, fe):
                        if a != IFLA_INET6_CACHEINFO or e2 - s2 < 16:
                            continue
                        # struct ifla_cacheinfo: max_reasm_len, tstamp,
                        # reachable_time, retrans_time. Mask fields 1 and 2.
                        for b in range(s2 + 4, s2 + 12):
                            out[b] = 0
                            masked.append(b)
        off += (nlmsg_len + 3) & ~3
    return bytes(out), masked


def summarize(buf):
    lines = []
    nmsg = 0
    off = 0
    while off + 16 <= len(buf):
        nlmsg_len, nlmsg_type = struct.unpack("<IH", buf[off : off + 6])
        if nlmsg_len < 16 or off + nlmsg_len > len(buf):
            break
        nmsg += 1
        if nlmsg_type == 16:
            ifi_family, _, ifi_type, ifi_index, ifi_flags, _ = struct.unpack(
                "<BBHiII", buf[off + 16 : off + 32]
            )
            name, inv = "?", []
            for t, ps, pe in attrs(buf, off + 32, off + nlmsg_len):
                inv.append("%d:%d" % (t, pe - ps))
                if t == 3:  # IFLA_IFNAME
                    name = buf[ps:pe].rstrip(b"\0").decode()
            lines.append(
                "link name=%s index=%d type=%d flags=0x%08x attrs=%s"
                % (name, ifi_index, ifi_type, ifi_flags, ",".join(inv))
            )
        off += (nlmsg_len + 3) & ~3
    lines.append("messages=%d bytes=%d" % (nmsg, len(buf)))
    return lines


blob = dump()
masked, offsets = mask_cacheinfo(blob)
body = blob if UNMASK else masked
for line in summarize(blob):
    print(line)
print("masked_fields=%d" % (len(offsets) // 8))
print("digest=%s" % hashlib.sha256(body).hexdigest())
