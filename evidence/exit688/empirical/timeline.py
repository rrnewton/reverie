import re,sys,collections
# Parse PROBE[...] lines; group by (label, leader) blocks per test section.
for path in sys.argv[1:]:
    test=None
    blocks=collections.OrderedDict()
    for line in open(path, errors='replace'):
        m=re.match(r'^---- (\S+) stdout ----',line)
        if m: test=m.group(1).split('::')[-1]; continue
        if line.startswith('PROBE_EXITSTOP_GETEVENT_ESRCH'):
            print(f"{path}: {test}: {line.strip()[:600]}")
        m=re.match(r'^PROBE\[(\S+)\] t=(\d+) \(dt=\d+us\) caller=(\d+) target=(\d+) what=(.*) rc=(-?\d+) site=(\S+)$',line.strip())
        if not m: continue
        label,t,caller,target,what,rc,site=m.groups()
        key=(test,label)
        blocks.setdefault(key,[]).append((int(t),caller,target,what,rc,site))
    for (test,label),recs in blocks.items():
        t0=recs[0][0]
        print(f"== {path.split('/')[-1]} {test} [{label}]")
        for t,caller,target,what,rc,site in recs:
            w=what
            w=re.sub(r'thread=Some\("guest-\d+"\) ','',w)
            if 'snapshot' in w: w=w[:400]
            print(f"  +{(t-t0)/1000:9.1f}us tgt={target} {w} rc={rc} @{site.replace('reverie-ptrace/src/','').replace('safeptrace/src/','sp/')}")
