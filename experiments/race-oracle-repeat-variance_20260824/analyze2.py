import json, glob, os, math
from collections import Counter

def wilson(k,n,z=1.959963985):
    p=k/n; d=1+z*z/n; c=(p+z*z/(2*n))/d
    h=z*math.sqrt(p*(1-p)/n+z*z/(4*n*n))/d
    return (max(0,c-h),min(1,c+h))
def pearson(x,y):
    n=len(x); mx=sum(x)/n; my=sum(y)/n
    sx=math.sqrt(sum((a-mx)**2 for a in x)); sy=math.sqrt(sum((b-my)**2 for b in y))
    return sum((a-mx)*(b-my) for a,b in zip(x,y))/(sx*sy) if sx and sy else float('nan')

S={'concurrent':[],'sequential':[]}; ALL={'concurrent':[],'sequential':[]}
for f in sorted(glob.glob('/tmp/h005-oracle/reps/*.json')):
    d=json.load(open(f)); m=d['meta']['mode']; runs=d['runs']
    idx=int(os.path.basename(f)[:-5].split('-')[1])
    loads=[x['load1_start'] for x in runs if x.get('load1_start') is not None]
    S[m].append({'i':idx,'div':sum(1 for x in runs if x['category']=='diverged'),
                 'load':sum(loads)/len(loads) if loads else float('nan'),
                 't':min(x['started_at'] for x in runs)})
    ALL[m].extend(runs)
for m in S: S[m].sort(key=lambda s:s['i'])

print("="*78); print("DOES LOAD OR TIME EXPLAIN THE CONCURRENT OVERDISPERSION?"); print("="*78)
for m in ('concurrent','sequential'):
    ss=S[m]; div=[s['div'] for s in ss]; ld=[s['load'] for s in ss]; order=[s['i'] for s in ss]
    print(f"{m:11} corr(divergences, mean load) = {pearson(div,ld):+.3f}")
    print(f"{'':11} corr(divergences, sweep order) = {pearson(div,order):+.3f}")
    print(f"{'':11} load range across sweeps: {min(ld):.1f} .. {max(ld):.1f}")

print()
print("="*78); print("DROP THE LAST 3 CONCURRENT SWEEPS (10,13,16) AND RE-TEST DISPERSION"); print("="*78)
for m in ('concurrent',):
    for cut,label in ((20,'all 20 sweeps'),(17,'first 17 sweeps')):
        cs=[s['div'] for s in S[m][:cut]]
        R=len(cs); n=20; k=sum(cs); N=R*n; p=k/N
        ev=n*p*(1-p); mean=k/R; sq=sum((c-mean)**2 for c in cs)
        X2=sq/ev; df=R-1; z=(X2-df)/math.sqrt(2*df)
        lo,hi=wilson(k,N)
        print(f"{label:16} {k:3}/{N:3} = {p*100:4.1f}% [{lo*100:.1f},{hi*100:.1f}]  "
              f"dispersion={sq/(R-1)/ev:.2f}  X2={X2:.1f}/{df}df  z={z:+.2f} "
              f"-> {'OVERDISPERSED' if z>1.96 else 'consistent with binomial'}")

print()
print("="*78); print("CONCURRENT vs SEQUENTIAL, two-proportion test at n=400 each"); print("="*78)
k1=sum(s['div'] for s in S['concurrent']); n1=400
k2=sum(s['div'] for s in S['sequential']); n2=400
p1,p2=k1/n1,k2/n2; pp=(k1+k2)/(n1+n2)
se=math.sqrt(pp*(1-pp)*(1/n1+1/n2)); z=(p1-p2)/se
print(f"concurrent {k1}/{n1}={p1*100:.1f}%  sequential {k2}/{n2}={p2*100:.1f}%")
print(f"z = {z:.2f}   ratio = {p1/p2:.2f}x")
print(f"Wilson intervals: conc {tuple(round(v*100,1) for v in wilson(k1,n1))}  "
      f"seq {tuple(round(v*100,1) for v in wilson(k2,n2))}  -> "
      f"{'DISJOINT' if wilson(k1,n1)[0] > wilson(k2,n2)[1] else 'OVERLAP'}")

print()
print("="*78); print("FIRST-DIVERGENCE POINT: IS THE MINIMUM STILL MOVING WITH N?"); print("="*78)
for m in ('concurrent','sequential'):
    recs=[x['info_record'] for x in ALL[m] if x['category']=='diverged' and x.get('info_record')]
    recs_in_order=[x['info_record'] for x in sorted([r for r in ALL[m] if r['category']=='diverged' and r.get('info_record')], key=lambda r:r['started_at'])]
    print(f"{m:11} n_diverged_with_record={len(recs)}  distinct values={sorted(Counter(recs).items())[:8]}")
    run_min=[]; cur=None
    for v in recs_in_order:
        cur=v if cur is None else min(cur,v); run_min.append(cur)
    if run_min:
        marks=[1,5,10,25,50,100,len(run_min)]
        print(f"{'':11} running minimum at N=", {k:run_min[k-1] for k in marks if k<=len(run_min)})
        # last change point
        lastdrop=max((i for i in range(1,len(run_min)) if run_min[i]<run_min[i-1]), default=0)
        print(f"{'':11} last time the minimum moved: observation #{lastdrop+1} of {len(run_min)}")
