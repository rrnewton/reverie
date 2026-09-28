from pathlib import Path
import json,hashlib,stat,sys
F=Path(__file__).resolve().parent;O=F/'claude';C=F/'claude-coverage-continuation-v1';P=F/'claude-report-continuation-v1'
def need(v,m):
 if not v:raise RuntimeError(m)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))
def write(p,v):
 with Path(p).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
need(sys.flags.optimize==0,'optimization')
a=load(P/'ROOT-TRANSCRIPT-READBACK.json');need(a['status']['exit_code']==0 and a['status']['inputs_unchanged'],'report invocation custody')
need(a['mandatory_chunks']==0 and a['mandatory_documents']==0,'report-only shape')
old=load(O/'INCOMPLETE-COVERAGE-DIAGNOSTIC.json');tc=load(C/'TIMEOUT-COVERAGE.json');tr=load(C/'TIMEOUT-READBACK.json')
need(load(O/'audit-final.status.json')['raw_exit_code']==1,'original audit refusal retained')
need(load(C/'exit.json')['exit_code']==124 and load(C/'audit-final.status.json')['raw_exit_code']==1,'timeout/audit refusal retained')
need(tr['literal_read_union']=={'original_visible':224,'new_raw_visible':16,'original_required':240,'new_rule_visible':1} and not tr['approval_completed'],'timeout not a verdict')
original={v['file']['path']:v for g in old['documents'] for v in g['parts']};new={v['file']['path']:v for v in tc['coverage'][0]['parts']}
missing={p:v for p,v in original.items() if not v['complete_read_tool_ids']}
need(len(original)==240 and len(missing)==16 and set(new)==set(missing),'exact read union')
for p,v in missing.items():
 need(v['file']==new[p]['file'] and new[p]['complete_read_tool_ids'],'supplement exact record')
 got=rec(p);need(all(got[k]==v['file'][k] for k in got),'current raw bytes changed')
texts=load(P/'ASSISTANT-TEXT-EVENTS.json');terminal=load(P/'TERMINAL-RESULT.json');matches=[t for t in texts if t['text']==terminal.get('result')];need(len(matches)==1,'explicit report extraction')
t=matches[0];need('a9449f8ee22a17ba87a106c2af957a938fa57c45' in t['text'],'report exact head')
with (P/'SUPPLEMENTAL-REPORT.md').open('xb') as f:f.write(t['text'].encode())
status=load(P/'exit.json');rows=[O/n for n in ['assistant-event-562-block-0.txt','INCOMPLETE-CUSTODY.json','INCOMPLETE-COVERAGE-DIAGNOSTIC.json','audit-final.status.json','audit-final.stderr']]+[C/n for n in ['TIMEOUT-READBACK.json','TIMEOUT-COVERAGE.json','TIMEOUT-ASSISTANT-TEXTS.json','COMPACTION-EVENTS.json','exit.json','audit-final.status.json','audit-final.stderr']]+[P/n for n in ['SETUP.json','TARGET.json','prompt.txt','launch-review.py','capacity-before.json','launch.json','INITIAL-POLICY-READBACK.json','exit.json','inputs.json','source-before.json','source-after.json','output.jsonl','stderr.log','audit-final.py','audit-final.status.json','audit-final.stdout','audit-final.stderr','ROOT-TRANSCRIPT-READBACK.json','ASSISTANT-TEXT-EVENTS.json','ASSISTANT-TEXT-EVENTS.md','TERMINAL-RESULT.json','SUPPLEMENTAL-REPORT.md']]+[Path(__file__)]
result={'scope':'Actual same-Claude report-only terminal custody joined to its own complete prior reading. The substantive source verdict is the exact supplemental report, not inferred by this script.','head':'a9449f8ee22a17ba87a106c2af957a938fa57c45','base':'b5e2ab49cd99e5d456fa0238b8cebd75958c529f','tree':'da3e0a5c13bbd1b95adca1bc3ac0037b89494544','python_optimize':0,'session':status['session_id'],'exec_handle':94954,'handle_collected':True,'raw_exit_code':status['exit_code'],'elapsed_seconds':status['elapsed_seconds'],'files_verified':a['inputs'],'aliases_verified':a['symlinks'],'inputs_unchanged':a['bytes_modes_symlinks_unchanged'],'actual_policy':{'model':a['actual_init']['model'],'session':a['actual_init']['session_id'],'permission':a['actual_init']['permissionMode']},'actual_tool_names':a['actual_tool_names'],'actual_tool_calls':a['tool_calls'],'tool_errors':a['tool_errors'],'unindexed_reads':a['unindexed_reads'],'reading':{'original_required_chunks':240,'original_visible':224,'original_audit_failed':True,'supplement_raw_visible':16,'supplement_rule_visible':1,'supplement_raw_status':124,'supplement_audit_failed':True,'combined_original_required_chunks_complete':240,'combined_documents_complete':60,'report_only_new_required_chunks':0,'precision':'Distinct original 224 plus supplemental 16; no retroactive original success/order claim. Original canonical grounding remains separately authenticated. Seven compacting statuses and timeout retained. No native material supplied.'},'report_extraction':{k:t[k] for k in ('event','block','uuid','timestamp')},'terminal_exactly_matches_extracted_report':True,'records':[rec(q) for q in rows]}
write(P/'FINAL-CUSTODY.json',result);print(json.dumps(rec(P/'FINAL-CUSTODY.json')));print(json.dumps(rec(P/'SUPPLEMENTAL-REPORT.md')))
