import datetime, hashlib, json, os, pathlib, sys
if sys.flags.optimize != 0:
    raise RuntimeError('optimization must be disabled')
r=pathlib.Path(__file__).resolve().parent

def record(path):
    p=pathlib.Path(path); b=p.read_bytes()
    return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':p.stat().st_mode&0o777}

def save(name,obj):
    with (r/name).open('x') as f: json.dump(obj,f,indent=2); f.write('\n')

coverage=json.loads((r/'01-source.final-COVERAGE.json').read_text())
commands=json.loads((r/'01-source.final-COMMANDS.json').read_text())
inputs=json.loads((r/'inputs.json').read_text())
before=json.loads((r/'source-before.json').read_text())
after=json.loads((r/'source-after.json').read_text())
if before != after or before != inputs['files']:
    raise RuntimeError('input snapshot difference')
for x in inputs['symlinks']:
    if os.readlink(x['path']) != x['target']: raise RuntimeError('alias changed')
if not coverage['complete'] or coverage['covered_chunks'] != coverage['mandatory_chunks']:
    raise RuntimeError('coverage incomplete')
ordered=[]
for document in coverage['documents']:
    for chunk in document['chunks']:
        hits=[m for m in chunk['matches'] if m.get('ordinary_unique')]
        if not hits: raise RuntimeError('missing literal ordinary result')
        first=min(hits,key=lambda m:m['output_line'])
        ordered.append({'index_position':len(ordered)+1,'path':chunk['path'],'first_complete_output_line':first['output_line'],'call_id':first['call_id']})
actual=sorted(ordered,key=lambda x:x['first_complete_output_line'])
permutation=[x['index_position'] for x in actual]
deviations=[{'actual_position':i,'index_position':p} for i,p in enumerate(permutation,1) if i!=p]
order={'scope':'This continuation only, not prior canonical grounding. Complete literal content is distinguished from first-complete-read order.','required_chunks':len(ordered),'first_complete_order_matches_index':not deviations,'permutation':permutation,'deviations':deviations,'chunks':ordered,'coverage_helper_stage_order_caveat':'All current document stage_order values are 0. exact_stage_order_observed=true in the unchanged helper does not establish current index order.'}
save('READ-ORDER.json',order)
nonzero=[{'id':x.get('id'),'command':x.get('command'),'exit_code':x.get('exit_code')} for x in commands if x.get('exit_code') not in (0,None)]
result=json.loads((r/'01-source.result.json').read_text())
guard=json.loads((r/'guard-result.json').read_text())
if result['raw_exit_code'] !=0 or result['bound_failure'] is not None or guard['raw_runner_status'] !=0 or not guard['inputs_unchanged']:
    raise RuntimeError('terminal custody failure')
refs=['01-source.final.txt','01-source.result.json','01-source.jsonl','01-source.stderr','guard-result.json','guard-launch.json','source-before.json','source-after.json','inputs.json','01-source.capacity-before.json','01-source.capacity-after.json','EARLY-EFFECTIVE-POLICY.json','01-source.final-COVERAGE.json','01-source.final-COMMANDS.json','01-source.final-MODEL-TOOLS.json','audit-source.status.json','audit-source.stdout','audit-source.stderr','READ-ORDER.json','TARGET.json','COMPLETE_READS.json','READ-INDEX.txt','OWN-PRIOR-CUSTODY.json','GROUNDING_AUTHENTICATION.json','GROUNDING_INPUTS.json','01-source.prompt.txt','freeze_readback.py']
final={'at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'scope':'Independent native whole-PR source verdict and orchestration custody. No publication, merge, runtime integration or parity approval.','head':'3d4a401ed8959befad0b2f10db59093725f27432','tree':'73024912837cbbeaa70b6d2173f5b596eebedcb3','base':'6e3915b70a71657a08f0028b3f70d6074116206a','session':'01a0b38e-9e3c-7883-9816-65199c048935','actual_model':'gpt-5.6-sol','actual_approval_policy':'never','actual_sandbox':'read-only','model_override':None,'verdict':'APPROVE','report_preserved_unmodified':True,'raw_exit_code':0,'elapsed_seconds':result['elapsed_seconds'],'exec_handle':71578,'handle_collected':True,'input_files':len(inputs['files']),'input_aliases':len(inputs['symlinks']),'input_before_after_exact':True,'mandatory_documents':len(coverage['documents']),'literal_complete_chunks':len(ordered),'mandatory_chunks':coverage['mandatory_chunks'],'first_complete_index_order':not deviations,'actual_completed_commands':len(commands),'actual_nonzero_commands':nonzero,'diagnostic_candidates':len(coverage['failed_or_diagnostic_commands']),'truncated_output_candidates':coverage['truncated_output_candidates'],'limitations':['The report says all chunks were read in index order. All required literal bytes were read, but actual first-complete order has the explicit deviations in READ-ORDER.json; the report is retained unchanged. Prior canonical grounding remains separately authenticated.','Four mandatory truncated chunks were subsequently read completely. Broad optional manifest prints were truncated and are not credited as full semantic reads.','One exploratory rg returned source paths and matching source declaration lines, not filenames alone as the report states. The inspected output contained no reviewer report or peer verdict. It is not credited as mandatory source coverage.','One relative-glob lookup failed; three additional searches returned no matches. The complete five nonzero results and ten text-marker diagnostic candidates remain preserved.','Additional global skill reads were not part of the frozen input set; they carry no prelaunch unchanged-input claim.','No peer findings were supplied. No new independent verdict is inferred from raw status or audit.'],'references':[record(r/n) for n in refs]}
save('FINAL-READBACK.json',final)
print(json.dumps(record(r/'FINAL-READBACK.json')))
print(json.dumps({'first_complete_order_deviations':deviations,'nonzero_commands':len(nonzero)}))
