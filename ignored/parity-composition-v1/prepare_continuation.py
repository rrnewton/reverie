from pathlib import Path
import ast,difflib,hashlib,json,os,shutil,stat,subprocess
N=Path(__file__).resolve().parent;O=N/'qualification-v1';Q=N/'qualification-v2';assert not Q.exists();Q.mkdir(mode=0o700)
def rec(p):
 p=Path(p);s=p.lstat();b=os.fsencode(os.readlink(p)) if p.is_symlink()else p.read_bytes();d={'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':stat.S_IMODE(s.st_mode)}
 if p.is_symlink():d.update(kind='symlink',target=os.readlink(p))
 return d
def write(p,d):
 with p.open('x')as f:f.write(d if isinstance(d,str) else json.dumps(d,indent=2)+'\n')
oldplan=json.loads((O/'metadata-plan.json').read_text());oldresult=json.loads((O/'controls/metadata/result.json').read_text());observer=json.loads((O/'observer/metadata/result.json').read_text())
assert oldresult['accepted'] is False and oldresult['raw_status']==137 and observer['accounting_complete'] is False and observer['final_accounting'] is None
assert 'RefUnit' in observer['observer_error'] and 'TimeoutError' in observer['observer_error']
unit='safehermit-20260918T200250Z-2374808.service';argv=['systemctl','--user','show',unit,'--property=Id,LoadState,ActiveState,SubState,MainPID,ControlGroup,ExecMainPID,ExecMainStatus,Result,CPUUsageNSec,MemoryCurrent,TasksCurrent']
p=subprocess.run(argv,capture_output=True,text=True,timeout=10);assert p.returncode==0 and not p.stderr
properties=dict(x.split('=',1)for x in p.stdout.splitlines()if '='in x)
assert properties['LoadState']=='not-found' and properties['ActiveState']=='inactive' and properties['MainPID']=='0' and properties['ControlGroup']==''
pids=[2374746,2374808,2374834,2375985];assert all(not Path('/proc/'+str(x)).exists() for x in pids)
assert not list(Path(oldplan['target']['path']).iterdir())
oldtoken=Path(oldplan['lease']['path']).read_bytes();assert json.loads(oldtoken)['plan_sha256']==hashlib.sha256((O/'metadata-plan.json').read_bytes()).hexdigest()
assert not list(Path(oldplan['lease']['state_directory']).iterdir())
proof={'unit_query':{'argv':argv,'raw_status':p.returncode,'stdout':p.stdout,'stderr':p.stderr},'processes':[{'pid':x,'exists':False}for x in pids],'old_result':rec(O/'controls/metadata/result.json'),'observer_result':rec(O/'observer/metadata/result.json'),'old_plan':rec(O/'metadata-plan.json'),'old_lease_token_hex':oldtoken.hex(),'old_state_records':[],'target_still_empty':True,'accounting_remains_incomplete':True,'old_receipt_repaired':False}
write(Q/'FIRST-REFUSAL.json',proof)
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','retain_artifacts.py','toolchain-standard-inputs.json','source-manifest.json','SETUP.json','SELECTORS.json','PREDECESSOR-ELFS.json','TARGET-ADMISSION.json']:
 shutil.copyfile(O/name,Q/name)
(Q/'observer').mkdir()
for p in (O/'observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
old=(Q/'prepare.py').read_text();new=old.replace("lease=HERE.parent/'lane.lease'","lease=HERE/'lane.lease'").replace("state_directory=str(HERE.parent/'lease-state')","state_directory=str(HERE/'lease-state')")
assert old!=new;ast.parse(new);(Q/'prepare.py').write_text(new)
write(Q/'CALLER-DELTA.patch',''.join(difflib.unified_diff(old.splitlines(True),new.splitlines(True),fromfile='v1/prepare.py',tofile='v2/prepare.py')))
write(Q/'RUNNER_ORIGINS.json',{'original':str(O),'records':[{'before':rec(O/name),'after':rec(Q/name),'byte_identical':(O/name).read_bytes()==(Q/name).read_bytes()}for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','retain_artifacts.py']],'reason':'Preserve original incomplete metadata attempt; fresh private lease/output paths, unchanged source/observer/bounds/selection/target'})
for name in ['tmp','cache','controls','loader','launch','lease-state']:(Q/name).mkdir(mode=0o700)
fd=os.open(Q/'lane.lease',os.O_CREAT|os.O_EXCL|os.O_RDWR|os.O_CLOEXEC,0o600);os.close(fd)
rows=json.loads((O/'CALLER-INPUTS.json').read_text())['records']
for r in rows:assert rec(r['path'])==r
rows += [rec(p)for p in Q.iterdir()if p.is_file() and p.name!='lane.lease']
rows += [rec(p)for p in (Q/'observer').iterdir()if p.is_file()]
write(Q/'CALLER-INPUTS.json',{'records':list({r['path']:r for r in rows}.values()),'old_lease_excluded_not_completed':True})
write(Q/'CONTINUATION.json',{'old_plan':str(O/'metadata-plan.json'),'old_plan_sha256':rec(O/'metadata-plan.json')['sha256'],'proof':rec(Q/'FIRST-REFUSAL.json'),'target':oldplan['target'],'new_lease':str(Q/'lane.lease'),'source_unchanged':rec(O/'source-manifest.json'),'metadata_must_qualify_before_compile':True,'sequence':json.loads((N/'RUN-ORDER.json').read_text())['phases']})
print(json.dumps({'source_unchanged':True,'refusal_preserved':rec(Q/'FIRST-REFUSAL.json'),'delta':rec(Q/'CALLER-DELTA.patch'),'next':'bounded metadata only under old-lease exclusion','product_tests_run':0},indent=2))
