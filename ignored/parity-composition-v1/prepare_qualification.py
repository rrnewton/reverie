from pathlib import Path
import ast,difflib,hashlib,json,os,shutil,stat,subprocess
N=Path(__file__).resolve().parent;L=N.parents[1];S=N/'source';Q=N/'qualification-v1'
R=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918/ignored')
C=R/'cpuid-recovery-source-v1';O=C/'qualification-preparation-v1'
CAP=R/'kvm-capture-pipe-identity-source-v2'
assert not Q.exists();Q.mkdir()
def rec(p):
 p=Path(p);s=p.lstat();data=os.fsencode(os.readlink(p)) if p.is_symlink() else p.read_bytes()
 d={'path':str(p),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'mode':stat.S_IMODE(s.st_mode)}
 if p.is_symlink():d.update(kind='symlink',target=os.readlink(p))
 return d
def write(p,d):
 with p.open('x')as f:f.write(d if isinstance(d,str) else json.dumps(d,indent=2)+'\n')
manifest=[];aliases=[];records=[]
oldmanifest=json.loads((O/'source-manifest.json').read_text());gitlinks=[x for x in oldmanifest if x['mode']=='160000']
for directory,dirs,files in os.walk(S,followlinks=False):
 for name in sorted(dirs+files):
  p=Path(directory)/name
  if p.is_symlink() or p.is_file():
   r=rec(p);records.append(r);row={'path':str(p.relative_to(S)),'mode':'120000' if p.is_symlink() else '100755' if p.stat().st_mode&0o111 else '100644','sha256':r['sha256']};manifest.append(row)
   if p.is_symlink():aliases.append({'path':str(p),'target':os.readlink(p)})
for row in gitlinks:
 p=S/row['path'];assert p.is_dir() and not p.is_symlink() and not list(p.iterdir());manifest.append(row)
write(N/'SOURCE-MANIFEST.json',sorted(manifest,key=lambda x:x['path']))
write(N/'SOURCE-TARGET.json',{'base':'68569ed7fd5c3667456d1d3e0d74ba66264242f4','tree':'8e9a1d6b22b421992508e26855a568314a12bddf','source':str(S),'manifest':rec(N/'SOURCE-MANIFEST.json'),'linear_source_plan':rec(N/'LINEAR-SOURCE-PLAN.json'),'patches':[rec(N/'01-cpuid-on-v7.patch'),rec(N/'02-capture-after-cpuid.patch')],'qualified':False,'source_reviewed':False})
# Union exact selectors by artifact and full declaration; retain every predecessor assertion.
groups={};seen={}
for label,p in [('cpuid',O/'SELECTORS.json'),('capture',CAP/'SELECTORS.json')]:
 for oldkey,v in json.loads(p.read_text())['groups'].items():
  assert len(v['names'])==1
  key=(v['artifact'],v['names'][0]);origin={'selector_file':rec(p),'group':oldkey}
  if key in seen:groups[seen[key]]['predecessors'].append(origin);continue
  name=f'control-{len(groups)+1:02d}';seen[key]=name;groups[name]={**v,'origin':'Prepared composition; predecessor executions are not composition results','predecessors':[origin]}
for art,name,source in [('lib','runtime::terminal_tests::timestamp_exit_admission_preserves_returning_ordinary_injections','reverie-kvm/src/terminal_runtime_tests.rs'),('static','timestamp_terminal::worker_timestamp_exit_group_terminates_leader_and_runs_hooks_once','reverie-kvm/tests/support/timestamp_terminal.rs')]:
 assert (art,name) not in seen;groups[f'control-{len(groups)+1:02d}']={'artifact':art,'names':[name],'source':source,'origin':'Additive V7 timestamp control retained in composition','predecessors':[]}
