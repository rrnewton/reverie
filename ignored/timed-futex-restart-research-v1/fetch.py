import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import subprocess

D = Path(__file__).resolve().parent
REV = '199c9959d3a9b53f346c221757fc7ac507fbac50'
kernel = ['kernel/futex/waitwake.c', 'kernel/futex/syscalls.c',
          'kernel/futex/futex.h', 'include/linux/restart_block.h',
          'include/linux/errno.h']
docs = ['FUTEX_WAIT.2const', 'FUTEX_WAIT_BITSET.2const', 'restart_syscall.2', 'futex.2']
requests = [(f'linux-{p.replace("/", "--")}',
             f'https://api.github.com/repos/gregkh/linux/contents/{p}?ref={REV}', True)
            for p in kernel]
requests += [(f'man-{p}', f'https://man7.org/linux/man2/{p}.html', False) for p in docs]

def one(item):
    name,url,is_kernel=item
    args=['curl','--silent','--show-error','--fail','--max-time','40',url]
    p=subprocess.run(args,capture_output=True,timeout=45)
    raw=D/(name+('.api.json' if is_kernel else '.html'))
    err=D/(name+'.stderr')
    assert not raw.exists() and not err.exists()
    raw.write_bytes(p.stdout);err.write_bytes(p.stderr)
    row={'url':url,'argv':args,'rc':p.returncode,'raw':str(raw),'stderr':str(err),
         'raw_sha256':hashlib.sha256(p.stdout).hexdigest(),'raw_bytes':len(p.stdout)}
    if p.returncode==0 and is_kernel:
        obj=json.loads(p.stdout);data=base64.b64decode(obj['content'])
        blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
        assert blob==obj['sha'] and len(data)==obj['size']
        path=D/name;assert not path.exists();path.write_bytes(data)
        row.update({'decoded':str(path),'git_blob':blob,'bytes':len(data),
                    'sha256':hashlib.sha256(data).hexdigest(),'revision':REV})
    return row

with ThreadPoolExecutor(max_workers=3) as pool:
    rows=list(pool.map(one,requests))
(D/'FETCH.json').write_text(json.dumps({'revision':REV,'requests':rows},indent=2)+'\n')
for r in rows: print(r['rc'],r['url'])
assert all(r['rc']==0 for r in rows)
