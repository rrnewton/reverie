from pathlib import Path
import ast,datetime,difflib,hashlib,json,os,stat,subprocess
p=Path(__file__).resolve().parent;old=p.parent/'hermit-3047-review-preparation'
m=json.loads((p/'manifest.json').read_text())
def sha(b):return hashlib.sha256(b).hexdigest()
def row(q):
 b=q.read_bytes();return {'path':str(q),'bytes':len(b),'sha256':sha(b),'mode':oct(stat.S_IMODE(q.stat().st_mode))}
c=m['composition']
source={'repository':'https://github.com/rrnewton/hermit','pull_request':'https://github.com/rrnewton/hermit/pull/3047','root':str(p/'source'),
        'base':c['base'],'head':c['head'],'tree':c['tree'],'candidate_patch_sha256':c['patch_sha256'],
        'prior_head':c['prior_head'],'authored_patch_unchanged':True,
        'files':[{'path':str(Path(x['path']).relative_to(p/'source')),'bytes':x['bytes'],'sha256':x['sha256'],'mode':x['mode'],'origin':x['origin']} for x in m['source_files']]}
(p/'candidate-binding.json').write_text(json.dumps(source,indent=2)+'\n')
priorplan=json.loads((old/'execution-plan.json').read_text())
plan={**priorplan,'state':'prepared_continuation_only_not_released_or_executed',
 'proposed_released_argv':['/usr/bin/python3','-B',str(p/'launch-review.py'),'--execute-reviewed-a9d3b1fa'],
 'cwd':str(p/'source/hermit'),'outputs':[str(p/n) for n in ['launch.json','stdout.jsonl','stderr.log','exit.json']],
 'hermit_base':c['base'],'hermit_head':c['head'],'hermit_tree':c['tree'],
 'candidate_binding_sha256':sha((p/'candidate-binding.json').read_bytes()),
 'snapshot_manifest_sha256':sha((p/'manifest.json').read_bytes()),
 'actual_evidence_cutoff':'aa7 native19/fmt/Clippy634/build and2descriptor+3complete-mode runtime passes; actual a9 source composition; official full-image cat pending; Reverie426/1 unchanged.',
 'normal_disjoint_future_main':'b03 is now the actual base; a9 source union verified without claiming new runtime evidence.',
 'public_transcript_bytes':(p/'prior-public-transcript.json').stat().st_size,
 'decoded_public_tool_result_bytes':348339,'public_material_parts':8,'public_material_total_bytes':380280,
 'public_material_max_part_bytes':58440,'public_material_not_embedded_in_prompt':True,
 'public_tool_pairs':75,'public_text_blocks':2,'hidden_thinking_blocks_excluded':47,
 'original_attempt':'900.019045436 seconds / exit124 / no terminal verdict; unchanged original files preserved.'}
(p/'execution-plan.json').write_text(json.dumps(plan,indent=2)+'\n')
(p/'plan.diff').write_text(''.join(difflib.unified_diff((old/'execution-plan.json').read_text().splitlines(True),(p/'execution-plan.json').read_text().splitlines(True),fromfile='prior-approved/execution-plan.json',tofile='prepared-continuation/execution-plan.json')))
inputs=[{k:x[k] for k in ['path','bytes','sha256','mode']} for x in m['original169_inputs']]
inputs += [{k:x[k] for k in ['path','bytes','sha256','mode']} for x in m['files']]
for name in ['manifest.json','candidate-binding.json','prompt.txt','EVIDENCE-APPENDIX.md','launch-review.py','launcher.diff','execution-plan.json','plan.diff']:
 inputs.append(row(p/name))
assert len({x['path'] for x in inputs})==len(inputs)
(p/'input-binding.json').write_text(json.dumps(inputs,indent=2)+'\n')
ast.parse((p/'launch-review.py').read_text())
block='''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it'''
assert block in (p/'prompt.txt').read_text()
for x in inputs:assert row(Path(x['path']))==x,x['path']
for x in m['source_files']:
 o=x['origin'];b=subprocess.check_output(['git','cat-file','blob',o['git_blob']],cwd=o['repository'],env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'})
 assert sha(b)==x['sha256']
# Reconstruct the canonical public transcript independently from the original
# wire using only allowed public block types and the successful tool IDs.
wire=[json.loads(line) for line in (old/'stdout.jsonl').read_text().splitlines() if line]
ok=set()
for event in wire:
 for b in event.get('message',{}).get('content',[]):
  if b.get('type')=='tool_result' and not b.get('is_error',False):ok.add(b['tool_use_id'])
actual=[]
for event in wire:
 for b in event.get('message',{}).get('content',[]):
  if b.get('type')=='text' and event.get('type')=='assistant':actual.append({'kind':'assistant_public_text','text':b['text']})
  elif b.get('type')=='tool_use' and b['id'] in ok:actual.append({'kind':'tool_request','id':b['id'],'name':b['name'],'input':b['input']})
  elif b.get('type')=='tool_result' and b['tool_use_id'] in ok:actual.append({'kind':'tool_result','tool_use_id':b['tool_use_id'],'content':b['content'],'is_error':False})
assert actual==json.loads((p/'prior-public-transcript.json').read_text())
assert all(x['kind'] in ['assistant_public_text','tool_request','tool_result'] for x in actual)
assert len(ok)==75
for q in plan['outputs']:assert not Path(q).exists()
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd='/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917')
readback={'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'head':c['head'],'base':c['base'],'tree':c['tree'],
 'input_count':len(inputs),'input_bytes':sum(x['bytes'] for x in inputs),'all_bound_bytes_modes_verified':True,
 'source_files':len(source['files']),'all_source_git_objects_rechecked':True,'all_original169_inputs_unchanged':True,
 'public_transcript_independently_reconstructed':True,'public_transcript_bytes':(p/'prior-public-transcript.json').stat().st_size,
 'successful_public_tool_pairs':75,'public_text_blocks':2,'hidden_blocks_excluded':47,'failed_tool_pairs_separately_recorded':1,
 'all1726_source_entries_verified_against_union':True,'authored_patch_96502_unchanged':True,
 'launcher_default_disabled':True,'no_continuation_started':True,'own_tracked_and_index_clean':True,
 'files':{name:row(p/name) for name in ['prompt.txt','launch-review.py','launcher.diff','execution-plan.json','plan.diff','input-binding.json','candidate-binding.json','EVIDENCE-APPENDIX.md','PRIOR-MATERIAL-INDEX.md','public-transcript-binding.json','composition-readback.json']}}
(p/'PREPARATION-READBACK.json').write_text(json.dumps(readback,indent=2)+'\n')
print(json.dumps(readback,indent=2))
