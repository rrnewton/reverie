import difflib,hashlib,json,os,shutil,subprocess,sys
from pathlib import Path
repo=Path(__file__).resolve().parents[2]; own=Path(__file__).resolve().parent;old=repo/'ignored/timer-integration-20260918/frozen-v1';q=own/'qualification-v6';out=own/'frozen-v2';out.mkdir(exist_ok=True)
def digest(data):return hashlib.sha256(data).hexdigest()
def record(p):
 p=Path(p).resolve();raw=p.read_bytes();return dict(path=str(p),bytes=len(raw),sha256=digest(raw))
def check(r):
 p=Path(r['path']);assert p.stat().st_size==r['bytes'] and digest(p.read_bytes())==r['sha256'],r['path']
def write(name,value):
 p=out/name;p.parent.mkdir(parents=True,exist_ok=True);assert not p.exists();p.write_text(json.dumps(value,indent=2)+'\n');return record(p)
def git(*args):return subprocess.check_output(['/usr/bin/git','-C',str(repo),*args],env={**os.environ,'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1'})
def blob(raw):return hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
def patch(path,before,after):
 if before==after:return b''
 header=f'diff --git a/{path} b/{path}\n'
 if before is None:header+='new file mode 100644\n'
 header+=f'index {blob(before) if before is not None else "0"*40}..{blob(after)}'+(' 100644' if before is not None else '')+'\n'
 return header.encode()+''.join(difflib.unified_diff((before or b'').decode().splitlines(True),after.decode().splitlines(True),fromfile=f'a/{path}' if before is not None else '/dev/null',tofile=f'b/{path}')).encode()
old_inputs=json.loads((old/'SOURCE_INPUTS.json').read_text());old_rb=json.loads((old/'READBACK.json').read_text());preserved=[]
for r in old_rb['records']+old_rb['cleanup_only_recovery']:
 check(r);preserved.append(r)
for entry in old_rb['original_failed_receipts']:
 for r in entry.values():
  if isinstance(r,dict) and {'path','bytes','sha256'} <= r.keys():check(r);preserved.append(r)
for entry in old_inputs['changed_paths']:
 for key in ['before','after']:
  if isinstance(entry.get(key),dict):check(entry[key]);preserved.append(entry[key])
for entry in old_inputs['context']:check(entry['snapshot']);preserved.append(entry['snapshot'])
for entry in json.loads((old/'ARTIFACTS.json').read_text()):
 check(entry['retained']);preserved.append(entry['retained'])
assert digest((old/'SOURCE.patch').read_bytes())=='2376c4f9405d551cb5ffd4edf0314ef8e2dcefde1d2c327c22d34da831a1e804'
base=git('rev-parse','HEAD').decode().strip();assert base=='f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f'
assert git('branch','--show-current').decode().strip()=='codex/kvm-setitimer-20260918'
assert git('diff','--cached','--quiet')==b''
changed=[];source_patch=b'';delta_patch=b'';delta_paths=[]
for entry in old_inputs['changed_paths']:
 name=entry['path'];after=(repo/name).read_bytes();previous=Path(entry['after']['path']).read_bytes();before=Path(entry['before']['path']).read_bytes() if isinstance(entry.get('before'),dict) else None
 after_path=out/'source/after'/name;after_path.parent.mkdir(parents=True,exist_ok=True);after_path.write_bytes(after)
 row=dict(path=name,working=record(repo/name),after=record(after_path),predecessor=entry['after'])
 if before is not None:
  before_path=out/'source/before'/name;before_path.parent.mkdir(parents=True,exist_ok=True);before_path.write_bytes(before);row['before']=record(before_path)
 else:row['before']=None
 changed.append(row);source_patch+=patch(name,before,after);delta_patch+=patch(name,previous,after)
 if previous!=after:delta_paths.append(name)
(out/'SOURCE.patch').write_bytes(source_patch);(out/'DELTA.patch').write_bytes(delta_patch)
subprocess.run(['/usr/bin/git','-C',str(repo),'apply','--reverse','--check',str(out/'SOURCE.patch')],check=True)
subprocess.run(['/usr/bin/git','-C',str(repo),'apply','--reverse','--check',str(out/'DELTA.patch')],check=True)
contexts=[]
for e in old_inputs['context']:
 raw=(repo/e['path']).read_bytes();assert digest(raw)==e['snapshot']['sha256'];p=out/'context'/e['path'];p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(raw);contexts.append(dict(path=e['path'],working=record(repo/e['path']),snapshot=record(p)))
