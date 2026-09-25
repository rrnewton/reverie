#!/usr/bin/env python3
"""List the libc names reverie-syscalls and reverie name, one per line.

Two sources:
  * every `libc::NAME` path in the given source trees;
  * every variant of a `const_enum!` invocation, which the macro expands to
    `libc::VARIANT` (reverie-syscalls/src/macros.rs).
Names matching a line of SKIP_FILE (extended regexes, `#` comments) are
dropped.

Usage: find_roots.py SKIP_FILE DIR... > roots.txt
"""

import os
import re
import sys

skip_path, *dirs = sys.argv[1:]
skips = [
    re.compile(line.strip())
    for line in open(skip_path)
    if line.strip() and not line.startswith("#")
]
GENERATED = {"libc_shim.rs", "nix_shim.rs"}

path_re = re.compile(r"\blibc::([A-Za-z_][A-Za-z0-9_]*)")
const_enum_re = re.compile(r"^const_enum! \{\n(.*?)^\}", re.M | re.S)
variant_re = re.compile(r"^\s*([A-Z][A-Z0-9_]*),\s*$", re.M)

names = set()
for d in dirs:
    for dirpath, _, files in os.walk(d):
        for f in files:
            if not f.endswith(".rs") or f in GENERATED:
                continue
            text = open(os.path.join(dirpath, f)).read()
            names.update(path_re.findall(text))
            for block in const_enum_re.findall(text):
                names.update(variant_re.findall(block))

for name in sorted(names):
    if not any(s.fullmatch(name) for s in skips):
        print(name)
