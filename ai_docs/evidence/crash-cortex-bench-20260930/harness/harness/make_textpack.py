#!/usr/bin/env python3
"""Pack a list of files into one plain-text archive suitable for a Phabricator paste.
Unpack: split on lines '===== FILE: <path> sha256=<h> bytes=<n> =====' ... '===== END FILE ====='."""
import hashlib, os, sys
out, files = sys.argv[1], sys.argv[2:]
with open(out, "w") as o:
    o.write("# ccbench textpack v1 — unpack by splitting on '===== FILE:' headers\n")
    for f in files:
        b = open(f, "rb").read()
        try:
            t = b.decode()
        except UnicodeDecodeError:
            continue
        o.write(f"\n===== FILE: {f} sha256={hashlib.sha256(b).hexdigest()} bytes={len(b)} =====\n{t}\n===== END FILE =====\n")
print(out, os.path.getsize(out))