assert len(groups)==83
write(Q/'SELECTORS.json',{'groups':groups,'declarations':83,'deduplicated_predecessor_union':81,'additive_v7':2,'execution':False})
files=['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','toolchain-standard-inputs.json']
for name in files:shutil.copyfile(O/name,Q/name)
(Q/'observer').mkdir()
for p in (O/'observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
oldsetup=json.loads((O/'SETUP.json').read_text());paths=[x['relative'] for x in oldsetup['source_files']]
paths += [p for p in json.loads((N/'LINEAR-SOURCE-PLAN.json').read_text())['second_commit_paths'] if p not in paths]
assert len(paths)==17 and len(set(paths))==17
scope='Combined V7 timestamp, CPUID and captured-pipe identity source: exact 83 declarations; no Hermit cancellation, signal/timer or full backend parity claim from component tests'
setup={'owner_slot':str(L),'source_root':str(S),'base':'68569ed7fd5c3667456d1d3e0d74ba66264242f4','base_tree':'8e9a1d6b22b421992508e26855a568314a12bddf','source_files':[{'relative':p,'file':rec(S/p)} for p in paths],'lock':rec(S/'Cargo.lock'),'target':str(L/'target/parity-composition-v1-cold-v1'),'purpose':scope,'source_target':rec(N/'SOURCE-TARGET.json')}
write(Q/'SETUP.json',setup);shutil.copyfile(N/'SOURCE-MANIFEST.json',Q/'source-manifest.json')
# Adapt path/selector binding only. Existing observer, phase, lease and comparator remain unchanged.
p=Q/'prepare.py';text=p.read_text();assert text.count('cpuid-recovery-v1-cold-v1')==2;text=text.replace('cpuid-recovery-v1-cold-v1','parity-composition-v1-cold-v1')
oldpaths=[x['relative']for x in oldsetup['source_files']];assert '        paths='+repr(oldpaths) in text;text=text.replace('        paths='+repr(oldpaths),'        paths='+repr(paths))
text=text.replace("OWNER/'ignored/timer-integration-20260918/lane.lease'","HERE.parent/'lane.lease'").replace("OWNER/'ignored/timer-integration-20260918/lease-state'","HERE.parent/'lease-state'")
oldscope='CPL3 CPUID Tool dispatch on frozen timestamp V6: exact 55 declarations; unchanged four-artifact clock/vmcall neighbors; no Detcore cancellation, timer or full parity claim';assert oldscope in text;text=text.replace(oldscope,scope)
p.write_text(text)
p=Q/'admit_target.py';text=p.read_text().replace('cpuid-recovery-v1-cold-v1','parity-composition-v1-cold-v1')
text=text.replace("require(target.parent.is_dir() and not target.parent.is_symlink(),'target parent absent or aliased')","if not target.parent.exists():target.parent.mkdir(mode=0o700)\nrequire(target.parent.is_dir() and not target.parent.is_symlink(),'target parent absent or aliased')\nlease=HERE.parent/'lane.lease';state=HERE.parent/'lease-state'\nrequire(not lease.exists() and not lease.is_symlink() and not state.exists(),'private lease destination exists')\nfd=os.open(lease,os.O_CREAT|os.O_EXCL|os.O_RDWR|os.O_CLOEXEC,0o600);os.close(fd);state.mkdir(mode=0o700)")
p.write_text(text)
oldart=json.loads((O/'retained-binaries/ARTIFACTS.json').read_text());write(Q/'PREDECESSOR-ELFS.json',{'source':str(O/'retained-binaries/BINDING.json'),'sha256':rec(O/'retained-binaries/BINDING.json')['sha256'],'hashes':{k:v['file']['sha256'] for k,v in oldart.items()},'purpose':'Require newly built composition harnesses distinct from qualified CPUID predecessor; no predecessor execution credit'})
order=['metadata','compile','format','list-lib','list-static','list-vmcall','list-read-clock',*groups,'core-check','clippy'];assert len(order)==92
write(N/'RUN-ORDER.json',{'phases':order,'metadata_followup':'bind_dependencies.py','compile_followup':'retain_artifacts.py','no_extra_retry_or_skipped_phase':True})
for name in ['execute_phases.py','finalize_results.py']:
 text=(C/name).read_text().replace("Q=N/'qualification-preparation-v1'","Q=N/'qualification-v1'").replace('Actual CPUID V1 source','Actual composed source')
 write(N/name,text)
origins=[];delta=''
for name in files:
 if name.endswith('.py'):
  a=(O/name).read_text();b=(Q/name).read_text();ast.parse(b);origins.append({'before':rec(O/name),'after':rec(Q/name),'byte_identical':a==b});delta+=''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='cpuid/'+name,tofile='composition/'+name))
for p in (Q/'observer').iterdir():
 if p.suffix=='.py':ast.parse(p.read_text());origins.append({'before':rec(O/'observer'/p.name),'after':rec(p),'byte_identical':(O/'observer'/p.name).read_bytes()==p.read_bytes()})
write(Q/'RUNNER_ORIGINS.json',{'records':origins,'bounds_unchanged':True,'source_and_selectors_are_composition':True})
write(Q/'CALLER-DELTA.patch',delta)
records += [rec(p) for p in Q.iterdir() if p.is_file()]
records += [rec(p) for p in (Q/'observer').iterdir() if p.is_file()]
records += [rec(p) for p in N.iterdir() if p.is_file()]
for row in json.loads((Q/'toolchain-standard-inputs.json').read_text())['files']:
 assert rec(row['path'])==row;records.append(row)
for row in origins:records.append(row['before'])
for v in oldart.values():assert rec(v['file']['path'])==v['file'];records.append(v['file'])
records.extend([rec(O/'retained-binaries/BINDING.json'),rec(O/'retained-binaries/ARTIFACTS.json')])
records=list({r['path']:r for r in records}.values())
write(Q/'CALLER-INPUTS.json',{'records':records,'symlinks':aliases,'source_entries':len(manifest),'gitlinks':[r['path']for r in gitlinks],'metadata_dependent_external_closure':'Require actual accepted offline locked metadata and binder before compile'})
write(Q/'CALLER-TARGET.json',{'source':rec(N/'SOURCE-TARGET.json'),'inputs':rec(Q/'CALLER-INPUTS.json'),'delta':rec(Q/'CALLER-DELTA.patch'),'order':rec(N/'RUN-ORDER.json'),'selectors':rec(Q/'SELECTORS.json'),'scope':scope,'phases':92,'declarations':83,'executed':False})
for row in records:assert rec(row['path'])==row
print(json.dumps({'target':rec(Q/'CALLER-TARGET.json'),'records':len(records),'aliases':len(aliases),'source_entries':len(manifest),'phases':92,'declarations':83,'executed':False},indent=2))
