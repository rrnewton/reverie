import collections,datetime,hashlib,json,pathlib,stat,sys
P=pathlib.Path;O=P(__file__).resolve().parent;stage='01-source';tag=sys.argv[1];session=P('/home/newton/.codex/sessions/2026/09/18/rollout-2026-09-18T01-07-43-01a0b38e-9e3c-7883-9816-65199c048935.jsonl')
def req(v,m):
 if not v:raise RuntimeError(m)
def rec(p):
 b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
def wr(name,x):
 p=O/f'{stage}.{tag}-{name}.json';req(not p.exists(),'no overwrite');p.write_text(json.dumps(x,indent=2)+'\n');return rec(p)
def outtext(p):
 x=p.get('output','');return x if isinstance(x,str) else '\n'.join(a.get('text','') for a in x if isinstance(a,dict))
launch=json.loads((O/(stage+'.launch.json')).read_text());start=launch['started_at_utc'];events=[];contexts=[]
for n,l in enumerate(session.read_text().splitlines(),1):
 try:r=json.loads(l)
 except json.JSONDecodeError:continue
 if r.get('timestamp','')<start:continue
 if r.get('type')=='turn_context':contexts.append({'line':n,'timestamp':r['timestamp'],'payload':r['payload']})
 if r.get('type')=='response_item' and r.get('payload',{}).get('type') in ['custom_tool_call','custom_tool_call_output','function_call','function_call_output']:events.append({'line':n,**r})
calls=collections.Counter(x['payload']['call_id'] for x in events if x['payload']['type'] in ['custom_tool_call','function_call']);outputs=[x for x in events if x['payload']['type'].endswith('_output')];outcounts=collections.Counter(x['payload']['call_id'] for x in outputs)
docs=json.loads((O/'COMPLETE_READS.json').read_text())['documents'];covered=[];allmatches=[]
for d in docs:
 cs=[]
 for c in d['chunks']:
  text=P(c['path']).read_text();hits=[]
  for x in outputs:
   p=x['payload'];out=outtext(p)
   if text in out or text.rstrip('\n') in out:
    hits.append({'call_id':p['call_id'],'timestamp':x['timestamp'],'ordinary_unique':calls[p['call_id']]==1 and outcounts[p['call_id']]==1,'output_line':x['line']})
  good=[h for h in hits if h['ordinary_unique']];cs.append({**c,'covered':bool(good),'matches':hits})
  if good:allmatches.append((d.get('stage_order',0),c['first_line'],min(h['timestamp'] for h in good)))
 covered.append({'source':d['document'],'lines':d['lines'],'stage_order':d.get('stage_order',0),'complete':all(c['covered'] for c in cs),'chunks':cs})
order=True
for a,b in [(1,2),(2,3)]:
 aa=[t for s,_,t in allmatches if s==a];bb=[t for s,_,t in allmatches if s==b]
 if aa and bb and max(aa)>=min(bb):order=False
commands=[]
for l in (O/(stage+'.jsonl')).read_text().splitlines():
 try:r=json.loads(l)
 except json.JSONDecodeError:continue
 item=r.get('item',{})
 if r.get('type')=='item.completed' and item.get('type')=='command_execution':commands.append(item)
req(bool(contexts),'no actual resumed turn context observed')
for x in contexts:
 p=x['payload'];req(p.get('approval_policy')=='never' and p.get('sandbox_policy',{}).get('type')=='read-only' and p.get('cwd')==str(O) and p.get('model')=='gpt-5.6-sol','effective policy drift')
errors=[c for c in commands if c.get('exit_code')!=0 or any(s in c.get('aggregated_output','') for s in ['No such file or directory','Traceback (most recent call last)','error:'])];trunc=[{'call_id':x['payload']['call_id'],'line':x['line']} for x in outputs if any(m in outtext(x['payload']) for m in ['Warning: truncated output','tokens truncated','output was truncated','… output truncated'])]
proof={'at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'start':start,'complete':all(d['complete'] for d in covered),'exact_stage_order_observed':order,'documents':covered,'covered_chunks':sum(c['covered'] for d in covered for c in d['chunks']),'mandatory_chunks':sum(len(d['chunks']) for d in docs),'actual_commands':len(commands),'tool_records':len(events),'failed_or_diagnostic_commands':errors,'truncated_output_candidates':trunc,'contexts':contexts,'limit':'Literal model-visible bytes and source order, not comprehension; extra actual reads and all diagnostics require human orchestration inspection.'}
a=wr('COVERAGE',proof);b=wr('MODEL-TOOLS',events);c=wr('COMMANDS',commands)
print(json.dumps({'proof':a,'model_tools':b,'commands':c,'covered':proof['covered_chunks'],'complete':proof['complete'],'order':order,'errors':len(errors),'truncation_candidates':len(trunc)},indent=2))
