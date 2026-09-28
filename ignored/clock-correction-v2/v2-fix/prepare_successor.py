#!/usr/bin/python3
"""Prepare only the two-import successor and identical-algorithm fresh caller."""
import ast,difflib,hashlib,json,os,shutil,stat,subprocess,sys
from pathlib import Path
V=Path(__file__).resolve().parent;W=V.parent;L=W.parent.parent
OLD=W/'qualification-v1';Q=W/'qualification-v2';R=V/'source'
def require(x,m):
 if not x:raise RuntimeError(m)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);s=p.lstat();require(stat.S_ISREG(s.st_mode),'not regular '+str(p));h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=h.hexdigest())
def write(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
def text(p,s):p.open('x').write(s)
def refresh(v):
 if isinstance(v,dict):
  if {'path','bytes','mode','sha256'}<=v.keys():return rec(v['path'])
  return {k:refresh(x) for k,x in v.items()}
 if isinstance(v,list):return [refresh(x) for x in v]
 return v
def rewrite(v):
 if isinstance(v,dict):return {k:rewrite(x) for k,x in v.items()}
 if isinstance(v,list):return [rewrite(x) for x in v]
 if isinstance(v,str):
  for a,b in [(str(OLD),str(Q)),(str(W/'source'),str(R)),(str(W/'private-target'),str(V/'private-target'))]:
   if v==a or v.startswith(a+'/'):return b+v[len(a):]
 return v
require(sys.flags.optimize==0,'unoptimized preparation required')
require(not R.exists() and not Q.exists(),'preserve any existing successor')
shutil.copytree(W/'source',R,symlinks=True)
rel='reverie-ptrace/src/injection_stop_tests.rs';p=R/rel;s=p.read_text()
require(s.count('use std::io::Write as _;\n')==1 and s.count('use reverie::syscalls::args::PathPtr;\n')==1,'unexpected imports')
s=s.replace('use std::io::Write as _;\n','').replace('use reverie::syscalls::args::PathPtr;','use reverie::syscalls::PathPtr;');p.write_text(s)
require('pub use crate::args::*;' in (R/'reverie-syscalls/src/lib.rs').read_text(),'public export absent')
# Existing bounded author formatter, recorded separately and never counted as qualification.
shutil.copy2(W/'author_format_3.py',V/'author_format.py')
formatted=subprocess.run(['/usr/bin/python3','-B',str(V/'author_format.py')],env={**os.environ,'PYTHONOPTIMIZE':'0','PYTHONDONTWRITEBYTECODE':'1'},capture_output=True,check=False)
text(V/'author-format-driver.stdout',formatted.stdout.decode());text(V/'author-format-driver.stderr',formatted.stderr.decode())
require(formatted.returncode==0,'author formatting refused')
prior=load(W/'SOURCE-MANIFEST.json');manifest=[];changed=[]
for row in prior:
 path=R/row['path'];new=dict(row)
 if row['mode']=='120000':value=hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
 else:value=rec(path)['sha256']
 new['sha256']=value;manifest.append(new)
 if value!=row['sha256']:changed.append(row['path'])
require(changed==[rel],'only the import file may differ')
oldtext=(W/'source'/rel).read_text();newtext=(R/rel).read_text()
require(oldtext.replace('use std::io::Write as _;\n','').replace('use reverie::syscalls::args::PathPtr;\n','').replace('use reverie::syscalls::PathPtr;\n','')==newtext.replace('use reverie::syscalls::PathPtr;\n',''),'non-import source change')
write(V/'SOURCE-MANIFEST.json',manifest)
def patch(a,b,names):
 out=[]
 for n in names:
  old=a/n;new=b/n;out.append('diff --git a/'+n+' b/'+n+'\n')
  if not old.exists():out.append('new file mode 100644\n')
  out.extend(difflib.unified_diff(old.read_text().splitlines(True) if old.exists() else [],new.read_text().splitlines(True),fromfile='a/'+n if old.exists() else '/dev/null',tofile='b/'+n))
 return ''.join(out)
text(V/'V2-FIX.patch',patch(W/'source',R,[rel]))
fullpaths=load(W/'CONTINUITY.json')['full_changed_paths'];text(V/'SOURCE.patch',patch(W/'base',R,fullpaths))
write(V/'CONTINUITY.json',dict(prior_source_target=rec(W/'TARGET.json'),prior_failure_target=rec(OLD/'final-v1/TARGET.json'),prior_failure_readback=rec(OLD/'final-v1/READBACK.json'),changed_paths=changed,only_two_import_corrections=True,all_other_entries_identical=len(prior)-1,production_and_test_bodies_and_assertions_identical=True,base='3d4a401ed8959befad0b2f10db59093725f27432',landed_identical_base='b5e2ab49cd99e5d456fa0238b8cebd75958c529f',tree='73024912837cbbeaa70b6d2173f5b596eebedcb3',public_export=rec(R/'reverie-syscalls/src/lib.rs')))
text(V/'REPORT.md','''# Two-import correction after the preserved compiler failure

This source-only successor fixes exactly two test imports in injection_stop_tests.rs: use the public reverie::syscalls::PathPtr re-export, and remove the unused std::io::Write import. The existing formatter places PathPtr with the other public syscall imports. The public export is verified from the bound reverie-syscalls/src/lib.rs. Every production byte, test body, assertion, guest byte sequence, five-second deadline and other source entry remains identical to the first candidate.

The first run remains metadata raw0/accepted and compile raw101/refused, with E0603 plus the unused import warning. All 38 declarations were unrun. Its raw streams, source, completed private token and final evidence are preserved. This successor is unexecuted, and it claims no clock result or approval.

SOURCE.patch is the full change against exact3d4/tree730, also landed b5's identical tree. V2-FIX.patch shows only the import correction against W/source. The original author report retains the precise post_exec0/timer1 rationale and real1/65 or65/129 requirements. Seven new declarations still cover eight modes; Write checks only its three meaningful arguments. No selection or coverage claim expanded.

The fresh caller is W/qualification-v2, alongside qualification-v1, so all 11 runtime and orchestration helper files remain byte-identical. Only data bindings and owned source/target/output paths change. There are still 45 observed phases and 38 declarations under the original limits. Both new harnesses must be emitted fresh=false and retained; no failed predecessor target or ELF is reused. Author formatting is separately recorded, not qualification. No metadata, compiler, test, guest or lease admission has run for this successor.
''')
write(V/'TARGET.json',dict(scope='Unexecuted two-import test-only successor; original compiler failure preserved',base='3d4a401ed8959befad0b2f10db59093725f27432',source_root=str(R),source_manifest=rec(V/'SOURCE-MANIFEST.json'),full_patch=rec(V/'SOURCE.patch'),delta=rec(V/'V2-FIX.patch'),continuity=rec(V/'CONTINUITY.json'),report=rec(V/'REPORT.md'),prior_report=rec(W/'REPORT.md'),format=rec(V/'AUTHOR-FORMAT-3.json')))
Q.mkdir(mode=0o700)
for name in ['observer','cache','controls','launch','lease-state','loader','tmp']:(Q/name).mkdir(mode=0o700)
helper_names=['phase.py','common.py','cache_lease.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json','execute.py','prepare.py','retain_artifacts.py','bind_dependencies.py']
helpers=[]
for name in helper_names:
 shutil.copy2(OLD/name,Q/name);require((OLD/name).read_bytes()==(Q/name).read_bytes(),'helper changed');helpers.append(dict(before=rec(OLD/name),after=rec(Q/name),identical=True))
for p in list(Q.glob('*.py'))+list((Q/'observer').glob('*.py')):ast.parse(p.read_text(),filename=str(p))
write(Q/'HELPER-CONTINUITY.json',dict(runtime_and_orchestration_helpers=helpers,all_eleven_identical=True,supervision_guards_parser_and_lease_algorithm_unchanged=True))
for name in ['SETUP.json','PHASES.json']:write(Q/name,rewrite(load(OLD/name)))
shutil.copy2(OLD/'SELECTORS.json',Q/'SELECTORS.json')
oldphases=load(OLD/'PHASES.json')['phases'];newphases=load(Q/'PHASES.json')['phases']
require(len(newphases)==45 and len([x for x in newphases if x['kind']=='test'])==38,'selection changed')
require(newphases==rewrite(oldphases),'phase difference beyond paths')
history=load(OLD/'HISTORY.json');history.update(prior_import_failure_target=rec(OLD/'final-v1/TARGET.json'),prior_import_failure_results=rec(OLD/'final-v1/RESULTS.json'),prior_import_failure_readback=rec(OLD/'final-v1/READBACK.json'),prior_import_failure_lease_release=rec(OLD/'final-v1/LEASE-RELEASE.json'))
write(Q/'HISTORY.json',history)
target=V/'private-target';target.mkdir(mode=0o700);t=target.stat()
lease=Q/'lane.lease';fd=os.open(lease,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.close(fd);ls=lease.stat()
context=rewrite(load(OLD/'CONTEXT.json'));context['sources']=[dict(root=str(R),manifest=rec(V/'SOURCE-MANIFEST.json'))]
context['target']=dict(path=str(target),identity=[t.st_dev,t.st_ino,t.st_uid]);context['lease']=dict(path=str(lease),owner_slot=str(L),identity=[ls.st_dev,ls.st_ino,ls.st_uid],state_directory=str(Q/'lease-state'))
context['inputs']=refresh(context['inputs'])
context['inputs']+=[rec(V/n) for n in ['TARGET.json','SOURCE-MANIFEST.json','SOURCE.patch','V2-FIX.patch','CONTINUITY.json','REPORT.md']]+[rec(OLD/'final-v1'/n) for n in ['TARGET.json','RESULTS.json','READBACK.json']]
context['absent_inputs']+=[str(V/'.cargo/config'),str(V/'.cargo/config.toml')]
for name in context['absent_inputs']:require(not Path(name).exists() and not Path(name).is_symlink(),'Cargo override appeared')
write(Q/'CONTEXT.json',context)
plan=dict(context);plan['inputs']=[*context['inputs'],rec(Q/'CONTEXT.json')]
plan.update(path=str(Q/'metadata-plan.json'),name='metadata',kind='metadata',argv=newphases[0]['argv'],selected=[],artifact_selectors=[],output=str(Q/'observer/metadata'),control=str(Q/'controls/metadata'),scope='Two-import successor fresh metadata; original compile101 preserved; no test result transfer')
write(Q/'metadata-plan.json',plan)
text(Q/'CALLER-DELTA.patch',''.join(difflib.unified_diff((OLD/n).read_text().splitlines(True),(Q/n).read_text().splitlines(True),fromfile='qualification-v1/'+n,tofile='qualification-v2/'+n) for n in []))
# Expose complete data changes; helper bodies have no delta.
with (Q/'CALLER-DELTA.patch').open('a') as f:
 for name in ['SETUP.json','PHASES.json','CONTEXT.json','metadata-plan.json','HISTORY.json','HELPER-CONTINUITY.json']:
  f.writelines(difflib.unified_diff((OLD/name).read_text().splitlines(True),(Q/name).read_text().splitlines(True),fromfile='qualification-v1/'+name,tofile='qualification-v2/'+name))
text(Q/'README.md','''# Fresh qualification of the two-import successor

Preparation only; root must inspect this caller before execution. Exact once-only invocation is `PYTHONOPTIMIZE=0 /usr/bin/python3 -B execute.py run` from this directory.

The complete 45-phase and 38-declaration selection is identical after substitution of source/retained-output paths. This includes all nine original declarations and seven new declarations/eight modes. All 11 runtime/orchestration helpers are byte-identical to qualification-v1, including the observer, before-exec gate, terminal/parser/lease algorithms and fresh artifact retainer. CALLER-DELTA.patch contains data/path/history rebinding only. The old raw101 compile failure remains failed; no test was run there.

Metadata/compile/core/Clippy retain 600 CPU seconds / 900 wall seconds; format, both inventories and every exact test retain 30 CPU seconds / 60 wall seconds. Memory remains16GiB, swap0, freefloor100GiB, stderr16MiB lethal, stdout64MiB live with16MiB reader, two Cargo jobs and five-second guest wait deadlines. Loader queries remain5seconds/64KiB. No retries, limit changes, case omissions, ignored-test credit or oracle edits.

The source is the isolated v2-fix/source snapshot; the target is the fresh empty v2-fix/private-target, with this caller's separate private lease/state. Neither failed target nor old lease is used. Both test harnesses must be freshly emitted and retained before any later Cargo phase. The old legacy test's skip remains forbidden by the same before-run mapping requirement and after-run marker check. Stop on the first refusal and retain full raw status, terminal/accounting/source evidence. No Hermit or parity execution is included.
''')
text(Q/'EXECUTION.md','Preparation only; after root authorization run once:\n\n```\nPYTHONOPTIMIZE=0 /usr/bin/python3 -B execute.py run\n```\n\nWorking directory: '+str(Q)+'. Preserve the first refusal; no automatic retry or change to source, assertions, limits or selection. The fixed45-phase /38-declaration sequence and unchanged11 helper bodies are bound by CALLER-TARGET.json.\n')
files={};links={}
for row in load(OLD/'final-v1/INPUTS.json')['files']:
 actual=rec(row['path']);require(actual==row,'old evidence/input drift: '+row['path']);files[row['path']]=row
for row in load(OLD/'final-v1/INPUTS.json')['symlinks']:
 path=Path(row['path']);require(path.is_symlink() and os.readlink(path)==row['target'],'old alias drift');links[row['path']]=row
for root in [V,Q]:
 for path in sorted(root.rglob('*')):
  if path==lease or path.is_relative_to(target):continue
  if path.is_symlink():links[str(path)]=dict(path=str(path),target=os.readlink(path))
  elif path.is_file():files[str(path)]=rec(path)
for name in ['TARGET.json','READBACK.json','INPUTS.json']:
 path=OLD/'final-v1'/name;files[str(path)]=rec(path)
absent=[Q/n for n in ['EXECUTION-RESULT.json','retained-binaries','dependency-inputs.json','dependency-symlinks.json','dependency-closure.json']]
for row in newphases:
 absent += [Q/'controls'/row['name'],Q/'observer'/row['name']]
 if row['name']!='metadata':absent.append(Q/(row['name']+'-plan.json'))
for p in absent:require(not p.exists() and not p.is_symlink(),'future output exists')
write(Q/'INPUTS.json',dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(links.values(),key=lambda x:x['path']),must_be_absent_before_execution=[str(p) for p in absent],mutable_targets_and_leases_excluded=True,prior_failure_preserved=True))
write(Q/'CALLER-TARGET.json',dict(scope='Unexecuted identical45-phase/38-declaration caller for two-import successor',source_target=rec(V/'TARGET.json'),inputs=rec(Q/'INPUTS.json'),metadata_plan=rec(Q/'metadata-plan.json'),phases=rec(Q/'PHASES.json'),selectors=rec(Q/'SELECTORS.json'),helper_continuity=rec(Q/'HELPER-CONTINUITY.json'),caller_delta=rec(Q/'CALLER-DELTA.patch'),readme=rec(Q/'README.md'),execution=rec(Q/'EXECUTION.md'),command=['/usr/bin/python3','-B','execute.py','run'],environment={'PYTHONOPTIMIZE':'0'},cwd=str(Q),no_retry=True))
write(Q/'READBACK.json',dict(source_target=rec(V/'TARGET.json'),caller_target=rec(Q/'CALLER-TARGET.json'),inputs=rec(Q/'INPUTS.json'),regular_files=len(files),literal_aliases=len(links),absent_outputs=len(absent),all_eleven_helpers_identical=True,phase_and_selector_continuity='Exact original records after source/output path substitution; selectors byte-identical',planned_phases=45,exact_declarations=38,library_declarations=37,integration_declarations=1,new_declarations=7,new_modes=8,source_fix_only_two_imports=True,private_target=context['target'],private_lease=context['lease'],no_execution=True))
write(V/'READBACK.json',dict(source_target=rec(V/'TARGET.json'),full_patch=rec(V/'SOURCE.patch'),delta=rec(V/'V2-FIX.patch'),caller_readback=rec(Q/'READBACK.json'),prior_failure_readback=rec(OLD/'final-v1/READBACK.json'),source_and_caller_preparation_only=True))
print(json.dumps(dict(source_target=rec(V/'TARGET.json'),source_patch=rec(V/'SOURCE.patch'),delta=rec(V/'V2-FIX.patch'),caller_target=rec(Q/'CALLER-TARGET.json'),caller_inputs=rec(Q/'INPUTS.json'),readback=rec(Q/'READBACK.json'),files=len(files),aliases=len(links))))
