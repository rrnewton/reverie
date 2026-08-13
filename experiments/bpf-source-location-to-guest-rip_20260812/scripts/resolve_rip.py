#!/usr/bin/env python3
"""Resolve a kernel RIP inside JITed BPF code to a scheduler source location.

Input is the text emitted by scripts/bpf_riplines.c (one or more programs).
The lookup is the same one bpftool and perf perform:

  1. find the subprogram whose [ksym, ksym + jited_func_len) contains the RIP;
  2. inside that subprogram, take the last jited_line_info entry whose address
     is <= RIP  (nearest-preceding: the entries are the *starts* of the JITed
     ranges belonging to each source location, not exact instruction addresses);
  3. report that entry's file:line:column.

Usage:
  resolve_rip.py <map.txt> <addr> [<addr> ...]
  resolve_rip.py <map.txt> --coverage      # granularity of every subprogram
"""
import bisect
import re
import sys


def parse(path):
    progs = []
    cur = None
    for line in open(path):
        m = re.match(r"=== prog id=(\d+) name=(\S+) type=(\d+) tag=(\w+)", line)
        if m:
            cur = {"id": int(m.group(1)), "name": m.group(2), "tag": m.group(4),
                   "subs": [], "rows": []}
            progs.append(cur)
            continue
        if cur is None:
            continue
        m = re.match(r"\s+subprog\[(\d+)\] ksym=0x([0-9a-f]+) len=(\d+)", line)
        if m:
            cur["subs"].append((int(m.group(2), 16), int(m.group(3))))
            continue
        m = re.match(r"\s+0x([0-9a-f]+) (\d+)\s+(\S+)", line)
        if m:
            cur["rows"].append((int(m.group(1), 16), int(m.group(2)), m.group(3)))
    return progs


def index(progs):
    """subprog -> sorted list of (addr, bpf_insn_idx, source)."""
    out = []
    for p in progs:
        for k, (base, length) in enumerate(p["subs"]):
            rows = sorted(r for r in p["rows"] if base <= r[0] < base + length)
            out.append({"prog": p, "sub": k, "base": base, "len": length, "rows": rows})
    return out


def resolve(subs, addr):
    for s in subs:
        if not (s["base"] <= addr < s["base"] + s["len"]):
            continue
        addrs = [r[0] for r in s["rows"]]
        i = bisect.bisect_right(addrs, addr) - 1
        if i < 0:
            return s, None
        return s, s["rows"][i]
    return None, None


def main():
    progs = parse(sys.argv[1])
    subs = index(progs)
    if len(sys.argv) > 2 and sys.argv[2] == "--coverage":
        print(f"{'prog':20} {'sub':>3} {'base':>18} {'len':>6} {'records':>8} "
              f"{'bytes/record min/med/max':>26}")
        for s in subs:
            g = []
            addrs = [r[0] for r in s["rows"]]
            ends = addrs[1:] + [s["base"] + s["len"]]
            g = sorted(y - x for x, y in zip(addrs, ends))
            med = g[len(g) // 2] if g else 0
            print(f"{s['prog']['name']:20} {s['sub']:3d} 0x{s['base']:016x} "
                  f"{s['len']:6d} {len(s['rows']):8d} "
                  f"{(g[0] if g else 0):8d}/{med}/{(g[-1] if g else 0)}")
        return
    for a in sys.argv[2:]:
        addr = int(a, 0)
        s, r = resolve(subs, addr)
        if s is None:
            print(f"0x{addr:016x}  NOT INSIDE ANY JITED BPF SUBPROGRAM")
        elif r is None:
            print(f"0x{addr:016x}  {s['prog']['name']} subprog {s['sub']} "
                  f"+0x{addr - s['base']:x}  (no line_info record at or before)")
        else:
            print(f"0x{addr:016x}  {s['prog']['name']} subprog {s['sub']} "
                  f"+0x{addr - s['base']:x}  bpf_insn {r[1]}  {r[2]}"
                  f"   (record starts at +0x{r[0] - s['base']:x}, "
                  f"RIP is {addr - r[0]} byte(s) into it)")


if __name__ == "__main__":
    main()
