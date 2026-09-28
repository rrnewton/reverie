import datetime, hashlib, json, pathlib, re, sys
if sys.flags.optimize !=0: raise RuntimeError('optimization enabled')
r=pathlib.Path(__file__).resolve().parent

def rec(p):
    p=pathlib.Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':p.stat().st_mode&0o777}

def save(name,obj):
    with (r/name).open('x') as f:json.dump(obj,f,indent=2);f.write('\n')

audit=json.loads((r/'ROOT-TRANSCRIPT-READBACK.json').read_text())
events=[json.loads(s) for s in (r/'output.jsonl').read_text().splitlines() if s.strip()]
calls={};result_events={};results={}
for ei,e in enumerate(events):
    for b in e.get('message',{}).get('content',[]):
        if b.get('type')=='tool_use':calls[b['id']]={'event':ei,'block':b}
        elif b.get('type')=='tool_result':result_events[b['tool_use_id']]=ei;results[b['tool_use_id']]=b
ordered=[]
for g in audit['coverage']:
    for p in g['parts']:
        first=min(p['complete_read_tool_ids'],key=lambda x:result_events[x])
        ordered.append({'index_position':len(ordered)+1,'path':p['file']['path'],'first_complete_result_event':result_events[first],'tool_id':first})
actual=sorted(ordered,key=lambda x:x['first_complete_result_event']);permutation=[x['index_position'] for x in actual]
deviations=[{'actual_position':i,'index_position':p} for i,p in enumerate(permutation,1) if i!=p]
chunk_paths={x['path'] for x in ordered};extra=[]
for tool_id,call in calls.items():
    b=call['block'];path=b.get('input',{}).get('file_path')
    if path in chunk_paths:continue
    result=results[tool_id];content=result.get('content','');visible={}
    for line in content.splitlines():
        m=re.match(r'^(\d+)\t(.*)$',line)
        if m:visible[int(m.group(1))]=m.group(2)
    expected=pathlib.Path(path).read_text().splitlines()
    extra.append({'path':path,'tool_id':tool_id,'result_event':result_events[tool_id],'expected_lines':len(expected),'literal_complete':all(visible.get(i)==line for i,line in enumerate(expected,1))})
save('READ-ORDER.json',{'scope':'This continuation only; canonical prior grounding is separate.','required_chunks':len(ordered),'first_complete_order_matches_index':not deviations,'permutation':permutation,'deviations':deviations,'chunks':ordered,'extra_reads':extra})
status=json.loads((r/'exit.json').read_text())
if status['exit_code']!=0 or not status['inputs_unchanged'] or audit['tool_errors'] or audit['unindexed_reads']:raise RuntimeError('custody failure')
if len(ordered)!=164 or audit['mandatory_chunks']!=164:raise RuntimeError('coverage population changed')
compactions=[{'event':i,'type':e.get('type'),'subtype':e.get('subtype'),'compact_metadata':e.get('compact_metadata')} for i,e in enumerate(events) if e.get('subtype')=='compact_boundary']
report=(r/'CLAUDE-REPORT.md').read_text();terminal=json.loads((r/'TERMINAL-RESULT.json').read_text())
refs=['CLAUDE-REPORT.md','REPORT-EXTRACTION.json','ROOT-TRANSCRIPT-READBACK.json','exit.json','output.jsonl','stderr.log','source-before.json','source-after.json','inputs.json','capacity-before.json','EARLY-EFFECTIVE-POLICY.json','ASSISTANT-TEXT-EVENTS.json','ASSISTANT-TEXT-EVENTS.md','TERMINAL-RESULT.json','audit-final.py','audit-final.stdout','audit-final.stderr','audit-final.status.json','READ-ORDER.json','TARGET.json','COMPLETE_READS.json','READ-INDEX.txt','OWN-PRIOR-CUSTODY.json','GROUNDING_AUTHENTICATION.json','GROUNDING_INPUTS.json','prompt.txt','freeze_readback.py']
final={'at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'scope':'Independent Claude whole-PR source verdict and orchestration custody. No publication, merge, consumer runtime or parity approval.','head':'3d4a401ed8959befad0b2f10db59093725f27432','tree':'73024912837cbbeaa70b6d2173f5b596eebedcb3','base':'6e3915b70a71657a08f0028b3f70d6074116206a','session':'5d98403d-b8e9-42bf-8e2f-da7e7b70809c','actual_model':audit['actual_init']['model'],'actual_permission_mode':audit['actual_init']['permissionMode'],'model_override':None,'verdict':'APPROVE','report_preserved_unmodified':True,'report_event':450,'report_matches_terminal_result':terminal.get('result')==report,'raw_exit_code':0,'elapsed_seconds':status['elapsed_seconds'],'exec_handle':99793,'handle_collected':True,'input_files':audit['inputs'],'input_aliases':audit['symlinks'],'inputs_unchanged':audit['bytes_modes_symlinks_unchanged'],'mandatory_documents':audit['mandatory_documents'],'literal_complete_chunks':len(ordered),'mandatory_lines':audit['mandatory_lines'],'first_complete_index_order':not deviations,'actual_tool_calls':audit['tool_calls'],'actual_tool_names':audit['actual_tool_names'],'tool_errors':0,'unindexed_reads':0,'automatic_compactions':compactions,'nonblocking_findings':['C1 host-assigned capture dev/ino and separate consumer determinization boundary','C2 stale capture test name','C3 extra MSR verification cost on timestamp GP when both consumers subscribe','C4 preexisting compiler diagnostics persistence ordering','C5 carried stack-segment acceptance looseness','C6 existing default host CPUID handler becomes reachable on KVM'],'limitations':['All 164 mandatory chunks have complete literal model-visible reads, with actual first-complete order deviations retained and disclosed by the reviewer. No prior canonical grounding order is inferred from this continuation.','Actual current tools were Read only (166 calls); the report says Read and Glob as an allowed subset. No peer report or findings were supplied.','Reviewer explicitly did not read determinize_stat body, lifecycle_signal_mask_preflight body or the tool-process async epilogue. Its approval is finite changed-source scope, not a consumer/runtime/parity claim.','The actual automatic compaction and all raw events are preserved. Content coverage is distinct from comprehension.','The original raw-137 metadata refusal and prior evidence limitations remain unchanged.'],'references':[rec(r/n) for n in refs]}
save('FINAL-READBACK.json',final)
print(json.dumps(rec(r/'FINAL-READBACK.json')))
print(json.dumps({'extra_reads':extra,'first_complete_order_deviation_positions':len(deviations),'terminal_result_matches':final['report_matches_terminal_result']}))
