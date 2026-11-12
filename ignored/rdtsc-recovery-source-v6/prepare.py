from pathlib import Path
import ast,copy,difflib,hashlib,json,os,shutil
N=Path(__file__).resolve().parent;P=N.parent/'rdtsc-recovery-source-v5';R=N.parents[1];Q=N/'qualification-v1';BASE=N.parent/'publication-fd-composition-v3/source';REL='reverie-kvm/src/terminal_runtime_tests.rs'
def rec(p):
 p=Path(p);st=p.lstat();h=hashlib.sha256()
 if p.is_symlink():b=os.fsencode(os.readlink(p));h.update(b);size=len(b)
 else:
  size=st.st_size
  with p.open('rb')as f:
   for b in iter(lambda:f.read(1024**2),b''):h.update(b)
 r=dict(path=str(p),bytes=size,sha256=h.hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():r.update(kind='symlink',target=os.readlink(p))
 return r
def write(name,v):
 p=N/name;p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x')as f:f.write(v if isinstance(v,str)else json.dumps(v,indent=2)+'\n')
def diff(a,b,path):return ''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='v5/'+path,tofile='v6/'+path))
shutil.copytree(P/'source',N/'source',symlinks=True)
p=N/'source'/REL;before=p.read_text();old='fn tail_injection_allowed(&self) -> bool {';new='fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {';assert before.count(old)==1;after=before.replace(old,new);p.write_text(after)
write('DELTA.patch',diff(before,after,REL))
source=[]
for row in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
 r=copy.deepcopy(row);r['path']=str(N/'source'/r['relative']);p=Path(r['path'])
 if row['kind']=='unexpanded_gitlink':assert p.is_dir() and not list(p.iterdir())
 else:
  was=rec(P/'source'/r['relative']);now=rec(p);assert was['sha256']==row['sha256'] and was['mode']==now['mode']
  if row['relative']==REL:r.update(sha256=now['sha256'],bytes=now['bytes'],changed_from_baseline=True)
  else:assert now['sha256']==row['sha256']
 source.append(r)