(out/'source-manifest.json').write_bytes((q/'source-manifest.json').read_bytes())
sys.path.insert(0,str(q));import phase
phases=[]
for name in ['compile','clippy','format','list-lib','list-static','test-domain-lib','test-alarm-vm','test-alarm-lib','test-child-lib','test-child-vm']:
 plan=json.loads((q/(name+'-plan.json')).read_text());phase.check(plan)
 result=json.loads((q/'controls'/name/'result.json').read_text());assert all(result[k] for k in ['accepted','terminal_authenticated','inputs_unchanged']) and result['raw_status']==0
 obs=json.loads(Path(result['observer_result']['path']).read_text())
 phases.append(dict(name=name,plan=record(q/(name+'-plan.json')),result=record(q/'controls'/name/'result.json'),observer=result['observer_result'],stdout=record(Path(plan['output'])/'stdout'),stderr=record(Path(plan['output'])/'stderr'),payload_seconds=result['payload_exit']['elapsed_seconds'],aggregate_cpu_usec=obs['maximum_sampled_usage_usec'],observer_seconds=obs['elapsed_seconds'],readback=result['readback']))
artifacts={}
for name,a in json.loads((q/'controls/compile/artifacts.json').read_text()).items():
 check(a['file']);p=out/'artifacts'/name;p.parent.mkdir(exist_ok=True);shutil.copyfile(a['file']['path'],p);p.chmod(0o755);assert record(p)['sha256']==a['file']['sha256'];artifacts[name]=dict(original=a['file'],frozen=record(p),cargo_artifact=a['cargo_artifact'])
write('ARTIFACTS.json',artifacts)
failures=[]
for d in sorted(own.glob('qualification-v*')):
 for p in sorted((d/'controls').glob('*/result.json')):
  r=json.loads(p.read_text())
  if not r.get('accepted'):
   pl=json.loads((d/(p.parent.name+'-plan.json')).read_text());failures.append(dict(qualification=d.name,phase=p.parent.name,result=record(p),plan=record(d/(p.parent.name+'-plan.json')),stdout=record(Path(pl['output'])/'stdout'),stderr=record(Path(pl['output'])/'stderr'),raw_status=r['raw_status'],terminal_authenticated=r['terminal_authenticated'],inputs_unchanged=r['inputs_unchanged']))
write('QUALIFICATION.json',dict(phases=phases,artifacts=artifacts,original_failures_preserved=failures,original_predecessor_readback=record(old/'READBACK.json'),limits=json.loads((q/'compile-plan.json').read_text())['limits'],runtime_limits=json.loads((q/'test-alarm-vm-plan.json').read_text())['limits'],execution_scope='36 selected library tests; seven actual VM declarations; no integrated Hermit timer or full-workspace claim'))
write('PREDECESSOR_PRESERVATION.json',dict(readback=record(old/'READBACK.json'),authenticated=preserved,raw_index_note='Initial frozen-v1 index bytes drifted before reassignment through root git status. No restore; staged entries still identical to base. Current START.json and final readback bind the preserved successor index.'))
api=dict(schema=2,base=base,source_patch=record(out/'SOURCE.patch'),delta_patch=record(out/'DELTA.patch'),files=[record(repo/e['path'])|{'path':e['path']} for e in changed if e['path'].startswith('reverie/')],early_inventory=record(own/'API_V2.json'),change='PreparedSignalToken and ParkedSignalFailureContext carry full CallbackSignalSite. SignalDequeue and Tool/Guest method declarations unchanged from frozen-v1. Internal FIFO owner and capacity preflight are private KVM implementation.',hermit_contract='Process-global contiguous sequence; sender owns each removal; no notification across a failed predecessor; no speculative sequence buffering.')
write('API_INVENTORY.json',api)
support=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored')
reviews=[record(support/'kvm-timer-reverie-native-review-v1-20260918/REPORT.md'),record(support/'kvm-timer-reverie-claude-review-v1-20260918/CLAUDE-REPORT.md')]
index=Path(git('rev-parse','--path-format=absolute','--git-path','index').decode().strip());scm=dict(head=base,branch=git('branch','--show-current').decode().strip(),index_raw=record(index),index_entries_sha256=digest(git('ls-files','--stage')),cached_diff_empty=True)
start=json.loads((own/'START.json').read_text());assert scm['index_raw']['sha256']==start['index_raw_sha256'];assert scm['index_entries_sha256']==start['index_entries_sha256']
write('SOURCE_INPUTS.json',dict(schema=2,base=base,base_tree=old_inputs['base_tree'],source_patch=record(out/'SOURCE.patch'),delta_patch=record(out/'DELTA.patch'),predecessor=record(old/'SOURCE_INPUTS.json'),changed_paths=changed,delta_paths=delta_paths,context=contexts,complete_source_manifest=record(out/'source-manifest.json'),complete_source_records=len(json.loads((out/'source-manifest.json').read_text())),scm=scm,design_inputs=old_inputs['design_inputs'],design_reviews=old_inputs['design_reviews'],source_reviews=reviews,qualification=record(out/'QUALIFICATION.json'),start=record(own/'START.json')))
print(json.dumps({'out':str(out),'patch':record(out/'SOURCE.patch'),'delta':record(out/'DELTA.patch'),'changed_paths':len(changed),'delta_paths':delta_paths,'artifacts':{n:a['frozen'] for n,a in artifacts.items()},'api':record(out/'API_INVENTORY.json')},indent=2))
