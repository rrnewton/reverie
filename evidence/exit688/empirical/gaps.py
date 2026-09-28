import re,sys
# For each exec-edge block: leader L, former F. Compute: tF_cont (first CONT to F), tL_cont_after (first resume of L after tF_cont),
# tL_exit (0x6057f WAITID of L). Report gaps in us and ordering.
pat=re.compile(r'^PROBE\[(\S+)\] t=(\d+) \(dt=\d+us\) caller=(\d+) target=(\d+) what=(.*) rc=(-?\d+) site=(\S+)$')
for path in sys.argv[1:]:
    test=None; blocks={}
    for line in open(path,errors='replace'):
        m=re.match(r'^---- (\S+) stdout ----',line)
        if m: test=m.group(1).split('::')[-1]; continue
        m=pat.match(line.strip())
        if m: blocks.setdefault((test,m.group(1)),[]).append(m.groups())
    for (test,label),recs in blocks.items():
        if not label.startswith('exec-edge') and label!='esrch-immediate': continue
        L=int(recs[-1][3]) if label!='esrch-immediate' else int(recs[0][3])
        tg=set(int(r[3]) for r in recs)
        F=[x for x in tg if x!=L]
        F=F[0] if F else None
        def t(r): return int(r[1])
        fcont=[t(r) for r in recs if F and int(r[3])==F and 'PTRACE_CONT' in r[4]]
        lexit=[t(r) for r in recs if int(r[3])==L and '0x6057f' in r[4]]
        if not fcont or not lexit: print(path.split('/')[-1],test,label,'incomplete'); continue
        lres=[(t(r),r[4].split('(')[0],r[6].split('/')[-1]) for r in recs if int(r[3])==L and t(r)>fcont[0] and re.match(r'PTRACE_(CONT|SINGLESTEP|SYSCALL)',r[4])]
        print(f"{path.split('/')[-1]:22s} {test[-45:]:45s} {label:18s} Fcont->Lexit={(lexit[0]-fcont[0])/1000:8.1f}us  Lresumes-after-Fcont=" + ", ".join(f"{(a-fcont[0])/1000:.1f}us {b}@{c}{'(AFTER-EXITSTOP)' if a>lexit[0] else ''}" for a,b,c in lres))
