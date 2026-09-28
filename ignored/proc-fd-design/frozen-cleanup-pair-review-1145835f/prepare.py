from pathlib import Path
import datetime,difflib,hashlib,json,os,re,shutil,subprocess
p=Path(__file__).resolve().parent
slot=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-frozen-cleanup-20260917')
old=p.parent/'hermit-3047-review-composition-231'
parent_evidence=Path('/home/newton/work/dev-hermit/ignored/kvm-parity-20260914-codex/next-work/queue-drain-20260916/parent-ledger/frozen-cleanup-repair-20260917')
au_evidence=slot/'ignored/frozen-cleanup'
env=dict(os.environ,GIT_NO_LAZY_FETCH='1',GIT_OPTIONAL_LOCKS='0')
def git(repo,*args):return subprocess.check_output(['git','-C',str(repo),*args],env=env)
def sha(b):return hashlib.sha256(b).hexdigest()
def write(path,data,mode=0o644):
 path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(data);path.chmod(mode)
def data_row(path):
 b=path.read_bytes();return {'path':str(path),'bytes':len(b),'sha256':sha(b),'mode':oct(path.stat().st_mode&0o777)}
repos={
 'parent':{'repository':str(slot),'base':'0510ee72b3c94f164b62d6a0d24be912b23b490b','head':'fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a','tree':'f127f6563d771eb172980af74e398fed8b6e0edd','changed':['ci-hub/validate/start_unit.py','ci-hub/validate/tests/test_start_unit.py']},
 'agent-utils':{'repository':str(slot/'hermit/agent-utils'),'base':'8720799f78f8ef56d796723dbc473a9171a4f4eb','head':'1145835fc804f47ae48b29b89009e6937184175a','tree':'e470a86697cbb9590c23d45ee5fab9ade12aab01','changed':['py/wrkslots/cli.py','py/wrkslots/tests/test_lifecycle.py']}}
contexts={
 'parent':['AGENTS.md','ci-hub/validate/run_registry.py','ci-hub/validate/service_result.py','ci-hub/validate/immutable_tool_authority.py','ci-hub/validate/tree_disposition.py','ci-hub/bin/wrkslots','ci-hub/health/host_process_context.py','ci-hub/health/removal_handoff_writer.py','ci-hub/lib/git_env.py'],
 'agent-utils':['AGENTS.md','py/wrkslots/__main__.py','py/wrkslots/__init__.py','py/wrkslots/pyproject.toml','scripts/validate.py','Makefile']}
files=[]
def snapshot(name,repo,rev,path,section):
 entry=git(repo,'ls-tree',rev,'--',path).decode().strip();fields=entry.split()
 assert len(fields)>=4 and fields[1]=='blob',(rev,path,entry)
 data=git(repo,'show',rev+':'+path);mode=int(fields[0],8)&0o777
 rel=f'{name}/{path}' if section=='source' else f'base/{name}/{path}'
 target=p/'source'/rel
 write(target,data,mode)
 row=data_row(target);row.update(path=rel,repository=str(repo),revision=rev,git_path=path,git_blob=fields[2],git_mode=fields[0]);files.append(row)
for name,r in repos.items():
 repo=Path(r['repository']);assert git(repo,'rev-parse',r['head']+'^{tree}').decode().strip()==r['tree']
 assert git(repo,'rev-parse',r['head']+'^').decode().strip()==r['base']
 changed=git(repo,'diff','--name-only',r['base'],r['head']).decode().splitlines();assert changed==r['changed']
 raw=git(repo,'cat-file','commit',r['head']);write(p/(name+'-commit.txt'),raw)
 write(p/(name+'-tree.txt'),git(repo,'ls-tree','-r',r['head']))
 for path in r['changed']:
  snapshot(name,repo,r['base'],path,'base');snapshot(name,repo,r['head'],path,'source')
  assert (repo/path).read_bytes()==git(repo,'show',r['head']+':'+path)
 for path in contexts[name]:snapshot(name,repo,r['head'],path,'source')
