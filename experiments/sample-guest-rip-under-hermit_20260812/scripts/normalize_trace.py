#!/usr/bin/env python3
"""Turn a QEMU `-d exec` guest-PC trace into base-relative symbolic tokens.

Absolute JITed addresses are only stable within one fixed configuration, so any
comparison across runs must be done in base-relative form.  This reduces each
traced guest PC to one of, in order of preference:

  <prog>#<subprog>+0x<off>   -- inside a subprogram named by the in-guest
                                BPF_OBJ_GET_INFO_BY_FD dump (jited_ksyms +
                                jited_func_lens), the form that also carries a
                                source location
  <symbol>+0x<off>           -- inside a guest /proc/kallsyms symbol
  ?0x<pc>                    -- neither

Inputs are all produced by one boot: the QEMU trace file, and the guest console
from the same boot (which carries the MAP and KALLSYMS blocks).

Usage:
  normalize_trace.py <trace.txt> <console.txt> [--source] [--distinct]
  normalize_trace.py --diff <traceA> <consoleA> <traceB> <consoleB>
"""
import bisect
import re
import sys

TRACE_RE = re.compile(r"^Trace \d+: \S+ \[[0-9a-f]+/([0-9a-f]+)/")


def read_block(console, begin, end):
    out, on = [], False
    for line in open(console, errors="replace"):
        line = line.rstrip("\r\n")
        if line.endswith(begin):
            on = True
            continue
        if line.endswith(end):
            on = False
            continue
        if on:
            out.append(line)
    return out


def load_map(console):
    """[(base, len, progname, subidx, [(addr, insn, src), ...])] sorted by base."""
    subs, rows, cur = [], {}, None
    for line in read_block(console, "--- MAP BEGIN ---", "--- MAP END ---"):
        m = re.match(r"=== prog id=(\d+) name=(\S+)", line)
        if m:
            cur = m.group(2)
            continue
        m = re.match(r"\s+subprog\[(\d+)\] ksym=0x([0-9a-f]+) len=(\d+)", line)
        if m and cur:
            subs.append([int(m.group(2), 16), int(m.group(3)), cur, int(m.group(1)), []])
            continue
        m = re.match(r"\s+0x([0-9a-f]+) (\d+)\s+(\S+)", line)
        if m and cur:
            rows.setdefault(cur, []).append((int(m.group(1), 16), int(m.group(2)), m.group(3)))
    subs.sort()
    for s in subs:
        s[4] = sorted(r for r in rows.get(s[2], []) if s[0] <= r[0] < s[0] + s[1])
    return subs


def load_kallsyms(console):
    syms = []
    for line in read_block(console, "--- KALLSYMS BEGIN ---", "--- KALLSYMS END ---"):
        m = re.match(r"([0-9a-f]{16}) (\S) (.*)$", line.strip())
        if m:
            syms.append((int(m.group(1), 16), m.group(3).split("\t")[0]))
    syms.sort()
    return syms


class Resolver:
    def __init__(self, console):
        self.subs = load_map(console)
        self.sub_bases = [s[0] for s in self.subs]
        self.syms = load_kallsyms(console)
        self.sym_addrs = [s[0] for s in self.syms]

    def token(self, pc, with_source=False):
        i = bisect.bisect_right(self.sub_bases, pc) - 1
        if i >= 0:
            base, length, name, idx, rows = self.subs[i]
            if base <= pc < base + length:
                tok = f"{name}#{idx}+0x{pc - base:x}"
                if with_source and rows:
                    j = bisect.bisect_right([r[0] for r in rows], pc) - 1
                    if j >= 0:
                        tok += f"  {rows[j][2]}"
                return tok
        i = bisect.bisect_right(self.sym_addrs, pc) - 1
        if i >= 0:
            return f"{self.syms[i][1]}+0x{pc - self.syms[i][0]:x}"
        return f"?0x{pc:x}"


def pcs(trace):
    for line in open(trace, errors="replace"):
        m = TRACE_RE.match(line)
        if m:
            yield int(m.group(1), 16)


def normalize(trace, console, with_source=False):
    r = Resolver(console)
    return [r.token(pc, with_source) for pc in pcs(trace)], r


def main():
    if sys.argv[1] == "--diff":
        ta, ca, tb, cb = sys.argv[2:6]
        a, ra = normalize(ta, ca)
        b, rb = normalize(tb, cb)
        print(f"A: {len(a)} TB entries, {len(set(a))} distinct sites")
        print(f"B: {len(b)} TB entries, {len(set(b))} distinct sites")
        if a == b:
            print("NORMALIZED TRACES IDENTICAL")
            return
        n = min(len(a), len(b))
        for i in range(n):
            if a[i] != b[i]:
                print(f"FIRST DIVERGENCE at TB entry #{i}")
                lo = max(0, i - 3)
                for j in range(lo, i):
                    print(f"  common #{j}: {a[j]}")
                print(f"  A      #{i}: {ra.token(0, False) and a[i]}")
                print(f"  B      #{i}: {b[i]}")
                print()
                print("  A source:", normalize_one(ta, ca, i))
                print("  B source:", normalize_one(tb, cb, i))
                return
        print(f"COMMON PREFIX of {n}; lengths differ ({len(a)} vs {len(b)})")
        return

    trace, console = sys.argv[1], sys.argv[2]
    with_source = "--source" in sys.argv
    toks, _ = normalize(trace, console, with_source)
    if "--distinct" in sys.argv:
        for t in sorted(set(toks)):
            print(t)
    else:
        for t in toks:
            print(t)


def normalize_one(trace, console, index):
    r = Resolver(console)
    for i, pc in enumerate(pcs(trace)):
        if i == index:
            return r.token(pc, True)
    return "<out of range>"


if __name__ == "__main__":
    main()
