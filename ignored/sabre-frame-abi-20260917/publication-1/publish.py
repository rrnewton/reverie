import hashlib,json,os,re,subprocess,time
from pathlib import Path
D=Path(__file__).parent;S=D.parents[2];HEAD='61098967f47041f96a43995f5272412cf9fe5761';TREE='a575118fc65729e451e64f3c328510075ae38947';BASE='24cd5bb518b027eddb62a226805210d74d31c3d8';REF='refs/heads/dev-hermit/sabre-frame-abi-20260917';TASK='hermit-main-tip-d0b1468b-not-validated-22-nodes-mostly-exit-2';ROOT=Path('/home/newton/work/dev-hermit');GH=str(ROOT/'ci-hub/bin/gh')
def save(n,x):
 with (D/n).open('x') as f:json.dump(x,f,indent=2);f.write('\n')
def call(label,argv,allowed=(0,),extra_env=None):
 env=dict(os.environ)
 if extra_env:env.update(extra_env)
 t=time.monotonic();p=subprocess.run(argv,cwd=S,env=env,capture_output=True,text=True);save(label+'.json',dict(argv=argv,actual_exit=p.returncode,seconds=time.monotonic()-t,stdout=p.stdout,stderr=p.stderr));assert p.returncode in allowed,label;return p.stdout
os.environ.update(TG_DB_PATH='/home/newton/.tg/hermit2.db',CARGO_HOME=str(D.parent/'native-build-3/cargo'),CARGO_TARGET_DIR=str(D.parent/'native-build-3/target'),CARGO_BUILD_JOBS='4',XDG_CACHE_HOME=str(D/'xdg'))
(D/'xdg').mkdir(mode=0o700)
assert json.loads((D.parent/'native-build-3/TERMINAL.json').read_text())['cgroup_absent']
assert json.loads((D.parent/'commit-2/TERMINAL.json').read_text())['cgroup_absent']
assert call('source-head',['git','rev-parse','HEAD']).strip()==HEAD and call('source-tree',['git','rev-parse','HEAD^{tree}']).strip()==TREE
assert not call('source-status',['git','status','--porcelain=v1','--untracked-files=all']).strip()
assert call('branch',['git','branch','--show-current']).strip()==REF.removeprefix('refs/heads/')
frozen=json.loads((D.parent/'final-1/SOURCE.json').read_text())
for r in frozen:
 if r.get('type')!='gitlink':
  p=S/r['path'];b=os.fsencode(os.readlink(p)) if r['mode']=='120000' else p.read_bytes();assert hashlib.sha256(b).hexdigest()==r['sha256'],r['path']
assert not call('remote-ref-before',['with-proxy','git','ls-remote','--exit-code','origin',REF],allowed=(2,)).strip()
main=call('remote-main-before',['with-proxy','git','ls-remote','--exit-code','origin','refs/heads/main']).split()[0]
if main!=BASE:
 call('fetch-current-main-object',['with-proxy','git','fetch','--no-tags','--no-write-fetch-head','--refmap=','origin',main]);call('incoming-main-diff',['git','diff','--binary','--full-index',BASE+'..'+main])
