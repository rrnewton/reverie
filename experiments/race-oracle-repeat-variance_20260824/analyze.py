import json, glob, os, math, random
from collections import Counter

def wilson(k, n, z=1.959963985):
    if n == 0: return (0.0, 0.0)
    p = k / n
    d = 1 + z*z/n
    c = (p + z*z/(2*n)) / d
    h = z*math.sqrt(p*(1-p)/n + z*z/(4*n*n)) / d
    return (max(0.0, c-h), min(1.0, c+h))

sweeps = {'concurrent': [], 'sequential': []}
allruns = {'concurrent': [], 'sequential': []}
for f in sorted(glob.glob('/tmp/h005-oracle/reps/*.json')):
    d = json.load(open(f))
    m = d['meta']['mode']
    runs = d['runs']
    dv = sum(1 for x in runs if x['category'] == 'diverged')
    other = Counter(x['category'] for x in runs)
    loads = [x['load1_start'] for x in runs if x.get('load1_start') is not None]
    sweeps[m].append({'name': os.path.basename(f)[:-5], 'n': len(runs), 'div': dv,
                      'load': sum(loads)/len(loads) if loads else float('nan'),
                      'cats': dict(other)})
    allruns[m].extend(runs)

print("="*78)
print("PER-SWEEP DIVERGENCE COUNTS  (each sweep n=20, identical arm)")
print("="*78)
for m in ('concurrent','sequential'):
    cs = [s['div'] for s in sweeps[m]]
    print(f"{m:11} ({len(cs)} sweeps): {cs}")
    print(f"{'':11} rates: {[round(c/20*100) for c in cs]}  min={min(cs)} max={max(cs)}")
    if min(cs) > 0:
        print(f"{'':11} observed max/min RATE ratio = {max(cs)/min(cs):.2f}x")
    else:
        print(f"{'':11} observed max/min RATE ratio = INFINITE (a sweep read 0)")

print()
print("="*78)
print("POOLED RATE PER MODE, with Wilson 95%")
print("="*78)
res = {}
for m in ('concurrent','sequential'):
    n = sum(s['n'] for s in sweeps[m]); k = sum(s['div'] for s in sweeps[m])
    lo, hi = wilson(k, n)
    res[m] = (k, n)
    print(f"{m:11} {k:3}/{n:3} = {k/n*100:5.1f}%   Wilson 95% [{lo*100:.1f}%, {hi*100:.1f}%]")
    cc = Counter()
    for x in allruns[m]: cc[x['category']] += 1
    print(f"{'':11} categories: {dict(cc)}")

print()
print("="*78)
print("IS THE SWEEP-TO-SWEEP SPREAD BIGGER THAN BINOMIAL SAMPLING NOISE?")
print("  (the whole question: 'does the 5.3x survive at high N')")
print("="*78)
for m in ('concurrent','sequential'):
    cs = [s['div'] for s in sweeps[m]]
    R = len(cs); n = 20
    k, N = res[m]; p = k / N
    exp_var = n*p*(1-p)
    mean = sum(cs)/R
    ss = sum((c-mean)**2 for c in cs)
    samp_var = ss/(R-1)
    X2 = ss/exp_var
    df = R-1
    # normal approx to chi-square tail
    zc = (X2 - df) / math.sqrt(2*df)
    print(f"{m:11} mean={mean:.2f}  sample var={samp_var:.2f}  binomial var={exp_var:.2f}"
          f"  dispersion={samp_var/exp_var:.2f}")
    print(f"{'':11} chi-square dispersion X2={X2:.2f} on {df} df  (z={zc:+.2f}) "
          f"-> {'OVERDISPERSED' if zc>1.96 else 'CONSISTENT WITH BINOMIAL'}")

print()
print("="*78)
print("HOW SURPRISING IS THE ORIGINAL 11/20 vs 6/20 (55% vs 30%) UNDER ONE FIXED p?")
print("="*78)
random.seed(20260824)
for m in ('concurrent',):
    k, N = res[m]; p = k/N
    trials = 200000
    hits = 0; ratios = []
    for _ in range(trials):
        a = sum(1 for _ in range(20) if random.random() < p)
        b = sum(1 for _ in range(20) if random.random() < p)
        if abs(a-b) >= 5: hits += 1
    print(f"p taken from my pooled {m} rate = {p:.3f}")
    print(f"P(two independent n=20 sweeps differ by >= 5 counts, i.e. >= 25 points) = {hits/trials*100:.1f}%")
