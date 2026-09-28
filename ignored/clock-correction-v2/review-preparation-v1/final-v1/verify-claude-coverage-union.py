from pathlib import Path
import json,hashlib,stat,sys
F=Path(__file__).resolve().parent;O=F/'claude';P=F/'claude-coverage-continuation-v1'
def need(v,m):
 if not v:raise RuntimeError(m)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':stat.S_IMODE(p.stat().st_mode)}
def write(p,x):
 with Path(p).open('x') as f:json.dump(x,f,indent=2);f.write('\n')
need(sys.flags.optimize==0,'optimization')
a=load(P/'ROOT-TRANSCRIPT-READBACK.json');old=load(O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json');gap=load(P/'MISSING-CHUNKS.json')
need(a['mandatory_chunks']==17 and a['mandatory_documents']==2,'supplement complete dimensions')
need(a['status']['exit_code']==0 and a['status']['inputs_unchanged'],'supplement terminal custody')
need(load(O/'audit-final.status.json')['raw_exit_code']==1,'preserve original refusal')
expected={r['path']:r for r in gap['missing']};original={};new={}
for g in old['documents']:
 for c in g['parts']:
  r=c['file'];need(r['path'] not in original,'duplicate original path');original[r['path']]=c
for g in a['coverage']:
 for c in g['parts']:
  r=c['file'];need(c['complete_read_tool_ids'],'missing new complete read')
  if g['name']=='missing-entry-replay-raw-events':new[r['path']]=c
old_missing={p for p,c in original.items() if not c['complete_read_tool_ids']}
need(set(expected)==set(new)==old_missing and len(new)==16,'exact original missing set')
for p in expected:
 need(original[p]['file']==expected[p]==new[p]['file'],'original/supplement record drift '+p)
 got=rec(p);need(all(got[k]==expected[p][k] for k in got),'chunk bytes drift')
need(len(original)==240 and sum(bool(c['complete_read_tool_ids']) for c in original.values())==224,'original counts')
for p,c in original.items():need(c['complete_read_tool_ids'] or p in new,'uncovered original chunk')
es=[json.loads(s) for s in (P/'output.jsonl').read_text().splitlines() if s.strip()];calls={};order={}
for ei,e in enumerate(es):
 for b in e.get('message',{}).get('content',[]):
  if isinstance(b,dict) and b.get('type')=='tool_use':calls[b['id']]=b;order[b['id']]=ei
actual=sorted(({'path':p,'first_complete_call_event':min(order[k] for k in c['complete_read_tool_ids']),'tool_ids':c['complete_read_tool_ids']} for p,c in new.items()),key=lambda r:r['first_complete_call_event'])
texts=load(P/'ASSISTANT-TEXT-EVENTS.json');terminal=load(P/'TERMINAL-RESULT.json');matches=[t for t in texts if t['text']==terminal.get('result')]
need(len(matches)==1,'explicit report extraction requires one exact terminal match');report=matches[0]
with (P/'SUPPLEMENTAL-REPORT.md').open('xb') as f:f.write(report['text'].encode())
status=load(P/'exit.json')
result={'scope':'Read coverage and custody only. Original failed audit remains failed; this distinct invocation supplies missing raw bytes. Source verdict remains the actual supplemental report, not inferred from raw zero.','head':'a9449f8ee22a17ba87a106c2af957a938fa57c45','original_required_chunks':240,'original_complete_chunks':224,'original_failed_audit_preserved':True,'original_raw_event_chunks_complete':8,'original_raw_event_chunks_required':24,'supplement_raw_chunks_complete':16,'supplement_rule_chunks_complete':1,'combined_original_required_chunks_complete':240,'combined_complete_documents':60,'new_paths_exactly_original_missing_set':True,'supplement_actual_read_order':actual,'supplement_exact_required_order':list(expected)==[r['path'] for r in actual],'grounding_scope':'Original canonical grounding and own-session source reuse remain separately authenticated; no retroactive source-index-order claim.','exec_handle':13464,'handle_collected':True,'raw_exit_code':status['exit_code'],'elapsed_seconds':status['elapsed_seconds'],'actual_policy':{'session':a['actual_init']['session_id'],'model':a['actual_init']['model'],'permission':a['actual_init']['permissionMode']},'actual_tool_names':a['actual_tool_names'],'actual_tool_calls':a['tool_calls'],'tool_errors':a['tool_errors'],'unindexed_reads':a['unindexed_reads'],'report_extraction':{k:report[k] for k in ('event','block','uuid','timestamp')},'terminal_report_exact_match':True,'records':[rec(q) for q in [O/'assistant-event-562-block-0.txt',O/'audit-final.status.json',O/'audit-final.stderr',O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json',P/'MISSING-CHUNKS.json',P/'SETUP.json',P/'prompt.txt',P/'TARGET.json',P/'exit.json',P/'output.jsonl',P/'stderr.log',P/'INITIAL-POLICY-READBACK.json',P/'ROOT-TRANSCRIPT-READBACK.json',P/'ASSISTANT-TEXT-EVENTS.json',P/'TERMINAL-RESULT.json',P/'SUPPLEMENTAL-REPORT.md',P/'audit-final.status.json',Path(__file__)]]}
write(P/'COVERAGE-UNION.json',result)
print(json.dumps(rec(P/'COVERAGE-UNION.json')));print(json.dumps(rec(P/'SUPPLEMENTAL-REPORT.md')))
