import json,glob,os,math,random
def wilson(k,n,z=1.959963985,deff=1.0):
    z=z*math.sqrt(deff); p=k/n; d=1+z*z/n; c=(p+z*z/(2*n))/d
    h=z*math.sqrt(p*(1-p)/n+z*z/(4*n*n))/d
    return (max(0,c-h),min(1,c+h))
S={'concurrent':[],'sequential':[]}
for f in sorted(glob.glob('/tmp/h005-oracle/reps/*.json')):
    d=json.load(open(f)); m=d['meta']['mode']; runs=d['runs']
    loads=[x['load1_start'] for x in runs if x.get('load1_start') is not None]
    S[m].append({'i':int(os.path.basename(f)[:-5].split('-')[1]),
                 'div':sum(1 for x in runs if x['category']=='diverged'),
                 'load':sum(loads)/len(loads) if loads else 0.0})
for m in S: S[m].sort(key=lambda s:s['i'])

print("DESIGN-EFFECT-CORRECTED INTERVALS (inflate by the measured dispersion)")
print("-"*74)
out={}
for m in ('concurrent','sequential'):
    cs=[s['div'] for s in S[m]]; R=len(cs); n=20; k=sum(cs); N=R*n; p=k/N
    ev=n*p*(1-p); mean=k/R; deff=max(1.0, sum((c-mean)**2 for c in cs)/(R-1)/ev)
    lo,hi=wilson(k,N,deff=deff); out[m]=(k,N,p,deff,lo,hi)
    print(f"{m:11} {k}/{N} = {p*100:.1f}%  dispersion(deff)={deff:.2f}  "
          f"corrected 95% [{lo*100:.1f}%, {hi*100:.1f}%]")
print()
c=out['concurrent']; s=out['sequential']
print(f"disjoint after correction? {'YES' if c[4] > s[5] else 'NO'}   "
      f"(conc low {c[4]*100:.1f}% vs seq high {s[5]*100:.1f}%)")
print()
print("MATCHED-LOAD CHECK: restrict BOTH modes to sweeps with mean load < 25")
print("-"*74)
tot={}
for m in ('concurrent','sequential'):
    sel=[x for x in S[m] if x['load']<25]
    k=sum(x['div'] for x in sel); N=20*len(sel)
    cs=[x['div'] for x in sel]; R=len(cs); p=k/N; ev=20*p*(1-p); mean=k/R
    deff=max(1.0,sum((x-mean)**2 for x in cs)/(R-1)/ev)
    lo,hi=wilson(k,N,deff=deff); tot[m]=(k,N,lo,hi)
    print(f"{m:11} {len(sel)} sweeps  {k}/{N} = {k/N*100:.1f}%  deff={deff:.2f}  [{lo*100:.1f}%, {hi*100:.1f}%]")
print(f"still disjoint at matched load? {'YES' if tot['concurrent'][2] > tot['sequential'][3] else 'NO'}")
print()
print("THE MAX/MIN RATIO IS A GARBAGE STATISTIC -- simulate a PERFECTLY binomial process")
print("-"*74)
random.seed(7)
for p,label in ((0.188,'sequential p=0.188'),(0.36,'concurrent p=0.360')):
    ratios=[]
    for _ in range(20000):
        cs=[sum(1 for _ in range(20) if random.random()<p) for _ in range(20)]
        if min(cs)>0: ratios.append(max(cs)/min(cs))
    ratios.sort()
    med=ratios[len(ratios)//2]
    ge53=sum(1 for r in ratios if r>=5.3)/len(ratios)*100
    print(f"{label}: 20 sweeps of n=20, EXACTLY binomial by construction")
    print(f"   median max/min ratio = {med:.1f}x   P(ratio >= 5.3x) = {ge53:.0f}%")
