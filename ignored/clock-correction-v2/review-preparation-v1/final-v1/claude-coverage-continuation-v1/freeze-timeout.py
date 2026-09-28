from pathlib import Path
import json,hashlib,stat,os,re,sys
P=Path(__file__).resolve().parent;O=P.parent/'claude'
def need(v,m):
 if not v:raise RuntimeError(m)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))
def check(r):
 got=rec(r['path']);need(all(got[k]==r[k] for k in got if k in r),'binding '+r['path'])
def write(n,v):
 with (P/n).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
need(sys.flags.optimize==0,'optimize')
status=load(P/'exit.json');need(status['exit_code']==124 and status['inputs_unchanged'],'expected timeout custody')
need(load(P/'audit-final.status.json')['raw_exit_code']==1,'retain audit refusal')
index=load(P/'inputs.json');before=load(P/'source-before.json');after=load(P/'source-after.json');need(before==after==index['files'],'before/after exact')
for r in index['files']:check(r)
for r in index['symlinks']:need(Path(r['path']).is_symlink() and os.readlink(r['path'])==r['target'],'alias '+r['path'])
events=[json.loads(s) for s in (P/'output.jsonl').read_text().splitlines() if s.strip()];calls={};results={};call_order={};texts=[];states=[]
for ei,e in enumerate(events):
 if e.get('type')=='system' and e.get('subtype')=='status':states.append({'event':ei,**e})
 for bi,b in enumerate(e.get('message',{}).get('content',[])):
  if not isinstance(b,dict):continue
  if b.get('type')=='tool_use':need(b['id'] not in calls,'duplicate call');calls[b['id']]=b;call_order[b['id']]=ei
  elif b.get('type')=='tool_result':need(b['tool_use_id'] not in results,'duplicate result');results[b['tool_use_id']]=b
  elif b.get('type')=='text' and e.get('type')=='assistant':texts.append({'event':ei,'block':bi,'uuid':e.get('uuid'),'text':b['text']})
init=[e for e in events if e.get('type')=='system' and e.get('subtype')=='init'];terminal=[e for e in events if e.get('type')=='result'];need(len(init)==1,'one init');need(not terminal,'no terminal report expected')
need(init[0]['model']=='claude-opus-5[1m]' and init[0]['session_id']=='5d98403d-b8e9-42bf-8e2f-da7e7b70809c' and init[0]['permissionMode']=='plan','actual policy')
need(set(calls)<=set(results),'all issued tools completed');need(all(b['name'] in ('Read','Grep','Glob') for b in calls.values()),'read only tools')
coverage=[]
for g in load(P/'COMPLETE_READS.json'):
 check(g['source']);parts=[];joined=[]
 for part in g['chunks']:
  check(part);expected=Path(part['path']).read_text().splitlines();joined.extend(expected);matches=[]
  for k,c in calls.items():
   if c['name']!='Read' or c.get('input',{}).get('file_path')!=part['path'] or k not in results:continue
   r=results[k]
   if r.get('is_error'):continue
   need(isinstance(r.get('content'),str),'text result')
   visible={}
   for line in r['content'].splitlines():
    m=re.match(r'^(\d+)\t(.*)$',line)
    if m:visible[int(m.group(1))]=m.group(2)
   if all(visible.get(i)==s for i,s in enumerate(expected,1)):matches.append(k)
  need(matches,'supplemental missing literal chunk '+part['path']);parts.append({'file':part,'complete_read_tool_ids':matches,'first_complete_call_event':min(call_order[k] for k in matches)})
 need(joined==Path(g['source']['path']).read_text().splitlines(),'chunk reconstruction');coverage.append({'name':g['name'],'source':g['source'],'parts':parts})
need(len(coverage)==2 and sum(len(g['parts']) for g in coverage)==17,'supplement dimensions')
old=load(O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json');old_parts={c['file']['path']:c for g in old['documents'] for c in g['parts']};new_parts={c['file']['path']:c for c in coverage[0]['parts']};missing={p:c for p,c in old_parts.items() if not c['complete_read_tool_ids']}
need(len(old_parts)==240 and len(missing)==16 and set(missing)==set(new_parts),'exact original gap')
for p,c in missing.items():need(c['file']==new_parts[p]['file'],'original chunk binding')
write('TIMEOUT-COVERAGE.json',{'scope':'Literal reading evidence only; timeout and failed audit remain failures; no supplemental verdict.','original_required':240,'original_visible':224,'supplement_raw_visible':16,'supplement_rule_visible':1,'union_original_visible':240,'raw_gap_exactly_filled':True,'coverage':coverage})
write('TIMEOUT-ASSISTANT-TEXTS.json',texts);write('COMPACTION-EVENTS.json',states)
indexed={r['path'] for r in index['files']};unindexed=[c for c in calls.values() if c['name']=='Read' and c.get('input',{}).get('file_path') not in indexed];errors=[r for r in results.values() if r.get('is_error')]
write('TIMEOUT-READBACK.json',{'scope':'Failed bounded supplemental review; complete missing-byte reading but no new verdict. Original report/audit untouched.','python_optimize':0,'head':status['head'],'exec_handle':13464,'handle_collected':True,'raw_exit_code':124,'elapsed_seconds':status['elapsed_seconds'],'inputs_unchanged':True,'verified_files':len(before),'verified_aliases':len(index['symlinks']),'actual_policy':{'model':init[0]['model'],'session':init[0]['session_id'],'permission':init[0]['permissionMode']},'actual_tool_names':sorted({b['name'] for b in calls.values()}),'actual_calls':len(calls),'tool_errors':errors,'unindexed_reads':unindexed,'system_status_events':states,'terminal_result_count':0,'supplemental_report_present':False,'approval_completed':False,'literal_read_union':{'original_visible':224,'new_raw_visible':16,'original_required':240,'new_rule_visible':1},'first_refusal_preserved':True,'records':[rec(P/n) for n in ['SETUP.json','TARGET.json','prompt.txt','launch-review.py','capacity-before.json','launch.json','INITIAL-POLICY-READBACK.json','inputs.json','source-before.json','source-after.json','output.jsonl','stderr.log','exit.json','audit-final.py','audit-final.status.json','audit-final.stdout','audit-final.stderr','TIMEOUT-COVERAGE.json','TIMEOUT-ASSISTANT-TEXTS.json','COMPACTION-EVENTS.json','freeze-timeout.py']]+[rec(O/n) for n in ['assistant-event-562-block-0.txt','audit-final.status.json','audit-final.stderr','INCOMPLETE-COVERAGE-DIAGNOSTIC.json']]})
print(json.dumps(rec(P/'TIMEOUT-READBACK.json')));print(json.dumps(rec(P/'TIMEOUT-COVERAGE.json')));print(json.dumps({'calls':len(calls),'errors':len(errors),'unindexed':len(unindexed),'status_events':len(states),'assistant_text_events':len(texts),'union_chunks':240}))
