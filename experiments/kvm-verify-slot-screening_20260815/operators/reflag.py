#!/usr/bin/env python3
"""Re-run the check-mutation screen under the HARNESS's compile flags and diff verdicts.

Answers directly: can the compile-flag divergence change a verdict? Compares the
same operator, same fixtures, only the compile flags differing.
"""
import json, sys, glob, tomllib, os
sys.path.insert(0,'/home/newton/work/dev-hermit/ignored/kvm2-screen')
import screen as S
REPO=S.REPO
cf={}
for f in glob.glob(os.path.join(REPO,'tests/e2e/manifests/*.toml')):
    d=tomllib.load(open(f,'rb'))
    for t in d.get('test',[]):
        p=t.get('program') or ''
        if p.endswith('.c'): cf.setdefault(p,t.get('build',{}).get('cflags',[]) or [])
HARN=["-std=c11","-O2","-g","-Wall","-Wextra","-Werror"]
targets=json.load(open(sys.argv[1]))
out=[]
for i,p in enumerate(targets):
    S.CFLAGS = HARN + cf.get(p,[])          # harness flags, per-cell
    r=S.screen_one(p); r["flags"]="harness"
    out.append(r)
    print("%d/%d %-50s %s"%(i+1,len(targets),p.split('/',1)[1][:48],r['bucket']),flush=True)
    json.dump(out,open(sys.argv[2],"w"),indent=1)
