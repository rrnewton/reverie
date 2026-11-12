"""Prepare text/identity bindings only. No admission, lease, Cargo or ELF execution."""
from pathlib import Path
import ast,difflib,hashlib,json,os,shutil,sys
Q=Path(__file__).resolve().parent;N=Q.parent;R=N.parents[1];P=N.parent/'rdtsc-recovery-source-v6';O=P/'qualification-v1';S=N/'source'
def rec(p):
 p=Path(p);st=p.lstat();b=os.fsencode(os.readlink(p)) if p.is_symlink() else p.read_bytes();d={'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':st.st_mode&0o7777}
 if p.is_symlink():d.update(kind='symlink',target=os.readlink(p))
 return d
def write(p,v):
 with Path(p).open('x')as f:f.write(v if isinstance(v,str)else json.dumps(v,indent=2)+'\n')
def diff(a,b,name):return ''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='V6/'+name,tofile='CPUID-V1/'+name))
files=['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','toolchain-standard-inputs.json']
for name in files:shutil.copyfile(O/name,Q/name)
(Q/'observer').mkdir()
for p in (O/'observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
shutil.copyfile(N/'SELECTORS.json',Q/'SELECTORS.json')
changes='';target_name='cpuid-recovery-v1-cold-v1'
setup=json.loads((O/'SETUP.json').read_text());oldpaths=[x['relative']for x in setup['source_files']]
newpaths=oldpaths+['reverie-kvm/src/cpuid_instruction.rs','reverie-kvm/src/cpuid_runtime_tests.rs','reverie-kvm/tests/support/cpuid_dispatch.rs','reverie-kvm/tests/support/cpuid_terminal.rs']
assert len(newpaths)==len(set(newpaths))==13
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;old=p.read_text();assert old.count('rdtsc-recovery-v6-cold-v1')==count;now=old.replace('rdtsc-recovery-v6-cold-v1',target_name)
 if name=='prepare.py':
  original='        paths='+repr(oldpaths);assert original in now;now=now.replace(original,'        paths='+repr(newpaths))
  original='CPL3 timestamp recovery: exact 37 declarations and existing public vmcall/clock neighbors; no public real-mode interception, timer completion or 75-cell parity claim from component phases'
  replacement='CPL3 CPUID Tool dispatch on frozen timestamp V6: exact 55 declarations; unchanged four-artifact clock/vmcall neighbors; no Detcore cancellation, timer or full parity claim'
  assert original in now;now=now.replace(original,replacement)
 p.write_text(now);changes+=diff(old,now,'qualification-v1/'+name)
setup.update(source_root=str(S),source_files=[{'relative':x,'file':rec(S/x)}for x in newpaths],lock=rec(S/'Cargo.lock'),target=str(R/'target'/target_name),purpose='CPUID source V1 only, based on frozen V6; no V7 test composition, qualification or parity claim from preparation',source_target=rec(N/'TARGET.json'))
write(Q/'SETUP.json',setup);changes+=diff((O/'SETUP.json').read_text(),(Q/'SETUP.json').read_text(),'qualification-v1/SETUP.json')
manifest=json.loads((N/'SOURCE-MANIFEST.json').read_text());phase_manifest=[]
for row in manifest:
 kind=row['kind'];mode='160000' if kind=='unexpanded_gitlink' else '120000' if kind=='symlink' else '100755' if row['mode']&0o111 else '100644'
 entry={'path':row['relative'],'mode':mode}
 if 'sha256'in row:entry['sha256']=row['sha256']
 phase_manifest.append(entry)
write(Q/'source-manifest.json',phase_manifest)
artifacts=json.loads((O/'retained-binaries/ARTIFACTS.json').read_text());hashes={k:v['file']['sha256']for k,v in artifacts.items()}
for v in artifacts.values():assert rec(v['file']['path'])==v['file']
write(Q/'PREDECESSOR-ELFS.json',{'source':str(O/'retained-binaries/BINDING.json'),'sha256':rec(O/'retained-binaries/BINDING.json')['sha256'],'hashes':hashes,'purpose':'Refuse all four actual V6 harness bytes as newly built CPUID candidate evidence; no historical execution credit'})
changes+=diff((O/'PREDECESSOR-ELFS.json').read_text(),(Q/'PREDECESSOR-ELFS.json').read_text(),'qualification-v1/PREDECESSOR-ELFS.json')
changes+=diff((O/'SELECTORS.json').read_text(),(Q/'SELECTORS.json').read_text(),'qualification-v1/SELECTORS.json')
oldorder=json.loads((P/'RUN-ORDER.json').read_text());newtests=[k for k in json.loads((N/'SELECTORS.json').read_text())['groups']if k.startswith('cpuid-')and not k.startswith('cpuid-neighbor-')]
oldtests=[k for k in oldorder['phases']if k.startswith('timestamp-')]
assert len(newtests)==15 and len(oldtests)==37
order={'phases':['metadata','compile','format','list-lib','list-static','list-vmcall','list-read-clock',*newtests,*oldtests,'cpuid-neighbor-01','cpuid-neighbor-02','cpuid-neighbor-03','core-check','clippy'],'metadata_followup':'bind_dependencies.py','compile_followup':'retain_artifacts.py','no_extra_retry_or_skipped_phase':True}
assert len(order['phases'])==len(set(order['phases']))==64
write(N/'RUN-ORDER.json',order);changes+=diff((P/'RUN-ORDER.json').read_text(),(N/'RUN-ORDER.json').read_text(),'RUN-ORDER.json')
for name in ['execute_phases.py','finalize_results.py']:
 old=(P/name).read_text();assert old.count("Q=N/'qualification-v1'")==1
 now=old.replace("Q=N/'qualification-v1'","Q=N/'qualification-preparation-v1'")
 if name=='finalize_results.py':
  assert "json.loads((N/'INPUTS.json').read_text())['records']"in now
  now=now.replace("json.loads((N/'INPUTS.json').read_text())['records']","json.loads((Q/'CALLER-INPUTS.json').read_text())['records']").replace('Actual V6 source','Actual CPUID V1 source')
 write(N/name,now);changes+=diff(old,now,name)
# Strict syntax check of authored text; do not import or execute the phase helpers.
origins=[]
for p in sorted(Q.rglob('*.py')):
 ast.parse(p.read_text());old=O/p.relative_to(Q)
 if old.exists():origins.append({'before':rec(old),'after':rec(p),'byte_identical':old.read_bytes()==p.read_bytes()})
for name in ['execute_phases.py','finalize_results.py']:
 ast.parse((N/name).read_text());origins.append({'before':rec(P/name),'after':rec(N/name),'byte_identical':(P/name).read_bytes()==(N/name).read_bytes()})
write(Q/'RUNNER_ORIGINS.json',{'records':origins,'material_delta':'new target spelling; four added format paths while retaining all nine old ones; exact 55 selector/scope/source bindings; V6 predecessor ELF identities; caller directory for outer orchestration/results and its normalized input index; limits/observer/phase/common/lease/dependency/ELF retention algorithms unchanged'})
write(Q/'CALLER-DELTA.patch',changes)
# Actual current configuration spellings and planned target absence. No target or lease opened.
configs=[];absent=[]
for parent in [S,*S.parents,Path('/home/newton')]:
 for rel in ['.cargo/config','.cargo/config.toml']:
  p=parent/rel
  if p.is_file():configs.append(rec(p.resolve()))
  else:absent.append(str(p))
target=R/'target'/target_name;assert not target.exists()and not target.is_symlink()
write(Q/'PREPARATION-STATE.json',{'python_optimize':sys.flags.optimize,'python_debug':__debug__,'source':rec(N/'TARGET.json'),'target':str(target),'target_exists':False,'target_admission_executed':False,'lease_opened':False,'phase_plan_created':False,'cargo_or_test_executed':False,'configs':configs,'absent_configs':sorted(set(absent)),'future_policy':'admit_target.py must create exactly this absent target once, owned 0700 and empty, then prepare actual plans with fresh tool/loader/source/lease identity; no target/cache import'})
# Normalize byte records to the established caller schema. Gitlink declarations
# remain in source-manifest, never masquerade as expanded dependency source.
records={}
def add(p):
 p=Path(p)
 if p.is_file() or p.is_symlink():
  row=rec(p);records[row['path']]=row
for row in json.loads((N/'INPUTS.json').read_text())['records']:
 if row['kind']=='unexpanded_gitlink':continue
 p=Path(row['path']);now=rec(p);assert now['sha256']==row['sha256'];add(p)
for row in json.loads((Q/'toolchain-standard-inputs.json').read_text())['files']:
 assert rec(row['path'])==row;add(row['path'])
for row in origins:add(row['before']['path']);add(row['after']['path'])
for v in artifacts.values():add(v['file']['path'])
for p in [O/'retained-binaries/ARTIFACTS.json',O/'retained-binaries/BINDING.json',N/'TARGET.json',N/'READBACK.json',N/'INPUTS.json',N/'RUN-ORDER.json',N/'execute_phases.py',N/'finalize_results.py']:
 add(p)
for p in Q.rglob('*'):add(p)
for row in configs:add(row['path'])
write(Q/'CALLER-INPUTS.json',{'records':list(records.values()),'source_entries':len(manifest),'gitlinks':'three declared unexpanded empty directories; no SDK execution claim','metadata_dependent_external_closure':'must be produced from actual accepted metadata before compile; no old resolution borrowed'})
for row in records.values():assert rec(row['path'])==row
print(json.dumps({'records':len(records),'source_entries':len(manifest),'planned_phases':64,'declarations':55,'delta':rec(Q/'CALLER-DELTA.patch'),'source_target_unchanged':rec(N/'TARGET.json'),'no_execution':True},indent=2))
