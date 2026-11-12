#!/usr/bin/python3
"""Authenticate and freeze continuation preparation; no admission or product execution."""
import ast,difflib,json,os,stat,sys
from pathlib import Path
from common import check_file,digest,file_record,json_read,require,write_new
N=Path(__file__).resolve().parent;Q=Path(json_read(N/'SETUP.json')['origin']);F=Q/'failure-final-v1'
require(sys.flags.optimize==0,'ordinary Python required')
old=json_read(Q/'SELECTORS.json');selection=json_read(N/'SELECTORS.json');result=json_read(F/'RESULTS.json');accepted=[r['name']for r in result['phases']if r['accepted'] and r['kind']=='test']
require(len(accepted)==10 and len(selection['groups'])==len(selection['proposed_order'])==53,'wrong selector counts')
require(selection['proposed_order']==result['remaining_unqualified_selectors']==[n for n in old['proposed_order']if n not in accepted],'wrong continuation order')
require(all(selection['groups'][n]==old['groups'][n]for n in selection['groups']),'selector changed')
require(len({(g['artifact'],tuple(g['names']))for g in selection['groups'].values()})==53,'duplicate declaration')
a=json_read(Q/'PHASE-PLAN.json');b=json_read(N/'PHASE-PLAN.json');by={r['name']:r for r in a['phases']}
require(len(b['phases'])==55 and [r['name']for r in b['phases']]==selection['proposed_order']+['core-check','clippy'],'wrong phase denominator')
require(all(r==by[r['name']]for r in b['phases']),'argv/limits/full phase row changed')
origins=[];delta=[]
for rel in ['execute.py','prepare.py','admit_target.py','phase.py','common.py','cache_lease.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json','toolchain-standard-inputs.json']:
 before,after=Q/rel,N/rel;ra,rb=file_record(before),file_record(after);origins.append(dict(before=ra,after=rb,byte_identical=ra['sha256']==rb['sha256']))
 if ra['sha256']!=rb['sha256']:delta.extend(difflib.unified_diff(before.read_text().splitlines(True),after.read_text().splitlines(True),fromfile='original/'+rel,tofile='continuation/'+rel))
require([r['after']['path'].removeprefix(str(N)+'/')for r in origins if not r['byte_identical']]==['execute.py','prepare.py','admit_target.py'],'unexpected runtime helper delta')
write_new(N/'RUNNER_ORIGINS.json',dict(records=origins,original_failure=file_record(F/'READBACK.json')))
(N/'CALLER-DELTA.patch').write_text(''.join(delta))
write_new(N/'CONTINUITY.json',dict(original_caller=file_record(Q/'CALLER-TARGET.json'),original_failure=file_record(F/'READBACK.json'),accepted_tests=accepted,unqualified_tests=selection['proposed_order'],same_55_phase_rows=True,original_bound_source=file_record(Q/'source-manifest.json'),source_reused_without_mutation=True,old_lease_unresolved=True,new_lease_not_created=True,target_reused_for_previously_unrun_checks_only=True,retained_artifacts=file_record(Q/'retained-binaries/BINDING.json'),retained_rlib=file_record(F/'retained-linked-library.rlib'),no_prior_results_relabelled=True))
parsed=[]
for p in [*sorted(N.glob('*.py')),*sorted((N/'observer').glob('*.py'))]:ast.parse(p.read_text(),filename=str(p));parsed.append(file_record(p))
write_new(N/'PREPARATION-CHECKS.json',dict(ast_only=parsed,all55phase_rows_unchanged=True,all53selectors_unchanged=True,python=dict(executable=sys.executable,argv=sys.argv,optimize=sys.flags.optimize,debug=__debug__),no_build_test_or_admission=True))
files={};links={};gitlinks={}
def add(p):
 p=Path(p)
 if p.is_symlink():links[str(p)]=dict(path=str(p),target=os.readlink(p))
 else:files[str(p)]=file_record(p)
held=json_read(F/'INPUTS.json')
for r in held['files']:check_file(r);files[r['path']]=r
for r in held['symlinks']:require(Path(r['path']).is_symlink() and os.readlink(r['path'])==r['target'],'prior alias drift');links[r['path']]=r
for r in held['unexpanded_gitlinks']:require(Path(r['path']).is_dir() and not Path(r['path']).is_symlink(),'prior gitlink drift');gitlinks[r['path']]=r
add(F/'INPUTS.json');add(F/'READBACK.json')
previous=N.parent/'qualification-continuation-v1'
for p in previous.iterdir():
 if p.is_file():add(p)
for d in ['observer','loader']:
 for p in (previous/d).iterdir():add(p)
for p in N.iterdir():
 if p.is_file():add(p)
for d in ['observer','loader']:
 for p in (N/d).iterdir():require(p.is_file(),'unexpected prepared child');add(p)
absent=[str(N/n)for n in ['lane.lease','lease-state','tmp','cache','controls','launch','TARGET-ADMISSION.json','EXECUTION-RESULT.json']]
for p in absent:require(not Path(p).exists() and not Path(p).is_symlink(),'continuation already admitted')
target=Path(json_read(N/'SETUP.json')['target']);prior=json_read(Q/'TARGET-ADMISSION.json');s=target.stat();require([s.st_dev,s.st_ino,s.st_uid]==prior['identity'] and stat.S_IMODE(s.st_mode)==0o700,'original private target changed')
index=dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(links.values(),key=lambda x:x['path']),unexpanded_gitlinks=sorted(gitlinks.values(),key=lambda x:x['path']),must_be_absent_before_execution=absent,excluded_mutable=['original private target contents','old lease bytes held separately by exact token under exclusive lock','new lease/runtime destinations'],scope='Immutable V2/source/raw/history plus unchanged 55 rows, fresh private continuation lease only after authorization')
write_new(N/'CALLER-INPUTS.json',index)
write_new(N/'CALLER-TARGET.json',dict(status='PREPARED ONLY; root authorization required',inputs=file_record(N/'CALLER-INPUTS.json'),origin=file_record(Q/'CALLER-TARGET.json'),source=file_record(Q.parent/'TARGET.json'),failure=file_record(F/'READBACK.json'),continuity=file_record(N/'CONTINUITY.json'),readme=file_record(N/'README.md'),execution=file_record(N/'EXECUTION.md'),driver=file_record(N/'execute.py'),prepare=file_record(N/'prepare.py'),admission=file_record(N/'admit_target.py'),phase_plan=file_record(N/'PHASE-PLAN.json'),helper_delta=file_record(N/'CALLER-DELTA.patch'),command=['/usr/bin/python3','-B',str(N/'execute.py'),'run'],environment={'PYTHONOPTIMIZE':'0'},cwd=str(N),planned_phases=55,planned_tests=53,executed_phases=0))
for r in files.values():check_file(r)
for r in links.values():require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'final alias drift')
write_new(N/'CALLER-READBACK.json',dict(target=file_record(N/'CALLER-TARGET.json'),inputs=file_record(N/'CALLER-INPUTS.json'),regular_files=len(files),symlinks=len(links),gitlinks=len(gitlinks),all_current_bytes_modes_unchanged=True,no_admission_or_execution=True,original_failed_phase_preserved=True))
print(json.dumps({n:file_record(N/n)for n in ['CALLER-TARGET.json','CALLER-INPUTS.json','CALLER-READBACK.json','CALLER-DELTA.patch']},indent=2))
