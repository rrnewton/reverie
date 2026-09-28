from pathlib import Path
import hashlib,json,os,re,stat,sys
F=Path(__file__).resolve().parent
HEAD='a9449f8ee22a17ba87a106c2af957a938fa57c45'
def require(v,msg):
 if not v:raise RuntimeError(msg)
def read(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p); b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))
def check(r):
 got=rec(r['path']);require(all(got[k]==r[k] for k in ('path','bytes','sha256','mode') if k in r),'binding changed '+r['path']);return got
def write(p,v):
 with Path(p).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
def held(p):
 i=read(p/'inputs.json');b=read(p/'source-before.json');a=read(p/'source-after.json')
 require(b==a==i['files'],'before/after input identity')
 for r in i['files']:check(r)
 for r in i['symlinks']:
  q=Path(r['path']);require(q.is_symlink() and os.readlink(q)==r['target'],'alias identity '+str(q))
 return {'files':len(b),'aliases':len(i['symlinks']),'all_current_bytes_modes_aliases_equal':True}
require(sys.flags.optimize==0,'optimize must be 0')
# Native: complete frozen audit, with explicit first-complete ordering and real command failures.
p=F/'native'; h=held(p); a=read(p/'01-source.final-COVERAGE.json');r=read(p/'01-source.result.json');g=read(p/'guard-result.json');cs=read(p/'01-source.final-COMMANDS.json')
require(a['complete'] and a['covered_chunks']==a['mandatory_chunks']==238,'native incomplete')
require(r['raw_exit_code']==0 and r['bound_failure'] is None and g['inputs_unchanged'] and g['raw_runner_status']==0,'native terminal')
for c in a['contexts']:
 x=c['payload'];require(x['model']=='gpt-5.6-sol' and x['approval_policy']=='never' and x['sandbox_policy']['type']=='read-only' and x['cwd']==str(p),'native policy')
order=[]
for d in a['documents']:
 for c in d['chunks']:
  m=min((m for m in c['matches'] if m['ordinary_unique']),key=lambda m:m['output_line'])
  order.append(dict(index_position=len(order),path=c['path'],first_complete_output_line=m['output_line'],call_id=m['call_id']))
actual=sorted(order,key=lambda x:x['first_complete_output_line']);permutation=[x['index_position'] for x in actual]
write(p/'FIRST-COMPLETE-READ-ORDER.json',{'scope':'First complete literal appearance in actual reviewer outputs. Canonical prior grounding is authenticated separately.','index_order':list(range(len(order))),'actual_permutation':permutation,'exact_index_order':permutation==list(range(len(order))),'reads':actual})
fail=[{k:c.get(k) for k in ('id','command','exit_code','aggregated_output')} for c in cs if c.get('exit_code') not in (0,None)]
text=(p/'01-source.final.txt').read_text();require(HEAD in text and 'APPROVE SOURCE' in text,'native report target/verdict')
write(p/'FINAL-CUSTODY.json',{'scope':'Actual native whole-source review at exact head; evidence custody, not an author approval.','head':HEAD,'session':r['session_resume'],'exec_handle':60083,'handle_collected':True,'raw_exit_code':0,'elapsed_seconds':r['elapsed_seconds'],'actual_policy':{'model':'gpt-5.6-sol','approval':'never','sandbox':'read-only','contexts':len(a['contexts'])},'source_custody':h,'verdict_in_actual_report':'APPROVE SOURCE','coverage':{'complete':True,'documents':len(a['documents']),'chunks':238,'exact_stage_order_observed':a['exact_stage_order_observed']},'actual_commands':len(cs),'actual_nonzero_commands':fail,'diagnostic_candidate_count':len(a['failed_or_diagnostic_commands']),'diagnostic_precision':'Diagnostic candidate strings include source text. Only the explicit nonzero command records are command failures.','truncated_output_candidates':a['truncated_output_candidates'],'order_precision':'All required literal chunks are complete. The first-complete order differs from the source index; no initial strict-order claim. All chunks after index 154 are first-complete in index order. Separate original canonical paper/scheduler/vision grounding and its historical policy limitation remain preserved.','records':[rec(p/n) for n in ['01-source.final.txt','01-source.result.json','01-source.jsonl','01-source.stderr','guard-result.json','INITIAL-POLICY-READBACK.json','inputs.json','source-before.json','source-after.json','01-source.final-COVERAGE.json','01-source.final-MODEL-TOOLS.json','01-source.final-COMMANDS.json','FIRST-COMPLETE-READ-ORDER.json','audit-source.status.json','audit-source.stdout','audit-source.stderr']]+[rec(__file__)]})
# Claude diagnostic traverses the complete index despite frozen audit failure. It does not replace the refusal.
p=F/'claude';h=held(p);r=read(p/'exit.json');require(r['exit_code']==0 and r['inputs_unchanged'],'claude terminal')
es=[json.loads(s) for s in (p/'output.jsonl').read_text().splitlines() if s.strip()];calls={};results={};order={};errors=[]
for ei,e in enumerate(es):
 for b in e.get('message',{}).get('content',[]):
  if not isinstance(b,dict):continue
  if b.get('type')=='tool_use':require(b['id'] not in calls,'duplicate call');calls[b['id']]=b;order[b['id']]=ei
  elif b.get('type')=='tool_result':require(b['tool_use_id'] not in results,'duplicate result');results[b['tool_use_id']]=b
