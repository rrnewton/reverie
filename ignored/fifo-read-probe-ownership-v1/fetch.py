import base64,hashlib,json,subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from html.parser import HTMLParser
D=Path(__file__).resolve().parent
REV='199c9959d3a9b53f346c221757fc7ac507fbac50'
items=[('linux-'+p.replace('/','--'),f'https://api.github.com/repos/gregkh/linux/contents/{p}?ref={REV}',True) for p in ['fs/pipe.c','fs/read_write.c','include/linux/fs.h']]
items += [('man-'+p.replace('/','--'),f'https://man7.org/linux/man-pages/{p}.html',False) for p in ['man2/open.2','man2/F_SETFL.2const','man2/dup.2','man7/fifo.7','man2/poll.2','man7/unix.7','man2/preadv.2']]
class Text(HTMLParser):
    def __init__(self):super().__init__();self.parts=[]
    def handle_data(self,s):self.parts.append(s)
def one(item):
    name,url,kernel=item;args=['curl','--silent','--show-error','--fail','--max-time','40',url]
    p=subprocess.run(args,capture_output=True,timeout=45)
    raw=D/(name+('.api.json' if kernel else '.html'));err=D/(name+'.stderr')
    assert not raw.exists() and not err.exists();raw.write_bytes(p.stdout);err.write_bytes(p.stderr)
    row={'url':url,'argv':args,'rc':p.returncode,'raw':str(raw),'stderr':str(err),'raw_sha256':hashlib.sha256(p.stdout).hexdigest(),'raw_bytes':len(p.stdout)}
    if p.returncode==0:
        if kernel:
            x=json.loads(p.stdout);b=base64.b64decode(x['content']);blob=hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest();assert len(b)==x['size'] and blob==x['sha'];row.update({'revision':REV,'git_blob':blob})
        else:
            parser=Text();parser.feed(p.stdout.decode());b=''.join(parser.parts).encode()
        dest=D/(name if kernel else name+'.txt');dest.write_bytes(b);row.update({'decoded':str(dest),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()})
    return row
with ThreadPoolExecutor(max_workers=3) as pool: rows=list(pool.map(one,items))
(D/'FETCH.json').write_text(json.dumps({'revision':REV,'requests':rows},indent=2)+'\n')
for r in rows:print(r['rc'],r['url'])
assert all(r['rc']==0 for r in rows)
