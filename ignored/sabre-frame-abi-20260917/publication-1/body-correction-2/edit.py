import hashlib,json,os,subprocess,time
from pathlib import Path
D=Path(__file__).parent;P=D.parent;S=P.parents[2];ROOT=Path('/home/newton/work/dev-hermit');GH=str(ROOT/'ci-hub/bin/gh');HEAD='61098967f47041f96a43995f5272412cf9fe5761'
os.environ['TG_DB_PATH']='/home/newton/.tg/hermit2.db'
def save(name,value):
 with (D/name).open('x') as f:json.dump(value,f,indent=2);f.write('\n')
def call(name,argv):
 t=time.monotonic();r=subprocess.run(argv,cwd=S,env=os.environ,capture_output=True,timeout=90)
 (D/(name+'.stdout')).write_bytes(r.stdout);(D/(name+'.stderr')).write_bytes(r.stderr)
 save(name+'.json',dict(argv=argv,actual_exit=r.returncode,seconds=time.monotonic()-t,stdout_path=name+'.stdout',stderr_path=name+'.stderr'))
 assert r.returncode==0,name
 return r.stdout
assert call('source-head-before',['git','rev-parse','HEAD']).decode().strip()==HEAD
assert not call('source-status-before',['git','status','--porcelain=v1','--untracked-files=all']).strip()
body=(D/'body-v3.md').read_bytes();assert hashlib.sha256(body).hexdigest()=='6a59025af378be517d5d30fe988614e036175be7bb21942ffc0ae8afdcb185ea'
before=json.loads(call('pr-before',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/563']))
assert before['head']['sha']==HEAD
assert before['body'].encode()==(P/'body-correction-1/body-v2.md').read_bytes() or before['body'].encode()==body
call('edit',['with-proxy',GH,'pr','edit','563','--repo','rrnewton/reverie','--body-file',str(D/'body-v3.md')])
after=json.loads(call('pr-after',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/563']))
(D/'remote-body.md').write_bytes(after['body'].encode())
assert after['head']['sha']==HEAD and after['body'].encode()==body
assert call('source-head-after',['git','rev-parse','HEAD']).decode().strip()==HEAD
assert not call('source-status-after',['git','status','--porcelain=v1','--untracked-files=all']).strip()
x=dict(url=after['html_url'],head=HEAD,body_sha256=hashlib.sha256(body).hexdigest(),body_bytes=len(body),exact_body_readback_equal=True,source_unchanged=True,state=after['state'],merged=after['merged'],merged_at=after['merged_at'],labels=[x['name'] for x in after['labels']],no_self_attestation=True,no_source_mutation=True)
save('READBACK.json',x);print(json.dumps(x),flush=True)
