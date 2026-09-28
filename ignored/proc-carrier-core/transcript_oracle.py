#!/usr/bin/env python3
"""Independent stdlib oracle for the proc-carrier v1 wire transcript."""

import hashlib
import hmac
import struct

DOMAIN = b"reverie-kvm.proc-carrier"
VERSION = 1
N = 9223372036854775807000000000
R = 3865545067477747802768211456


def tlv(field, value):
    return struct.pack(">H", field) + struct.pack(">Q", len(value)) + value


authority = bytes([0x11]) * 16
path = b"/proc/uptime"
name = (
    b"reverie-kvm.proc-carrier.v1."
    + authority.hex().encode()
    + b".proc.nf1."
    + path.hex().encode()
)

fields = [
    (1, authority),
    (2, name),
    (3, b"proc"),
    (4, path),
    (5, struct.pack(">I", 0x20000)),
    (6, b"\x02"),
    (7, struct.pack(">i", 0)),
    (8, struct.pack(">Q", 0)),
    (9, struct.pack(">Q", 23)),
    (10, struct.pack(">Q", 23)),
    (11, struct.pack(">I", 5)),
    (12, struct.pack(">i", 7)),
    (13, bytes([1, 2, 3, 4, 5])),
    (14, struct.pack(">q", 0x01021994)),
    (15, struct.pack(">I", 0)),
    (16, struct.pack(">I", 42)),
    (17, struct.pack(">Q", 99)),
    (18, struct.pack(">H", 0o100444)),
    (19, struct.pack(">I", 0)),
    (20, struct.pack(">I", 1000)),
    (21, struct.pack(">I", 1001)),
    (22, struct.pack(">I", 0)),
    (23, struct.pack(">I", 0)),
    (24, struct.pack(">I", 4096)),
    (25, struct.pack(">Q", 0x20)),
    (26, struct.pack(">i", 0x0F)),
    (27, struct.pack(">Q", 3)),
    (28, b"abc"),
]

transcript = DOMAIN + struct.pack(">I", VERSION) + b"".join(
    tlv(field, value) for field, value in fields
)
print("name_len", len(name))
print("transcript_len", len(transcript))
print("transcript_hex", transcript.hex())
print("transcript_sha256", hashlib.sha256(transcript).hexdigest())

key = bytes([0x5A]) * 32
counter = 0
while True:
    candidate_input = transcript + tlv(0xFFFF, struct.pack(">Q", counter))
    digest = hmac.new(key, candidate_input, hashlib.sha256).digest()
    candidate = int.from_bytes(digest[:16], "big")
    print("candidate", counter, candidate, digest.hex())
    if candidate >= R:
        print("tag", candidate % N)
        break
    counter += 1

assert (1 << 128) % N == R
assert divmod(N - 1, 1_000_000_000) == (2**63 - 2, 999_999_999)