patches=[('parent',parent_evidence/'parent-implementation-2/parent.patch','2fed738285f0a8d1426293e2d804faee5f603921abf6adec82b10796b3563d12'),('agent-utils',au_evidence/'au-corrected-inputs/change.patch','c7b39cad7b7813514a5d78f74307be5c9814a3586430dfe8f9acc7696bf60940')]
for name,path,expected in patches:
 b=path.read_bytes();assert sha(b)==expected;write(p/(name+'.patch'),b);repos[name]['patch_sha256']=expected
 blocks=re.split(r'(?=^diff --git )',b.decode(),flags=re.M)[1:];assert len(blocks)==2
 for logical,block in zip(repos[name]['changed'],blocks):
  assert block.startswith(f'diff --git a/{logical} b/{logical}\n')
  src=git(repos[name]['repository'],'show',repos[name]['base']+':'+logical).decode().splitlines(True);out=[];at=0;lines=block.splitlines(True);i=0
  while i<len(lines):
   m=re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@',lines[i])
   if not m:i+=1;continue
   n=int(m[1])-1;out.extend(src[at:n]);at=n;i+=1
   while i<len(lines) and not lines[i].startswith('@@ '):
    x=lines[i]
    if x.startswith((' ','-')):assert src[at]==x[1:],(name,logical,at);at+=1
    if x.startswith((' ','+')):out.append(x[1:])
    i+=1
  out.extend(src[at:]);assert ''.join(out).encode()==git(repos[name]['repository'],'show',repos[name]['head']+':'+logical)
# Actual parent consumer/pin context is distinct from the separately reviewed AU candidate.
consumer='98d58b9bd6ea722c6d7087d45d5fd81792abfa16'
assert git(slot,'ls-tree',repos['parent']['head'],'--','hermit').decode().split()[2]==consumer
assert git(slot/'hermit','ls-tree',consumer,'--','agent-utils').decode().split()[2]=='2781b1054efc3a9c561dbed35584a6fca1ed8676'
for path in ['scripts/validate.rs','ci/manifest-plan/validation-service-result-schema.json']:
 snapshot('hermit-contract',slot/'hermit',consumer,path,'source')
# Preserve only terminal evidence. The concurrently running make validate outputs are excluded.
evidence=[]
def copy_evidence(original,relative):
 target=p/'evidence'/relative;write(target,original.read_bytes(),original.stat().st_mode&0o777)
 row=data_row(target);row['original_path']=str(original);evidence.append(row)
for stage in ['parent-implementation-1','parent-implementation-2']:
 for name in ['plan.json','tests.receipt.json','tests.stderr','tests.stdout','start_unit.py','test_start_unit.py']:
  copy_evidence(parent_evidence/stage/name,'parent/'+stage+'/'+name)
copy_evidence(parent_evidence/'parent-commit-1/readback.json','parent/commit-readback.json') if (parent_evidence/'parent-commit-1/readback.json').exists() else None
manifest=json.loads((au_evidence/'AU-CORRECTED-EVIDENCE-MANIFEST.json').read_text())
for row in manifest:
 f=Path(row['path']);assert len(f.read_bytes())==row['bytes'] and sha(f.read_bytes())==row['sha256']
 copy_evidence(f,'agent-utils/'+f.relative_to(au_evidence).as_posix())
for name in ['AU-COMMIT-READBACK.json','AU-SELF-REVIEW.md','AU-CORRECTED-EVIDENCE-MANIFEST.json','AU-COMMIT-MESSAGE.txt']:
 copy_evidence(au_evidence/name,'agent-utils/'+name)
# Exact prior failure source and corrected source are kept separately for sensitivity review.
for directory in ['au-corrected-inputs','au-initial-binding-before-fix','au-initial-binding-corrected']:
 for f in sorted((au_evidence/directory).iterdir()):
  if f.is_file() and f.name not in {'output.txt','time.txt','exit.txt','SOURCE-READBACK.json'}:
   copy_evidence(f,'agent-utils/'+directory+'/'+f.name)
