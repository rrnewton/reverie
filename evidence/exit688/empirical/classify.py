import re,sys
# For every exec-edge / esrch block: find the leader's 0x6057f WAITID, the leader's previous WAITID status,
# the leader ptrace requests between that previous stop and the 0x6057f, and the first leader request after it
# (plus whether a GETEVENTMSG preceded that request).
pat=re.compile(r'^PROBE\[(\S+)\] t=(\d+) \(dt=\d+us\) caller=(\d+) target=(\d+) what=(.*) rc=(-?\d+) site=(\S+)$')
REQ=re.compile(r'PTRACE_(CONT|SINGLESTEP|SYSCALL|DETACH|INTERRUPT)|pidfd_send_signal|tgkill|kill')
for path in sys.argv[1:]:
    test=None; blocks={}
    for line in open(path,errors='replace'):
        m=re.match(r'^---- (\S+) stdout ----',line)
        if m: test=m.group(1).split('::')[-1]; continue
        m=pat.match(line.strip())
        if m: blocks.setdefault((test,m.group(1)),[]).append(m.groups())
    for (test,label),recs in blocks.items():
        if label not in ('exec-edge-ok','exec-edge-NOSTATUS'): continue
        L=int(recs[-1][3])
        lr=[r for r in recs if int(r[3])==L]
        idx=[i for i,r in enumerate(lr) if '0x6057f' in r[4]]
        if not idx: print(path.split('/')[-1],test,label,'NO-0x6057f'); continue
        i=idx[0]
        prev=[j for j in range(i) if 'WAITID' in lr[j][4]]
        pj=prev[-1] if prev else -1
        prevst=re.search(r'"(0x[0-9a-f]+)"',lr[pj][4]).group(1) if pj>=0 else '?'
        between=[lr[j] for j in range(pj+1,i) if REQ.search(lr[j][4])]
        after=[r for r in lr[i+1:] if REQ.search(r[4]) or 'GETEVENTMSG-after' in r[4]]
        first=after[0] if after else None
        def s(r): return f"{r[4].split(' snapshot')[0][:40]}@{r[6].split('/')[-1]}" + (f" rc={r[5]}" if 'GETEVENTMSG' in r[4] else '')
        print(f"{path.split('/')[-1]:16s} {test[-42:]:42s} {label:18s} prev_stop={prevst:8s} reqs_between={[s(r) for r in between]} first_after={s(first) if first else None}")
