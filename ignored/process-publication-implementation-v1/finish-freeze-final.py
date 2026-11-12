from pathlib import Path
import json,hashlib,subprocess,shutil,os,stat,fcntl
R=Path(__file__).resolve().parents[2];D=Path(__file__).resolve().parent;Q=D/'qualification-v2';F=D/'frozen-v1'
def rec(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
def write(p,v):
 with p.open('x') as f:f.write(json.dumps(v,indent=2)+'\n')
def sha(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()

source=json.loads((Q/'source-manifest.json').read_text());full=json.loads((F/'FULL-SOURCE-MANIFEST.json').read_text())
base=subprocess.check_output(['git','rev-parse','HEAD'],cwd=R,timeout=30).decode().strip()
branch=subprocess.check_output(['git','branch','--show-current'],cwd=R,timeout=30).decode().strip();index=subprocess.check_output(['git','ls-files','--stage'],cwd=R,timeout=30)
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs'];check=F/'patch-reconstruction'
assert all((check/p).read_bytes()==(F/'source'/p).read_bytes()for p in paths)
write(F/'SOURCE_INPUTS.json',{'base':base,'branch':branch,'index_sha256':hashlib.sha256(index).hexdigest(),'full_source':rec(F/'FULL-SOURCE-MANIFEST.json'),'patch':rec(F/'SOURCE.patch'),'new_product_file':paths[-1],'changed':[rec(F/'source'/p)for p in paths],'local_ignored_lock':rec(F/'source/Cargo.lock'),'exact_patch_reconstruction':True,'no_SCM_mutation':True})
selection=json.loads((Q/'SELECTORS.json').read_text())['groups'];names=list(selection);phases=['metadata','compile','format','list-lib','list-static',*names,'core-check','clippy'];assert len(phases)==66
results=[]
for name in phases:
 p=Q/(name+'-plan.json');control=Q/'controls'/name/'result.json';out=Q/'observer'/name;r=json.loads(control.read_text());o=json.loads((out/'result.json').read_text());plan=json.loads(p.read_text())
 assert r['accepted'] and r['terminal_authenticated'] and r['inputs_unchanged'] and r['raw_status']==0 and r['plan_sha256']==sha(p)
 assert o['accounting_complete'] and o['final_accounting']['cgroup_empty']
 assert plan['limits']=={'aggregate_cpu_usec':600000000 if name in ['metadata','compile','core-check','clippy']else 30000000,'wall_seconds':900 if name in ['metadata','compile','core-check','clippy']else 60,'lethal_stderr_bytes':16777216,'live_stdout_samples_bytes':67108864,'phase_read_bytes':16777216,'memory_bytes':17179869184,'swap_bytes':0,'free_floor_bytes':107374182400}
 if name in selection:assert r['readback']['executed']==1 and r['readback']['names']==selection[name]['names']
 results.append({'name':name,'plan':rec(p),'result':rec(control),'observer':rec(out/'result.json'),'stdout':rec(out/'stdout'),'stderr':rec(out/'stderr'),'accepted':True,'raw_status':0,'cpu_seconds':o['final_accounting']['cpu_usage_nsec']/1e9,'observed_wall_seconds':o['elapsed_seconds'],'payload_seconds':r['payload_exit']['elapsed_seconds'],'readback':r['readback']})
first=D/'qualification-v1';refusal=json.loads((first/'controls/compile/result.json').read_text());assert refusal['raw_status']==0 and refusal['terminal_authenticated'] and refusal['inputs_unchanged'] and not refusal['accepted']
oldobs=json.loads((first/'observer/compile/result.json').read_text())
write(F/'RESULTS.json',{'qualification_directory':str(Q),'source':rec(F/'SOURCE_INPUTS.json'),'phases':results,'phase_count':len(results),'passed_declarations':59,'lib_declarations':50,'static_declarations':9,'new_declarations':7,'inventories':{'lib':495,'static':304},'total_cpu_seconds':sum(x['cpu_seconds']for x in results),'total_observed_wall_seconds':sum(x['observed_wall_seconds']for x in results),'first_attempt':{'qualified_metadata':rec(first/'controls/metadata/result.json'),'unqualified_compile':rec(first/'controls/compile/result.json'),'diagnostics':rec(first/'controls/compile/compiler-diagnostics.json'),'retained_elfs':rec(first/'retained-unqualified-binaries/BINDING.json'),'raw_status':0,'accepted':False,'cpu_seconds':oldobs['final_accounting']['cpu_usage_nsec']/1e9,'observed_wall_seconds':oldobs['elapsed_seconds'],'cause':'Two superseded private wrappers emitted dead_code warnings in both library builds; four diagnostics total. Removed the unused wrappers; no lint suppression or oracle change.'},'qualified_elfs':rec(Q/'retained-binaries/BINDING.json')})
lease=R/'ignored/timer-integration-20260918/lane.lease';fd=os.open(lease,os.O_RDWR|os.O_CLOEXEC);fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);s=os.fstat(fd);fcntl.flock(fd,fcntl.LOCK_UN);os.close(fd)
write(F/'LEASE-RELEASE.json',{'path':str(lease),'identity':[s.st_dev,s.st_ino,s.st_uid],'exclusive_lock_available':True,'check_descriptor_closed':True,'phase_owned_descriptors_already_closed_by_unchanged_caller':True,'last_completed_result':rec(Q/'controls/clippy/result.json'),'no_lease_state_mutation':True,'source_and_SCM_remain_frozen':True})
write(F/'CALLER_INPUTS.json',{'approval_report':rec(Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/process-publication-caller-review-v1/REPORT.md')),'original_plan':rec(D/'prequalification-v2/TEST_PLAN.md'),'caller_delta':rec(D/'prequalification-v2/CALLER.patch'),'successor_origin':rec(Q/'RUNNER_ORIGINS.json'),'source_manifest':rec(Q/'source-manifest.json'),'selection':rec(Q/'SELECTORS.json'),'actual_dependency_closure':rec(Q/'dependency-closure.json'),'actual_dependency_files':rec(Q/'dependency-inputs.json'),'actual_dependency_symlinks':rec(Q/'dependency-symlinks.json'),'recipe':rec(Q/'bind_dependencies.py'),'recipes_original_binding':rec(first/'dependency-recipe.json'),'plan_protocol_changes_after_approval':False})
print(json.dumps({'packet':str(F),'phases':len(results),'total_cpu_s':sum(x['cpu_seconds']for x in results),'total_observed_wall_s':sum(x['observed_wall_seconds']for x in results),'source_entries':len(full),'source_patch':sha(F/'SOURCE.patch'),'lease_released':True},indent=2))
