from pathlib import Path
import json,hashlib,stat,collections,re,subprocess,datetime
O=Path(__file__).parent;S=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored');H=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918')
C=S/'kvm-timer-review-grounding-v2-20260918';N=S/'kvm-timer-native-cli-review-v1-20260918'
SID={'claude':'5d98403d-b8e9-42bf-8e2f-da7e7b70809c','native':'01a0b38e-9e3c-7883-9816-65199c048935'}
def rec(p):
 p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))
def put(p,d):
 p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x') as f:f.write(json.dumps(d,indent=2)+'\n')
def check(x):
 r=rec(x['path']);assert all(r[k]==x[k] for k in ['bytes','sha256']);return r
g=json.loads((C/'GROUNDING.json').read_text());complete=json.loads((C/'COMPLETE.json').read_text());assert complete['complete'] and not complete['proposal_exposed'] and complete['session_id']==SID['claude']
ng=json.loads((S/'kvm-rdtsc-native-review-v1-20260918/GROUNDING_REUSE.json').read_text());assert ng['session']==SID['native']
hashcmd=['git','-C',str(H),'show','97c39b308efd854693038f24962484182d932f27:detcore/src/scheduler.rs'];proc=subprocess.run(hashcmd,capture_output=True,timeout=30);assert proc.returncode==0
scheduler=O/'common/hermit-97c/detcore/src/scheduler.rs';scheduler.parent.mkdir(parents=True,exist_ok=True);scheduler.write_bytes(proc.stdout)
assert rec(scheduler)['sha256']=='a5da44ec706ad656230cf78aa5742fc40c5339a14aad30410b880dc18b576fa3'
for canonical, stage in [(Path('/home/newton/work/dev-hermit/PROJECT_VISION.md'), 2), (Path('/home/newton/work/dev-hermit/ai_docs/hermit-v2-roadmap.md'), 3)]:
 assert canonical.read_bytes() == Path(g['stages'][stage]['source']['path']).read_bytes(), str(canonical)
assert __debug__
for lane in ['claude','native']:
 d=O/lane;d.mkdir();stages=[];records={};last_end=None
 def bind(p):
  r=rec(p);records[r['path']]=r;return r
 for i,stage in enumerate(['01-paper','02-scheduler','03-vision','04-roadmap']):
  src=g['stages'][i];chunks=src['chunks'];source=check(src['source']);records[source['path']]=source
  if lane=='claude':
   raw=C/stage/'output.jsonl';exitp=C/stage/'exit.json';ep=json.loads(exitp.read_text());assert ep['raw_status']==0 and ep['session_id']==SID[lane]
   start=ep['started_at'];end=ep['completed_at'];events=[json.loads(l) for l in raw.read_text().splitlines() if l.strip()];calls={};results={}
   for event in events:
    for b in event.get('message',{}).get('content',[]):
     if not isinstance(b,dict):continue
     if b.get('type')=='tool_use':calls[b['id']]=b
     if b.get('type')=='tool_result':results[b['tool_use_id']]=b
   hits=[]
   for c in chunks:
    cr=check(c);records[cr['path']]=cr;expected=Path(c['path']).read_text().splitlines();matches=[]
    for cid,call in calls.items():
     if call.get('name')!='Read' or call.get('input',{}).get('file_path')!=c['path']:continue
     rr=results.get(cid)
     if not rr or rr.get('is_error'):continue
     visible={}
     for line in rr.get('content','').splitlines():
      m=re.match(r'^(\d+)\t(.*)$',line)
      if m:visible[int(m[1])]=m[2]
     if all(visible.get(k)==line for k,line in enumerate(expected,1)):matches.append(cid)
    assert matches,('missing own Claude literal reading',c['path']);hits.append({'chunk':cr,'tool_ids':matches})
   cp=C/stage/'COVERAGE.json';cov=json.loads(cp.read_text());assert cov['qualified'];coverage=bind(cp)
  else:
   ns=ng['initial_ordered_coverage'][i];assert ns['stage']==stage and check(ns['source'])['sha256']==source['sha256']
   raw=Path(ns['archive']['path']);check(ns['archive']);exitp=N/(stage+'.result.json');ep=json.loads(exitp.read_text());assert ep['raw_exit_code']==0
   if i:assert ep['session_resume']==SID[lane]
   start=ep['started_at_utc'];end=ep['finished_at_utc'];events=[json.loads(l) for l in raw.read_text().splitlines() if l.strip()]
   calls=collections.Counter(e['payload']['call_id'] for e in events if e['payload']['type'] in ['custom_tool_call','function_call']);outs=[e for e in events if e['payload']['type'].endswith('_output')];counts=collections.Counter(e['payload']['call_id'] for e in outs);hits=[]
   for c in ns['chunks']:
    cr=check(c);records[cr['path']]=cr;text=Path(c['path']).read_text();matches=[]
    for event in outs:
     p=event['payload'];v=p.get('output','');v=v if isinstance(v,str) else '\n'.join(x.get('text','') for x in v if isinstance(x,dict));cid=p['call_id']
     if calls[cid]==1 and counts[cid]==1 and (text in v or text.rstrip('\n') in v):matches.append({'call_id':cid,'timestamp':event['timestamp']})
    assert matches,('missing own native literal reading',c['path']);hits.append({'chunk':cr,'ordinary_outputs':matches})
   coverage=check(ns['coverage']);records[coverage['path']]=coverage
  assert last_end is None or datetime.datetime.fromisoformat(last_end)<datetime.datetime.fromisoformat(start)
  last_end=end;records[rec(raw)['path']]=rec(raw)
  stages.append({'stage':stage,'source':source,'complete_lines':src['lines'],'raw_exit':bind(exitp),'raw_tool_evidence':bind(raw),'coverage':coverage,'started_at':start,'ended_at':end,'literal_matches':hits})
  if stage=='02-scheduler':assert Path(source['path']).read_bytes()==scheduler.read_bytes()
 for p in ([C/'GROUNDING.json',C/'COMPLETE.json',C/'ROOT-READBACK.json'] if lane=='claude' else [N/'SESSION_IDENTITY.json',N/'GROUNDING_READBACK.json']):bind(p)
 bind(scheduler)
 proof={'session_id':SID[lane],'scope':'Own actual reviewer ordered grounding only; no author or peer grounding/approval borrowed','all_39_chunks_reauthenticated_from_own_model_visible_reads':sum(len(x['literal_matches']) for x in stages)==39,'stages_in_order':stages,'current_hermit':'97c39b308efd854693038f24962484182d932f27','scheduler':rec(scheduler),'scheduler_git_blob':'20c3c4296223d1aa03ca28e2f45514ad0a72bff6','scheduler_equals_own_original_complete_reading':True,'raw_git_command':hashcmd,'raw_git_status':proc.returncode,'policy_limit':'Native initial paper/scheduler requested never but effectively on-request/read-only; that historical discrepancy remains preserved. Future actual context must prove never/read-only.' if lane=='native' else 'Own complete initial Claude stages were plan-mode Read-only. Recheck actual session/mode/model/tools at new launch.','prior_verdicts':'Separate prior reports remain untouched and are not evidence of approval for this clock implementation.'}
 put(d/'GROUNDING_AUTHENTICATION.json',proof);put(d/'GROUNDING_INPUTS.json',list(records.values()))
 print(lane,rec(d/'GROUNDING_AUTHENTICATION.json'))
