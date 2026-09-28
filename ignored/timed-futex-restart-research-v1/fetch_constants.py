from concurrent.futures import ThreadPoolExecutor
import hashlib,json,subprocess
from pathlib import Path
from html.parser import HTMLParser
D=Path(__file__).resolve().parent
class Text(HTMLParser):
    def __init__(self):super().__init__();self.parts=[]
    def handle_data(self,s):self.parts.append(s)
def one(name):
    url=f'https://man7.org/linux/man-pages/man2/{name}.2const.html'
    args=['curl','--silent','--show-error','--fail','--max-time','40',url]
    p=subprocess.run(args,capture_output=True,timeout=45)
    stem=D/f'man-v3-{name}';raw=stem.with_suffix('.html');err=stem.with_suffix('.stderr')
    assert not raw.exists() and not err.exists()
    raw.write_bytes(p.stdout);err.write_bytes(p.stderr)
    row={'url':url,'argv':args,'rc':p.returncode,'raw':str(raw),'stderr':str(err),'raw_bytes':len(p.stdout),'raw_sha256':hashlib.sha256(p.stdout).hexdigest()}
    if p.returncode==0:
        parser=Text();parser.feed(p.stdout.decode());data=''.join(parser.parts).encode()
        target=stem.with_suffix('.txt');target.write_bytes(data)
        row.update({'decoded':str(target),'sha256':hashlib.sha256(data).hexdigest(),'bytes':len(data)})
    return row
with ThreadPoolExecutor(max_workers=2) as pool:rows=list(pool.map(one,['FUTEX_WAIT','FUTEX_WAIT_BITSET']))
(D/'FETCH-CONSTANTS.json').write_text(json.dumps({'source_links':'Exact relative links from retained man-v2-futex.2.html lines144/175','requests':rows},indent=2)+'\n')
for r in rows:print(r['rc'],r['url'])
assert all(r['rc']==0 for r in rows)
