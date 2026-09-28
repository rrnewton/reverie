import re,sys
pat=re.compile(r'^PROBE\[(\S+)\] t=(\d+) \(dt=\d+us\) caller=(\d+) target=(\d+) what=(.*) rc=(-?\d+) site=(\S+)$')
for path in sys.argv[1:]:
    test=None; blocks={}
    for line in open(path,errors='replace'):
        m=re.match(r'^---- (\S+) stdout ----',line)
        if m: test=m.group(1).split('::')[-1]; continue
        m=pat.match(line.strip())
        if m: blocks.setdefault((test,m.group(1)),[]).append(m.groups())
    for (test,label),recs in blocks.items():
        if label!='exec-edge-NOSTATUS': continue
        L=int(recs[-1][3]); F=[int(r[3]) for r in recs if int(r[3])!=L][0]
        T=lambda pred: next((int(r[1]) for r in recs if pred(r)),None)
        clone57f=[int(r[1]) for r in recs if int(r[3])==L and '"0x57f"' in r[4]][-1]
        fcont=T(lambda r:int(r[3])==F and 'PTRACE_CONT' in r[4])
        ex=T(lambda r:int(r[3])==L and '0x6057f' in r[4])
        stale=[int(r[1]) for r in recs if int(r[3])==L and 'PTRACE_CONT(resume,' in r[4] and int(r[1])>clone57f][0]
        exf=T(lambda r:int(r[3])==L and 'ExitFuture resolved' in r[4])
        ge=T(lambda r:int(r[3])==L and 'GETEVENTMSG-after' in r[4] and 'tracer.rs' in r[6])
        echild=T(lambda r:int(r[3])==F and 'ECHILD' in r[4])
        us=lambda a,b: f"{(a-b)/1000:8.1f}" if a and b else '     n/a'
        print(f"{path.split('/')[-1]:12s} {test[-40:]:40s} leader57f->Fcont={us(fcont,clone57f)} Fcont->EXITstop={us(ex,fcont)} EXITstop->staleCONT={us(stale,ex)} staleCONT->ExitFuture={us(exf,stale)} ->getevent(ESRCH)={us(ge,exf)} FormerECHILD-getevent={us(echild,ge)}")
