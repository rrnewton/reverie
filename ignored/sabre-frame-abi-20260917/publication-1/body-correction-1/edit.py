import hashlib,json,os,subprocess,time
from pathlib import Path
D=Path(__file__).parent;P=D.parent;S=P.parents[2];ROOT=Path('/home/newton/work/dev-hermit');GH=str(ROOT/'ci-hub/bin/gh');HEAD='61098967f47041f96a43995f5272412cf9fe5761'
os.environ['TG_DB_PATH']='/home/newton/.tg/hermit2.db'
def save(name,value):
 with (D/name).open('x') as f:json.dump(value,f,indent=2);f.write('\n')
def call(name,argv,allowed=(0,),extra=None):
 env=dict(os.environ);env.update(extra or {});t=time.monotonic();r=subprocess.run(argv,cwd=S,env=env,text=True,capture_output=True,timeout=60);save(name+'.json',dict(argv=argv,actual_exit=r.returncode,seconds=time.monotonic()-t,stdout=r.stdout,stderr=r.stderr));assert r.returncode in allowed,name;return r.stdout
before=json.loads(call('pr-before',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/563']))
assert before['head']['sha']==HEAD and before['body'].replace('\r\n','\n').strip()==(P/'body.md').read_text().strip()
call('edit',['with-proxy',GH,'pr','edit','563','--repo','rrnewton/reverie','--body-file',str(D/'body-v2.md')])
after=json.loads(call('pr-after',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/563']))
body=(D/'body-v2.md').read_text();assert after['head']['sha']==HEAD and after['body'].replace('\r\n','\n').strip()==body.strip()
pe=dict(PR_LABELS='\n'.join(x['name'] for x in after['labels']),PR_BODY=after['body'],PR_NUMBER='563',PR_IS_KVM='false');save('policy-input.json',pe)
call('policy',[str(ROOT/'hermit/scripts/core-review-protocol-lint.sh')],allowed=(0,1),extra=pe)
assert call('source-head',['git','rev-parse','HEAD']).strip()==HEAD and not call('source-status',['git','status','--porcelain=v1','--untracked-files=all']).strip()
save('READBACK.json',dict(url=after['html_url'],head=HEAD,body_sha256=hashlib.sha256(body.encode()).hexdigest(),body_readback_equal=True,labels=[x['name'] for x in after['labels']],source_unchanged=True,no_self_attestation=True))
print(json.dumps(dict(url=after['html_url'],body_sha256=hashlib.sha256(body.encode()).hexdigest(),labels=[x['name'] for x in after['labels']]),indent=2),flush=True)