assert len(source)==2623
write('SOURCE-MANIFEST.json',source)
Q.mkdir()
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']:
 shutil.copyfile(P/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
caller=''
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;old=p.read_text();assert old.count('rdtsc-recovery-v5-cold-v1')==count
 new=old.replace('rdtsc-recovery-v5-cold-v1','rdtsc-recovery-v6-cold-v1')
 if name=='prepare.py':
  new=new.replace("'reverie-kvm/tests/support/timestamp_terminal.rs']","'reverie-kvm/tests/support/timestamp_terminal.rs', 'reverie-kvm/src/terminal_runtime_tests.rs']")
  assert new.count("'reverie-kvm/src/terminal_runtime_tests.rs'")==1
  new=new.replace('exact 36 declarations','exact 37 declarations')
 p.write_text(new);caller+=diff(old,new,'qualification-v1/'+name)
selection=json.loads((Q/'SELECTORS.json').read_text());selection['groups']['timestamp-37']=dict(artifact='lib',names=['runtime::terminal_tests::terminal_cancellation_and_into_guest_forwarding_do_not_inject_or_start_children'],source=REL,origin='Unchanged existing terminal-cancellation neighbor; private trait signature only adapted',purpose='Existing exact cancellation/IntoGuest forwarding assertions and no injection/child-start effects remain unchanged.')
selection['exact_declarations']=37;old=(Q/'SELECTORS.json').read_text();(Q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n');caller+=diff(old,(Q/'SELECTORS.json').read_text(),'qualification-v1/SELECTORS.json')
order=json.loads((P/'RUN-ORDER.json').read_text());order['phases'].insert(order['phases'].index('timestamp-36')+1,'timestamp-37');assert len(order['phases'])==46
write('RUN-ORDER.json',order);caller+=diff((P/'RUN-ORDER.json').read_text(),(N/'RUN-ORDER.json').read_text(),'RUN-ORDER.json');write('CALLER-DELTA.patch',caller)
shutil.copyfile(P/'execute_phases.py',N/'execute_phases.py');shutil.copyfile(P/'finalize_results.py',N/'finalize_results.py');s=(N/'finalize_results.py').read_text();(N/'finalize_results.py').write_text(s.replace('Actual V5 source','Actual V6 source'))
setup=json.loads((P/'qualification-v1/SETUP.json').read_text());setup['source_root']=str(N/'source');setup['target']=str(R/'target/rdtsc-recovery-v6-cold-v1');paths=[r['relative']for r in setup['source_files']]+[REL];setup['source_files']=[dict(relative=p,file=rec(N/'source'/p))for p in paths];setup['lock']=rec(N/'source/Cargo.lock');write('qualification-v1/SETUP.json',setup)
write('qualification-v1/source-manifest.json',[dict(path=r['relative'],mode=r['mode'],**(dict(sha256=r['sha256'])if 'sha256'in r else {}))for r in source])
write('SOURCE.patch',''.join(''.join(difflib.unified_diff(((BASE/p).read_text()if(BASE/p).exists()else'').splitlines(True),(N/'source'/p).read_text().splitlines(True),fromfile='a/'+p,tofile='b/'+p))for p in paths))
origins=[]
for p in sorted(Q.rglob('*.py')):
 ast.parse(p.read_text());old=P/'qualification-v1'/p.relative_to(Q);origins.append(dict(before=rec(old),after=rec(p),byte_identical=old.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json',dict(records=origins,meaning='Only new empty target spelling, added changed format path, truthful scope count and one unchanged neighbor selector; original helper logic/limits/observer unchanged.'))
controls=json.loads((P/'CONTROLS.json').read_text());controls['declarations']=37;controls['additional_existing_declarations']=2
for r in controls['controls']:r['source']=rec(N/'source'/selection['groups'][r['phase']]['source'])
group=selection['groups']['timestamp-37'];leaf=group['names'][0].split('::')[-1];text=(N/'source'/REL).read_text();assert text.count('fn '+leaf+'(')==1
controls['controls'].append(dict(phase='timestamp-37',artifact='lib',name=group['names'][0],source=rec(N/'source'/REL),line=text[:text.index('fn '+leaf+'(')].count('\n')+1,origin=group['origin'],purpose=group['purpose'],executed=False,cpu_seconds=30,wall_seconds=60));write('CONTROLS.json',controls)
write('SOURCE-CONTINUITY.json',dict(predecessor=rec(P/'TARGET.json'),source_entries=2623,only_changed_path=REL,before=rec(P/'source'/REL),after=rec(N/'source'/REL),production_unchanged=True,all_test_bodies_unchanged=True,assertions_unchanged=True))
write('REPORT.md','''# V6: one private test implementation signature correction

V5 failed its first compile with raw 101 / accepted=false / E0050. The included terminal_runtime_tests.rs RefusingExecutor still implemented the former zero-request private tail predicate. The V5 author symbol audit had searched runtime.rs but missed this included test module; its broad audit statement was incorrect. The exact raw compiler diagnostic and unsuccessful compile receipt remain preserved. No V5 test ran; its emitted non-test library is not a qualified executable.

V6 changes exactly that one method signature to accept _request: &SyscallRequest. It still returns false, its execute and complete_injection still panic, and every existing test body/assertion remains byte-identical. The whole V5 production implementation and all new V5 controls are byte-identical. A full source search identifies all private predicate implementations/calls; no other old signature remains. No behavior, warning suppression, assertion, skip or comparator change.

The changed included file joins the explicit rustfmt/source binding set. Root's requested unchanged terminal_cancellation_and_into_guest_forwarding_do_not_inject_or_start_children neighbor is added, producing 37 exact declarations and 46 phases. The original per-phase bounds, two jobs, offline/locked compiler, observer, phase/lease/retention helpers and all 36 prior selectors remain unchanged. A fresh empty target and newly emitted/retained source-bound harnesses are required; neither V4 harnesses nor V5's incomplete build receive new credit. Stop on the first unaccepted phase with no retry. F3 policy and actual Detcore before/after-charge coverage remain separate pending obligations.

This packet is source/caller preparation, not compilation, test or source approval. Root authorized this exact minimal successor and qualification after reauthentication without an additional planning round.
''')
write('PLAN.md',f'''# V6 qualification plan

Use the preserved V5 plan with the exact one-signature correction, one extra format path and one unchanged neighbor. Source {N/'source'}; new target {setup['target']}. RUN-ORDER has 46 phases / SELECTORS has 37 declarations. Original 600 CPU/900 wall for metadata/compile/check, 30 CPU/60 wall for remaining phases, two jobs, offline/locked, 16 GiB/no swap, 16 MiB stderr, 64 MiB stdout, 16 MiB phase reads and 100 GiB free-space floor are unchanged. Explicit PYTHONOPTIMIZE=0 / optimize=0 / debug=true. New target admission -> original lease -> metadata -> actual dependency closure -> compile -> fresh artifact retention -> remaining phases. All emitted harnesses and linked KVM library must be fresh=false; retained distinct-inode copies only. Actual inventory is separate from selection.

Stop first unaccepted phase. Preserve every raw failure and inspect every selected stdout/stderr for skip/unexecuted messages independently of libtest ok, including historical timestamp-19. Twelve new guest modes remain separate from the four new declarations and two existing unit neighbors. No V5 compilation/test result or existing same-run result qualifies V6. The actual H39ac cancellation composition caller follows only after component qualification, as separately directed.
''')
write('TARGET.json',dict(status='FROZEN V6 SOURCE/CALLER SUCCESSOR; NOT YET EXECUTED',predecessor=rec(P/'TARGET.json'),predecessor_failure=rec(P/'qualification-v1/controls/compile/result.json'),source=str(N/'source'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),setup=rec(Q/'SETUP.json'),selectors=rec(Q/'SELECTORS.json'),observer=rec(Q/'observer/observer.py'),production_unchanged_from_v5=True,test_bodies_unchanged=True,phases=46,declarations=37,root_minimal_successor_qualification_authorized=True))
records={}
def add(p):
 p=Path(p)
 if p.is_file()or p.is_symlink():records[str(p)]=rec(p)
for p in N.rglob('*'):add(p)
for row in json.loads((P/'INPUTS.json').read_text())['records']:
 r=rec(row['path']);assert r['sha256']==row['sha256'] and r['mode']==row['mode'];records[r['path']]=r
for p in [P/'TARGET.json',P/'INPUTS.json',P/'READBACK.json',P/'REPORT.md',P/'PLAN.md',P/'qualification-v1/compile-plan.json',P/'qualification-v1/controls/compile/result.json',P/'qualification-v1/observer/compile/stdout',P/'qualification-v1/observer/compile/stderr',P/'qualification-v1/observer/compile/result.json']:add(p)
for row in json.loads((P/'qualification-result-v1/INPUTS.json').read_text())['records']:
 r=rec(row['path']);assert r['sha256']==row['sha256'] and r['mode']==row['mode'];records[r['path']]=r
for p in (P/'compile-refusal-v1').rglob('*'):add(p)
for p in (P/'qualification-result-v1').iterdir():add(p)
write('INPUTS.json',dict(records=list(records.values()),source_entries=2623))
for row in records.values():assert rec(row['path'])==row
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),records=len(records),source_entries=2623,exact_one_signature_change=True,all_assertions_and_test_bodies_unchanged=True,production_unchanged=True,no_execution=True,original_limits_and_helpers_preserved=True))
print(json.dumps({p:rec(N/p)for p in ['TARGET.json','SOURCE.patch','DELTA.patch','CALLER-DELTA.patch','REPORT.md','PLAN.md','INPUTS.json','READBACK.json']},indent=2))