for dirname,filenames in [('parent-cleanup-slice-review',['REVIEW.md','SOURCE-BINDING.json']),('frozen-cleanup-contract-review',['REPORT.md','SOURCE-BINDING.json'])]:
 for name in filenames:copy_evidence(p.parent/dirname/name,'prior/'+dirname+'/'+name)
for name in ['validate-kvm-closed-stdin-a88f9480-frozen.json','validate-kvm-closed-stdin-a88f9480-frozen.service-result.json']:
 copy_evidence(p.parent/'frozen-cleanup-contract-review/evidence'/name,'actual-1839/'+name)
copy_evidence(Path('/home/newton/work/dev-hermit/.skills/code-review/SKILL.md'),'instructions/code-review-SKILL.md')
source={'root':str(p/'source'),'base':repos['agent-utils']['base'],'head':repos['agent-utils']['head'],'tree':repos['agent-utils']['tree'],'repositories':repos,'candidate_patch_sha256':sha((p/'agent-utils.patch').read_bytes()+(p/'parent.patch').read_bytes()),'files':files,'actual_parent_pinned_consumer':consumer,'actual_consumer_pinned_au':'2781b1054efc3a9c561dbed35584a6fca1ed8676','deployment_note':'This source-pair review does not claim parent fba5 already pins AU114. Actual consumer activation/pin coherence remains separately required; neither pin is altered here.'}
write(p/'candidate-binding.json',(json.dumps(source,indent=2)+'\n').encode())
write(p/'EVIDENCE-MANIFEST.json',(json.dumps(evidence,indent=2)+'\n').encode())
launcher=(old/'launch-review.py').read_text().replace('--execute-reviewed-4392560a','--execute-reviewed-1145835f-fba5d50c').replace("str(Path(source['root']) / 'hermit')","str(Path(source['root']) / 'parent')").replace("'base': source['base'], 'head': source['head'], 'tree': source['tree'],","'base': source['base'], 'head': source['head'], 'tree': source['tree'],\n          'repositories': source['repositories'],")
write(p/'launch-review.py',launcher.encode())
write(p/'launcher.diff',''.join(difflib.unified_diff((old/'launch-review.py').read_text().splitlines(True),launcher.splitlines(True),fromfile='prior/launch-review.py',tofile='pair/launch-review.py')).encode())
plan=json.loads((old/'execution-plan.json').read_text())
for key in ['hermit_base','hermit_head','hermit_tree','prior_literal_source_approval','actual_evidence_cutoff','no_rebuild_or_relist_or_retest']:plan.pop(key,None)
plan.update(state='prepared_under_root_source_review_release_not_executed',proposed_released_argv=['/usr/bin/python3','-B',str(p/'launch-review.py'),'--execute-reviewed-1145835f-fba5d50c'],cwd=str(p/'source/parent'),repositories=repos,candidate_binding_sha256=sha((p/'candidate-binding.json').read_bytes()),outputs=[str(p/n) for n in ['launch.json','stdout.jsonl','stderr.log','exit.json']],source_scope='Complete committed parent+AU preservation-classification correction, including full changed source/tests and unchanged canonical authority, process census and typed direct-removal context.',actual_evidence_cutoff='AU corrected focused43 and strict mypy13 passed; earlier four initial-window failures preserved. Parent28 passes in first32-method attempt plus corrected4 passes, production identical. Normal AU make validate pending at freeze and excluded from immutable inputs. No real1839 classifier, cleanup, admission or guest performed.')
for row in plan['tools_current_metadata']:
 resolved=Path(shutil.which(row['entrypoint']) if not Path(row['entrypoint']).is_absolute() else row['entrypoint']).resolve();assert str(resolved)==row['resolved_path'];assert sha(resolved.read_bytes())==row['sha256']
write(p/'execution-plan.json',(json.dumps(plan,indent=2)+'\n').encode())
print(json.dumps({'source_copies':len(files),'evidence_files':len(evidence),'repositories':repos,'candidate_binding_sha256':sha((p/'candidate-binding.json').read_bytes()),'launcher_sha256':sha((p/'launch-review.py').read_bytes()),'pending':'write and read prompt/EVIDENCE, then final input binding; no reviewer launched'},indent=2))