label=json.loads(call('routing-label',['with-proxy',GH,'api','repos/rrnewton/reverie/labels/post-facto-human-review']));assert label['name']=='post-facto-human-review'
tag=call('who-am-i',[str(ROOT/'ci-hub/bin/who-am-i'),'--tag','--role','impl','--task',TASK,'--repo-root',str(ROOT),'--db','/home/newton/.tg/hermit2.db']).strip();assert tag.startswith('[') and tag.endswith(']') and '\n' not in tag
body=(D/'body-template.md').read_text().replace('{disclosure}',tag);(D/'body.md').write_text(body)
call('push',['with-proxy',str(ROOT/'ci-hub/bin/git-push-verified'),'origin',HEAD+':'+REF])
assert call('remote-ref-after',['with-proxy','git','ls-remote','--exit-code','origin',REF]).split()[0]==HEAD
commit=json.loads(call('remote-commit',['with-proxy',GH,'api','repos/rrnewton/reverie/git/commits/'+HEAD]));assert commit['sha']==HEAD and commit['tree']['sha']==TREE
remote=json.loads(call('remote-tree',['with-proxy',GH,'api','repos/rrnewton/reverie/git/trees/'+TREE+'?recursive=1']));assert remote['sha']==TREE and not remote['truncated'];entries={r['path']:r for r in remote['tree'] if r['type'] in ['blob','commit']};assert set(entries)=={r['path'] for r in frozen}
for r in frozen:assert entries[r['path']]['sha']==r['git_object'] and entries[r['path']]['mode']==r['mode'],r['path']
url=call('create-pr',['with-proxy',GH,'--task',TASK,'pr','create','--repo','rrnewton/reverie','--draft','--base','main','--head',REF.removeprefix('refs/heads/'),'--title',(D/'title.txt').read_text().strip(),'--body-file',str(D/'body.md'),'--label','post-facto-human-review']).strip();assert re.fullmatch(r'https://github.com/rrnewton/reverie/pull/[0-9]+',url),url
print(url,flush=True)
number=url.rsplit('/',1)[1];pr=json.loads(call('pr-readback',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/'+number]));assert pr['head']['sha']==HEAD and pr['draft'] and pr['title']==(D/'title.txt').read_text().strip();assert pr['body'].replace('\r\n','\n').strip()==body.strip();assert 'post-facto-human-review' in [x['name'] for x in pr['labels']]
files=json.loads(call('pr-files',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/'+number+'/files?per_page=100']));owned=json.loads((D.parent/'final-1/IDENTITY.json').read_text())['paths'];assert sorted(r['filename'] for r in files)==sorted(owned)
for r in files:assert r['sha']==entries[r['filename']]['sha'],r['filename']
note='Draft upstream frame ABI repair: '+url+' at '+HEAD+' / tree '+TREE+'. Exact ten-path content and all2555 remote source entries verified after normal hooked feature push. Native original12 failures/corrected12 passes; three actual Rust wrapper/child-return methods and existing vfork/register tests pass; normal fmt/Clippy pass. Trigger2 post-facto-human-review routing applied. Native independent source review approves; actual Claude review and manual gate remain pending. No Hermit guest, full-main claim, merge or unlanded consumer pin. Existing owner dev-hermit retains this task.\n';(D/'task-note.txt').write_text(note)
call('task-note',[str(ROOT/'ci-hub/bin/tg-note-verified'),TASK,'--file',str(D/'task-note.txt')])
# Read actual published labels before the policy check; do not grant review labels.
pr=json.loads(call('pr-policy-readback',['with-proxy',GH,'api','repos/rrnewton/reverie/pulls/'+number]));policy_env=dict(PR_LABELS='\n'.join(x['name'] for x in pr['labels']),PR_BODY=pr['body'],PR_NUMBER=number,PR_IS_KVM='false');save('policy-input.json',policy_env)
call('core-review-policy',[str(ROOT/'hermit/scripts/core-review-protocol-lint.sh')],allowed=(0,1),extra_env=policy_env)
call('check-runs',['with-proxy',GH,'api','repos/rrnewton/reverie/commits/'+HEAD+'/check-runs?per_page=100'])
call('combined-status',['with-proxy',GH,'api','repos/rrnewton/reverie/commits/'+HEAD+'/status'])
assert call('source-final',['git','rev-parse','HEAD']).strip()==HEAD and not call('status-final',['git','status','--porcelain=v1','--untracked-files=all']).strip()
save('PUBLISHED.json',dict(url=url,head=HEAD,tree=TREE,draft=True,task=TASK,remote_all_source_entries_equal=len(entries),all10prfiles_equal=True,body_readback_equal_after_GitHub_newline_normalization=True,body_sha256=hashlib.sha256(body.encode()).hexdigest(),no_merge=True,no_self_attestation=True,source_main_before=main));print('publication readback complete',flush=True)
