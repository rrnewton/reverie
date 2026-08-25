#!/usr/bin/env python3
"""Pair hermit `--verify` run logs CORRECTLY, and classify how each pair diverges.

WHY THIS EXISTS
---------------
Pairing run1/run2 logs by MODIFICATION TIME is wrong and fails silently. Measured on
886 log pairs: it skipped 364 (41%) and cross-paired logs from DIFFERENT GUESTS -- it
matched a bash log to a /bin/true log. For a one-off that is an annoyance. For
characterising intermittent failures across many cells it is fatal, because it
produces a plausible pattern out of whichever pairs happened to line up, and the
pattern looks like a finding.

THE KEY
-------
  cell identity = the guest's first execve line (program + argv exactly as logged)
  invocation    = nearest mtime WITHIN that cell group only
  quality gate  = the pair must share >= --min-prefix identical leading DETLOG lines
Pairs failing the gate are reported UNPAIRED WITH A REASON. Nothing is dropped silently.

CLASSIFICATION (three classes -- counts alone will not distinguish them)
-----------------------------------------------------------------------
  CONTROL-FLOW : DETLOG counts differ, OR the syscall name / return value differs
  CLOCK        : every difference lies inside a tv_sec/tv_nsec/tv_usec payload
  OBSERVATION  : same syscall, same return, same buffer length -- differing bytes
`[iobuf][dtid N] op dir 0xADDR+LEN-><sha>` records are buffer CONTENT HASHES. They must
be shape-matched on (dtid, op, dir, addr, len); if you only match `finish syscall #N:`
records they fall through and every observation divergence is misfiled as control-flow.
That bug cost me 128 pairs and I published wrong figures before catching it.

⚠️ AGGREGATING BY CELL ACROSS RUNS: the execve argv embeds the WORKTREE PATH, so the same
logical cell run from two worktrees yields two different keys. That is harmless for
PAIRING (both runs of a pair share a worktree) but it FRAGMENTS any per-cell tally.
Use --strip-prefix to normalise the worktree root out when aggregating by cell.
"""
import argparse, glob, hashlib, os, re
from collections import Counter, defaultdict

EXECVE  = re.compile(r'execve\([^)]*\)')
TIMEPAY = re.compile(r'-> \{ tv_sec: \d+, tv_(?:nsec|usec): \d+ \}')
IOBUF   = re.compile(r'\[iobuf\]\[dtid (\d+)\] (\S+) (\S+) (0x[0-9a-f]+)\+(\d+)->')
CALL    = re.compile(r'finish syscall #(\d+): ([a-z_0-9]+)\(')
RET     = re.compile(r'\)\s*=\s*(.+)$')
WORKTREE = re.compile(r'"/[^"]*?/(tests/|ci/)')


def load(path):
    det, ex = [], None
    try:
        with open(path, errors='replace') as fh:
            for line in fh:
                if 'DETLOG' in line:
                    det.append(line.split('DETLOG', 1)[1].rstrip())
                if ex is None:
                    m = EXECVE.search(line)
                    if m:
                        ex = m.group(0)
    except OSError:
        return None, None
    return ex, det


def shape(line):
    b = IOBUF.search(line)
    if b:
        return ('iobuf',) + b.groups()
    c, r = CALL.search(line), RET.search(line)
    return (c.group(1) if c else None, c.group(2) if c else None,
            r.group(1) if r else None)


def diff_kind(x, y):
    if TIMEPAY.sub('T', x) == TIMEPAY.sub('T', y):
        return 'clock'
    sx = shape(x)
    if sx == shape(y) and sx != (None, None, None):
        return 'observation'
    return 'controlflow'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--dir', default='/tmp')
    ap.add_argument('--run1-glob', default='run1_log_*')
    ap.add_argument('--run2-glob', default='run2_log_*')
    ap.add_argument('--min-prefix', type=int, default=50)
    ap.add_argument('--max-mtime-gap', type=float, default=1e9)
    ap.add_argument('--strip-prefix', action='store_true',
                    help='normalise the worktree root out of the cell key when aggregating')
    a = ap.parse_args()

    meta = {}
    for side, pat in (('run1', a.run1_glob), ('run2', a.run2_glob)):
        for p in glob.glob(os.path.join(a.dir, pat)):
            ex, det = load(p)
            if not det:
                continue
            key_src = ex or 'none'
            if a.strip_prefix:
                key_src = WORKTREE.sub('"/<wt>/\\1', key_src)
            meta[p] = dict(ex=ex or '', key=hashlib.sha1(key_src.encode()).hexdigest()[:12],
                           det=det, mt=os.path.getmtime(p), side=side)
    print(f'logs loaded: {len(meta)}', flush=True)

    groups = defaultdict(lambda: {'run1': [], 'run2': []})
    for p, m in meta.items():
        groups[m['key']][m['side']].append(p)

    pairs, unpaired = [], []
    for g in groups.values():
        used = set()
        for x in sorted(g['run1'], key=lambda p: meta[p]['mt']):
            cand = [y for y in g['run2'] if y not in used]
            if not cand:
                unpaired.append((x, 'no run2 in cell group')); continue
            y = min(cand, key=lambda y: abs(meta[x]['mt'] - meta[y]['mt']))
            if abs(meta[x]['mt'] - meta[y]['mt']) > a.max_mtime_gap:
                unpaired.append((x, 'mtime gap exceeds limit')); continue
            pre = 0
            for u, v in zip(meta[x]['det'], meta[y]['det']):
                if u != v:
                    break
                pre += 1
            if pre < a.min_prefix:
                unpaired.append((x, f'prefix only {pre} lines')); continue
            used.add(y); pairs.append((x, y))
        for y in g['run2']:
            if y not in used:
                unpaired.append((y, 'no run1 in cell group'))

    print(f'PAIRED: {len(pairs)}   UNPAIRED: {len(unpaired)}\n')
    cls = Counter()
    per_cell = defaultdict(Counter)
    for x, y in pairs:
        dx, dy = meta[x]['det'], meta[y]['det']
        cell = meta[x]['key']
        if len(dx) != len(dy):
            cls['controlflow'] += 1; per_cell[cell]['controlflow'] += 1; continue
        d = [(u, v) for u, v in zip(dx, dy) if u != v]
        if not d:
            cls['identical'] += 1; per_cell[cell]['identical'] += 1; continue
        kinds = {diff_kind(u, v) for u, v in d}
        verdict = ('controlflow' if 'controlflow' in kinds
                   else 'observation' if 'observation' in kinds else 'clock')
        cls[verdict] += 1; per_cell[cell][verdict] += 1

    print('CLASSIFICATION:')
    for k in ('identical', 'clock', 'observation', 'controlflow'):
        print(f'   {k:12} {cls[k]}')
    print('\nPER-CELL (cells with any divergence), sorted by pair count:')
    rows = sorted(per_cell.items(), key=lambda kv: -sum(kv[1].values()))
    for cell, c in rows[:40]:
        if sum(c.values()) - c['identical'] == 0:
            continue
        tot = sum(c.values())
        print(f'   {cell}  pairs={tot:4}  identical={c["identical"]:4} '
              f'clock={c["clock"]:3} obs={c["observation"]:4} cf={c["controlflow"]:4}')
    print('\nUNPAIRED REASONS:')
    for r, n in Counter(r for _, r in unpaired).most_common():
        print(f'   {n:5}  {r}')


if __name__ == '__main__':
    main()
