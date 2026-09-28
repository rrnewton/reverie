import re, sys, collections, glob, os
d = sys.argv[1] if len(sys.argv) > 1 else 'campaign'
for f in sorted(glob.glob(d + '/*.out')):
    sc = os.path.basename(f)[:-4]
    lines = [l for l in open(f) if l.startswith('iter=')]
    c = collections.Counter()
    for l in lines:
        kv = dict(re.findall(r' (\w+)=(\S+)', l))
        key = []
        for k in ['lmsg','lmsg_pre','lmsg_post','lmsg_after_cont','lcont','intr','held_arrivals','held_state','state_pre','state_post','errno','zombie_state','proc_exit_code','waitid_L','waitpid_L','waitid_formerT','proc_formerT','held_leader_tid_now','leader_exit_stop','exec','leader_wexit_before_exec','order']:
            if k in kv: key.append(f'{k}={kv[k]}')
        if 'TIMEOUT' in l: key.append('TIMEOUT')
        if 'NO_ESRCH_1s' in l: key.append('NO_ESRCH_1s')
        if 'lmsg_changed' in l: key.append('LMSG_CHANGED')
        if 'exec_msg' in kv and 'T' in kv:
            key.append('exec_msg==T' if int(kv['exec_msg'], 16) == int(kv['T']) else 'exec_msg!=T')
        if sc == 'S3p':
            key.append('zhits>0' if int(kv.get('zombie_1792_samples', '0')) > 0 else 'zhits=0')
        c[' '.join(key)] += 1
    print(f'== {sc}: {len(lines)} iterations')
    for k, v in c.most_common():
        print(f'  {v:4d}  {k}')
    if sc == 'S3a':
        us = sorted(int(m) for l in lines for m in re.findall(r'esrch_after_us=(\d+)', l))
        if us: print(f'  esrch_after_us min/median/max = {us[0]}/{us[len(us)//2]}/{us[-1]}')
