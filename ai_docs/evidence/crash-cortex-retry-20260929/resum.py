#!/usr/bin/env python3
"""Independent re-sum of Claude Code transcript token usage (naive per-line and per-message)."""
import json, sys, glob, os
K=('input_tokens','output_tokens','cache_read_input_tokens','cache_creation_input_tokens')
def resum(path):
    naive=[0]*4; lines=0; seen={}
    for l in open(path, errors='replace'):
        try: d=json.loads(l)
        except Exception: continue
        if not isinstance(d,dict): continue
        m=d.get('message') if isinstance(d.get('message'),dict) else {}
        u=m.get('usage') or d.get('usage')
        if not isinstance(u,dict): continue
        lines+=1
        for i,k in enumerate(K): naive[i]+=int(u.get(k,0) or 0)
        key=(str(m.get('id') or ''), str(d.get('requestId') or ''))
        if key==('',''): key=('line',lines)
        seen.setdefault(key,u)
    dedup=[sum(int(u.get(k,0) or 0) for u in seen.values()) for k in K]
    return lines,len(seen),naive,dedup
if __name__=='__main__':
    tot_n=[0]*4; tot_d=[0]*4; tl=0; tm=0
    for p in sys.argv[1:]:
        l,m,n,d=resum(p); tl+=l; tm+=m
        tot_n=[a+b for a,b in zip(tot_n,n)]; tot_d=[a+b for a,b in zip(tot_d,d)]
        print(f"{p}: lines={l} messages={m} naive_total={sum(n):,} dedup_total={sum(d):,} dedup={dict(zip(K,d))}")
    print(f"TOTAL files={len(sys.argv)-1} lines={tl} messages={tm} naive={sum(tot_n):,} dedup={sum(tot_d):,} dedup_by_kind={dict(zip(K,tot_d))}")
