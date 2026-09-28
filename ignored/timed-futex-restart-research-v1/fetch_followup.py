import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib,json
from pathlib import Path
import subprocess
from html.parser import HTMLParser
D=Path(__file__).resolve().parent
REV='199c9959d3a9b53f346c221757fc7ac507fbac50'
requests=[('linux-include--linux--thread_info.h',f'https://api.github.com/repos/gregkh/linux/contents/include/linux/thread_info.h?ref={REV}',True)]
for name in ['FUTEX_WAIT.2const','FUTEX_WAIT_BITSET.2const','restart_syscall.2','futex.2']:
    section='man2const' if name.endswith('2const') else 'man2'
    requests.append((f'man-v2-{name}',f'https://man7.org/linux/man-pages/{section}/{name}.html',False))
class Text(HTMLParser):
    def __init__(self): super().__init__();self.parts=[]
    def handle_data(self,data): self.parts.append(data)
def fetch(item):
    name,url,kernel=item
    args=['curl','--silent','--show-error','--fail','--max-time','40',url]
    proc=subprocess.run(args,capture_output=True,timeout=45)
    raw=D/(name+('.api.json' if kernel else '.html'));err=D/(name+'.stderr')
    assert not raw.exists() and not err.exists()
    raw.write_bytes(proc.stdout);err.write_bytes(proc.stderr)
    row={'url':url,'argv':args,'rc':proc.returncode,'raw':str(raw),'stderr':str(err),
         'raw_sha256':hashlib.sha256(proc.stdout).hexdigest(),'raw_bytes':len(proc.stdout)}
    if proc.returncode==0:
        if kernel:
            obj=json.loads(proc.stdout);data=base64.b64decode(obj['content'])
            blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
            assert blob==obj['sha'] and len(data)==obj['size']
            row.update({'git_blob':blob,'revision':REV})
        else:
            parser=Text();parser.feed(proc.stdout.decode());data=''.join(parser.parts).encode()
        target=D/(name if kernel else name+'.txt');target.write_bytes(data)
        row.update({'decoded':str(target),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()})
    return row
with ThreadPoolExecutor(max_workers=3) as pool: rows=list(pool.map(fetch,requests))
(D/'FETCH-FOLLOWUP.json').write_text(json.dumps({'revision':REV,'requests':rows,'earlier_404s_preserved':True},indent=2)+'\n')
for row in rows: print(row['rc'],row['url'])
assert all(row['rc']==0 for row in rows)