require(set(calls)<=set(results),'unterminated tool calls')
init=[e for e in es if e.get('type')=='system' and e.get('subtype')=='init'];term=[e for e in es if e.get('type')=='result']
require(len(init)==1 and len(term)==1 and not term[0].get('is_error'),'claude init/terminal')
require(init[0]['session_id']=='5d98403d-b8e9-42bf-8e2f-da7e7b70809c' and init[0]['model']=='claude-opus-5[1m]' and init[0]['permissionMode']=='plan','claude policy')
require(all(c['name'] in ('Read','Grep','Glob') for c in calls.values()),'claude unexpected tool')
index=read(p/'COMPLETE_READS.json');coverage=[];missing=[];reading=[]
for group in index:
 check(group['source']);combined=[];parts=[]
 for part in group['chunks']:
  check(part);expected=Path(part['path']).read_text().splitlines();combined.extend(expected);matches=[];attempts=[]
  for key,c in calls.items():
   if c['name']!='Read' or c.get('input',{}).get('file_path')!=part['path']:continue
   result=results.get(key);attempts.append(key)
   if not result or result.get('is_error'):continue
   content=result.get('content');require(isinstance(content,str),'unexpected content type')
   visible={}
   for line in content.splitlines():
    m=re.match(r'^(\d+)\t(.*)$',line)
    if m:visible[int(m.group(1))]=m.group(2)
   if all(visible.get(n)==s for n,s in enumerate(expected,1)):matches.append(key)
  row=dict(file=part,complete_read_tool_ids=matches,attempted_tool_ids=attempts)
  parts.append(row)
  if not matches:missing.append({'document':group['name'],**row})
  else:reading.append({'path':part['path'],'document':group['name'],'first_complete_call_event':min(order[k] for k in matches),'complete_read_tool_ids':matches})
 require(combined==Path(group['source']['path']).read_text().splitlines(),'chunk reconstruction')
 coverage.append({'name':group['name'],'source':group['source'],'lines':len(combined),'parts':parts,'complete':all(v['complete_read_tool_ids'] for v in parts)})
write(p/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json',{'scope':'Diagnostic only. The unchanged frozen audit failed and remains failed. No substitute acceptance or missing reading credit.','mandatory_chunks':sum(len(x['parts']) for x in coverage),'covered_chunks':sum(bool(y['complete_read_tool_ids']) for x in coverage for y in x['parts']),'missing_chunks':missing,'documents':coverage})
write(p/'FIRST-COMPLETE-READ-ORDER.json',{'scope':'Actual complete Read results, only; no credit for missing chunks and no source-index-order assertion. Prior canonical grounding is separate.','reads':sorted(reading,key=lambda v:v['first_complete_call_event'])})
indexed={x['path'] for x in read(p/'inputs.json')['files']}
errors=[dict(tool_id=k,call=c,result=results[k]) for k,c in calls.items() if results[k].get('is_error')]
unindexed=[dict(tool_id=k,call=c) for k,c in calls.items() if c['name']=='Read' and c.get('input',{}).get('file_path') not in indexed]
report=read(p/'ASSISTANT-TEXT-EXTRACTION.json');require(len(report)==1 and report[0]['event']==562 and HEAD in report[0]['text'],'report extraction')
require(term[0].get('result')==report[0]['text'],'terminal text differs from extracted report')
write(p/'INCOMPLETE-CUSTODY.json',{'scope':'Actual Claude whole-source report with mandatory raw-evidence coverage failure. Not a completed required-read approval.','head':HEAD,'exec_handle':17538,'handle_collected':True,'session':r['session_id'],'raw_exit_code':0,'elapsed_seconds':r['elapsed_seconds'],'source_custody':h,'actual_init':init[0],'actual_tool_names':sorted({c['name'] for c in calls.values()}),'actual_tool_calls':len(calls),'tool_errors':errors,'unindexed_reads':unindexed,'report_event':{k:report[0][k] for k in ('event','block','uuid','timestamp')},'terminal_result_exactly_equals_report':True,'verdict_in_actual_report':'APPROVE, explicitly limited by 8/24 raw evidence chunks; audit refuses complete coverage','failed_original_audit':read(p/'audit-final.status.json'),'mandatory_documents':len(coverage),'complete_documents':sum(x['complete'] for x in coverage),'mandatory_chunks':sum(len(x['parts']) for x in coverage),'covered_chunks':sum(bool(y['complete_read_tool_ids']) for x in coverage for y in x['parts']),'missing_chunk_count':len(missing),'records':[rec(p/n) for n in ['assistant-event-562-block-0.txt','ASSISTANT-TEXT-EXTRACTION.json','TERMINAL-RESULT-RAW.json','INITIAL-POLICY-READBACK.json','exit.json','inputs.json','source-before.json','source-after.json','output.jsonl','stderr.log','audit-final.py','audit-final.status.json','audit-final.stdout','audit-final.stderr','INCOMPLETE-COVERAGE-DIAGNOSTIC.json','FIRST-COMPLETE-READ-ORDER.json']]+[rec(__file__)]})
for path in [F/'native/FINAL-CUSTODY.json',F/'claude/INCOMPLETE-CUSTODY.json',F/'claude/INCOMPLETE-COVERAGE-DIAGNOSTIC.json']:print(json.dumps(rec(path)))
print(json.dumps({'claude_complete_documents':sum(x['complete'] for x in coverage),'claude_covered_chunks':sum(bool(y['complete_read_tool_ids']) for x in coverage for y in x['parts']),'missing':[x['file']['path'] for x in missing],'errors':len(errors),'unindexed':len(unindexed)}))
