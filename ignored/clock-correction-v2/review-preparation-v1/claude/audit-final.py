from pathlib import Path
import hashlib,json,os,re,stat,sys
P=Path(__file__).resolve().parent

def rec(path):
 p=Path(path);h=hashlib.sha256()
 with p.open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return {'path':str(p),'bytes':p.stat().st_size,'sha256':h.hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
def check(row):
 got=rec(row['path']);assert all(got[k]==row[k] for k in row if k in got),row['path'];return got
def write(name,data):
 with (P/name).open('x') as f:json.dump(data,f,indent=2);f.write('\n')

status=json.loads((P/'exit.json').read_text())
assert status['exit_code']==0 and status['inputs_unchanged'],status
index=json.loads((P/'inputs.json').read_text())
before=json.loads((P/'source-before.json').read_text());after=json.loads((P/'source-after.json').read_text())
assert before==after==index['files']
for row in index['files']:check(row)
for row in index['symlinks']:
 p=Path(row['path']);assert p.is_symlink() and os.readlink(p)==row['target'],row
assert len(index['symlinks'])==json.loads((P/'TARGET.json').read_text())['expected_symlinks']
indexed={r['path'] for r in index['files']}
events=[json.loads(s) for s in (P/'output.jsonl').read_text().splitlines() if s.strip()]
calls={};results={};errors=[];unindexed=[];texts=[]
for ei,event in enumerate(events):
 for bi,block in enumerate(event.get('message',{}).get('content',[])):
  if not isinstance(block,dict):continue
  if block.get('type')=='tool_use':
   assert block['id'] not in calls,block['id'];calls[block['id']]=block
   if block['name']=='Read' and block.get('input',{}).get('file_path') not in indexed:unindexed.append(block)
  elif block.get('type')=='tool_result':
   assert block['tool_use_id'] not in results,block['tool_use_id'];results[block['tool_use_id']]=block
   if block.get('is_error'):errors.append(block)
  elif block.get('type')=='text' and event.get('type')=='assistant':
   texts.append({'event':ei,'block':bi,'uuid':event.get('uuid'),'timestamp':event.get('timestamp'),'text':block['text']})
coverage=[]
for group in json.loads((P/'COMPLETE_READS.json').read_text()):
 check(group['source']);combined=[];parts=[]
 for part in group['chunks']:
  check(part);expected=Path(part['path']).read_text().splitlines();combined.extend(expected);matches=[]
  for key,call in calls.items():
   if call['name']!='Read' or call.get('input',{}).get('file_path')!=part['path']:continue
   result=results.get(key)
   if not result or result.get('is_error'):continue
   content=result.get('content');assert isinstance(content,str)
   visible={}
   for line in content.splitlines():
    m=re.match(r'^(\d+)\t(.*)$',line)
    if m:visible[int(m.group(1))]=m.group(2)
   if all(visible.get(n)==line for n,line in enumerate(expected,1)):matches.append(key)
  assert matches,('mandatory content not completely visible',part['path'])
  parts.append({'file':part,'complete_read_tool_ids':matches})
 assert combined==Path(group['source']['path']).read_text().splitlines()
 coverage.append({'name':group['name'],'lines':len(combined),'parts':parts})
target=json.loads((P/'TARGET.json').read_text())
assert len(coverage)==target['mandatory_documents'] and sum(len(g['parts']) for g in coverage)==target['mandatory_chunks'] and sum(g['lines'] for g in coverage)==target['mandatory_lines']
terminal=[e for e in events if e.get('type')=='result'];assert len(terminal)==1 and not terminal[0].get('is_error')
assert all(k in results for k in calls),set(calls)-set(results)
# Preserve all actual assistant prose with event provenance; do not assume terminal text is the entire report.
write('ASSISTANT-TEXT-EVENTS.json',texts)
with (P/'ASSISTANT-TEXT-EVENTS.md').open('x') as f:
 for t in texts:f.write(f"Event {t['event']}, block {t['block']}, UUID {t['uuid']}\n\n{t['text']}\n\n")
write('TERMINAL-RESULT.json',terminal[0])
init=[e for e in events if e.get('type')=='system' and e.get('subtype')=='init'];assert len(init)==1
names=sorted({b['name'] for b in calls.values()})
assert init[0]['session_id']==target['session_id'] and init[0]['permissionMode']=='plan' and init[0]['model']==target['expected_actual_model']
assert set(names)<=set(['Read','Grep','Glob']), names
audit={'scope':'Actual independent initial-command clock and syscall-injection source review. No whole-backend parity or signal-design approval inferred.','inputs':len(before),'symlinks':len(index['symlinks']),'bytes_modes_symlinks_unchanged':True,'session_id':status['session_id'],'status':status,'actual_init':init[0],'requested_tools_in_target':'Requested builtins only; actual init also advertises MCP tools. Availability is distinct from actual use.','actual_tool_names':names,'tool_calls':len(calls),'tool_errors':errors,'unindexed_reads':unindexed,'mandatory_documents':target['mandatory_documents'],'mandatory_chunks':target['mandatory_chunks'],'mandatory_lines':target['mandatory_lines'],'coverage':coverage,'assistant_text_events':[{'event':t['event'],'block':t['block'],'uuid':t['uuid'],'bytes':len(t['text'].encode())} for t in texts],'report_extraction':'All assistant text events and terminal result preserved separately; substantive report extraction requires explicit event selection after reading.','records':[rec(P/n) for n in ['TARGET.json','prompt.txt','launch-review.py','capacity-before.json','inputs.json','source-before.json','source-after.json','output.jsonl','stderr.log','exit.json','audit-final.py','ASSISTANT-TEXT-EVENTS.json','ASSISTANT-TEXT-EVENTS.md','TERMINAL-RESULT.json']]}
write('ROOT-TRANSCRIPT-READBACK.json',audit)
print(json.dumps({'audit':rec(P/'ROOT-TRANSCRIPT-READBACK.json'),'tools':names,'calls':len(calls),'errors':len(errors),'unindexed_reads':len(unindexed),'assistant_text_events':audit['assistant_text_events'],'elapsed_seconds':status['elapsed_seconds']},indent=2))
