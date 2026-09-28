from pathlib import Path
import hashlib,json,os,re,subprocess
N=Path(__file__).resolve().parent; Q=N/'qualification-v1'; OUT=N/'qualification-result-v1';R=N.parents[1]
def rec(p):
 p=Path(p);st=p.lstat();b=os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
 d=dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():d.update(kind='symlink',target=os.readlink(p))
 return d
def write(name,value):
 with (OUT/name).open('x') as f:f.write(value if isinstance(value,str) else json.dumps(value,indent=2)+'\n')
assert not OUT.exists();OUT.mkdir()
order=json.loads((N/'RUN-ORDER.json').read_text())['phases'];groups=json.loads((Q/'SELECTORS.json').read_text())['groups']
phases=[];raw_tests=[];refused=False
for name in order:
 p=Q/'controls'/name/'result.json'
 if not p.exists():
  assert refused or not phases
  continue
 assert not refused,'phase occurred after first refusal'
 result=json.loads(p.read_text());plan=Q/(name+'-plan.json');planned=json.loads(plan.read_text())
 assert rec(plan)['sha256']==result['plan_sha256']
 obs=Q/'observer'/name/'result.json';observed=json.loads(obs.read_text())
 assert rec(obs)['sha256']==result['observer_result']['sha256']
 assert result['inputs_unchanged'] and result['terminal_authenticated']
 assert observed['accounting_complete'] and observed['final_accounting']['cgroup_empty']
 assert result['raw_status']==result['payload_exit']['returncode']
 phase=dict(name=name,plan=rec(plan),result=rec(p),observer=rec(obs),accepted=result['accepted'],raw_status=result['raw_status'],limits=planned['limits'],cpu_seconds=observed['final_accounting']['cpu_usage_nsec']/1e9,payload_seconds=result['payload_exit']['elapsed_seconds'],observer_transport_seconds=result['transport']['elapsed_seconds'])
 if name in groups:
  raw=[]
  for stream in ['stdout','stderr']:
   path=Q/'observer'/name/stream;text=path.read_text(errors='replace');raw.append((stream,path,text))
  matches=[dict(stream=s,line=i+1,text=line) for s,p,text in raw for i,line in enumerate(text.splitlines()) if re.search(r'\bskip(?:ped|ping)?\b|\bunexecuted\b|KVM unavailable|cannot open /dev/kvm',line,re.I)]
  events=[json.loads(line) for line in raw[0][2].splitlines() if line.startswith('{')]
  suite=[e for e in events if e.get('type')=='suite' and e.get('event') in ['ok','failed']];assert len(suite)==1
  tests=[e for e in events if e.get('type')=='test' and e.get('event') in ['ok','failed','ignored']]
  assert [t['name'] for t in tests]==groups[name]['names']
  if result['accepted']:assert suite[0]['passed']==1 and suite[0]['failed']==0 and suite[0]['ignored']==0
  audit=dict(name=name,selector=groups[name]['names'][0],artifact=groups[name]['artifact'],streams=[rec(p) for s,p,t in raw],raw_suite=suite[0],outcomes=tests,skip_or_unexecuted_messages=matches)
  raw_tests.append(audit);phase['raw_test']=audit
 else:
  phase['readback']=result.get('readback')
 phases.append(phase);refused=not result['accepted']
assert phases
scm={}
for key,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage']),('status',['status','--porcelain=v1','--untracked-files=no'])]:
 raw=subprocess.check_output(['/usr/bin/git','-C',str(R),*args],env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'})
 scm[key]=hashlib.sha256(raw).hexdigest() if key=='index' else raw.decode().strip()
expected_scm=json.loads((Q/'metadata-plan.json').read_text())['scm']
assert {k:scm[k] for k in expected_scm}==expected_scm
assert scm['status']=='', 'tracked product status changed'
skips=[a for a in raw_tests if a['skip_or_unexecuted_messages']]
all_qualified=len(phases)==len(order) and all(p['accepted'] for p in phases) and not skips
write('RAW-EXECUTION-AUDIT.json',dict(tests=raw_tests,attempted=len(raw_tests),unattempted=[n for n in groups if n not in [t['name'] for t in raw_tests]],skips_or_unexecuted=skips,claim='Raw streams inspected separately from libtest and inventory counts; no unattempted test credit'))
write('RESULTS.json',dict(status='ALL PLANNED PHASES QUALIFIED' if all_qualified else 'NOT FULLY QUALIFIED; preserve first failure/refusal or raw execution gap',phases=phases,planned_phases=len(order),attempted=len(phases),accepted=sum(p['accepted'] for p in phases),refused=sum(not p['accepted'] for p in phases),planned_declarations=len(groups),attempted_declarations=len(raw_tests),passed_declarations=sum(t['raw_suite']['passed'] for t in raw_tests),failed_declarations=sum(t['raw_suite']['failed'] for t in raw_tests),ignored_declarations=sum(t['raw_suite']['ignored'] for t in raw_tests),unattempted_phases=[p for p in order if p not in [x['name'] for x in phases]],all_qualified=all_qualified,no_skip_messages=not skips,scm_unchanged=scm,total_cpu_seconds=sum(p['cpu_seconds'] for p in phases),total_payload_seconds=sum(p['payload_seconds'] for p in phases),total_observer_transport_seconds=sum(p['observer_transport_seconds'] for p in phases),no_parity_or_source_approval=True))
records={}
def add(p):
 p=Path(p)
 if p.is_file() or p.is_symlink():records[str(p)]=rec(p)
for row in json.loads((N/'INPUTS.json').read_text())['records']:
 actual=rec(row['path']);assert actual['sha256']==row['sha256'];assert actual['mode']==row['mode'];add(row['path'])
for p in N.iterdir():
 if p.is_file():add(p)
for p in Q.iterdir():
 if p.is_file():add(p)
for directory in [Q/'controls',Q/'observer',Q/'retained-binaries',Q/'loader',N/'launch']:
 for p in directory.rglob('*'):add(p)
for p in OUT.iterdir():add(p)
write('INPUTS.json',dict(records=list(records.values()),meaning='Actual V5 source, caller, phase results, raw streams, retained emitted ELFs and preserved first failures'))
for row in records.values():assert rec(row['path'])==row
write('READBACK.json',dict(results=rec(OUT/'RESULTS.json'),raw_execution_audit=rec(OUT/'RAW-EXECUTION-AUDIT.json'),inputs=rec(OUT/'INPUTS.json'),records=len(records),all_inputs_unchanged=True,all_qualified=all_qualified,no_source_review_or_parity_verdict=True))
print(json.dumps({n:rec(OUT/n) for n in ['RESULTS.json','RAW-EXECUTION-AUDIT.json','INPUTS.json','READBACK.json']},indent=2))
