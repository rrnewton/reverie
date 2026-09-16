#!/usr/bin/env python3
"""Diff a post-fix validate log against the pinned pre-fix baseline."""
import json, re, sys, pathlib, collections
base = json.load(open(pathlib.Path(__file__).parent / "baseline-a836d76515fa.json"))
text = pathlib.Path(sys.argv[1]).read_text(errors="replace")

node_pat = re.compile(r'^\[([a-z0-9_]+\.[a-z0-9_]+)\]\s+(.*)$')
fail_pat = re.compile(r'\b(?:TRY \d+ )?FAIL\b.*?\s([A-Za-z0-9_]+)\s*$')
now = collections.defaultdict(set)
for line in text.splitlines():
    m = node_pat.match(line)
    if not m: continue
    f = fail_pat.search(m.group(2))
    if f and 'FAIL' in m.group(2) and '✗' not in m.group(2):
        now[m.group(1)].add(f.group(1))

old = {n: set(v) for n, v in base["failing_tests_by_node"].items()}
allnodes = sorted(set(old) | set(now))
cleared = fresh = 0
print(f"{'node':28} {'before':>6} {'after':>6}   cleared / new")
for n in allnodes:
    b, a = old.get(n, set()), now.get(n, set())
    c, f = b - a, a - b
    cleared += len(c); fresh += len(f)
    print(f"{n:28} {len(b):>6} {len(a):>6}   -{len(c)} / +{len(f)}")
    for t in sorted(c): print(f"      CLEARED  {t}")
    for t in sorted(f): print(f"      NEW      {t}")
print(f"\nTOTAL cleared={cleared}  new={fresh}")
print(f"PREDICTION: clear={base['prediction']['clear']} "
      f"liteinst_strict_after={base['prediction']['liteinst_strict_after']}")
print(f"ACTUAL:     clear={cleared} liteinst_strict_after={len(now.get('test.liteinst_strict', set()))}")
print("\n=== e2e cells (baseline had 3) ===")
for cell in base["e2e_cells"]:
    name = cell.split(" (")[0]
    still = f"FAIL {name} (" in text
    print(f"  {'STILL FAILING' if still else 'CLEARED      '}  {cell}")
