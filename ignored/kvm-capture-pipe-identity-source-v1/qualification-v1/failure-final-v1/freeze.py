#!/usr/bin/python3
"""Read-only terminal/evidence audit; append failure packet, never change source or lease token."""
import fcntl,hashlib,json,os,stat,sys
from pathlib import Path
F=Path(__file__).resolve().parent
Q=F.parent
sys.path.insert(0,str(Q))
from common import bounded_process,check_file,digest,file_record,json_read,read,require,write_new
import cache_lease
import phase
require(sys.flags.optimize==0,'explicit checks require ordinary execution')
seq=json_read(Q/'EXECUTION-RESULT.json')
require(seq['completed']==['metadata'] and seq['error'],'unexpected execution history')
require(not (Q/'retained-binaries').exists(),'unexpected retained binaries after compile failure')
rows=[]
for name in ['metadata','compile']:
    plan=json_read(Q/(name+'-plan.json'));receipt=json_read(Q/'controls'/name/'result.json')
    require(digest(Q/(name+'-plan.json'))==receipt['plan_sha256'],'plan mismatch')
    require(receipt['terminal_authenticated'] and receipt['inputs_unchanged'],'phase terminal/source missing')
    phase.check(plan)
    observed=json_read(Q/'observer'/name/'result.json')
    require(observed['accounting_complete'] and not observed['observer_error'] and not observed['stop_reason'],'observer qualification missing')
    final=observed['final_accounting'];p=final['properties']
    require(final['cgroup_empty'] and p['MainPID']==0 and p['ControlGroup']=='' and p['ActiveState'] in ['inactive','failed'],'service not empty')
    rows.append(dict(name=name,accepted=receipt['accepted'],raw_status=receipt['raw_status'],plan=file_record(Q/(name+'-plan.json')),receipt=file_record(Q/'controls'/name/'result.json'),observer=file_record(Q/'observer'/name/'result.json'),stdout=file_record(Q/'observer'/name/'stdout'),stderr=file_record(Q/'observer'/name/'stderr'),cpu_seconds=final['cpu_usage_nsec']/1e9,observer_wall_seconds=observed['elapsed_seconds'],payload_wall_seconds=receipt['payload_exit']['elapsed_seconds']))
require(rows[0]['accepted'] and rows[0]['raw_status']==0 and not rows[1]['accepted'] and rows[1]['raw_status']==101,'wrong preserved failure')
raw=[json.loads(line)for line in read(Q/'observer/compile/stdout').splitlines()]
diagnostics=[r for r in raw if r.get('reason')=='compiler-message']
require([r['message']['code']['code'] for r in diagnostics if r['message'].get('code')]==['E0046','E0308'],'compiler error set differs')
write_new(F/'COMPILER-DIAGNOSTICS.json',dict(raw_stdout=file_record(Q/'observer/compile/stdout'),messages=diagnostics,build_finished=[r for r in raw if r.get('reason')=='build-finished']))
(F/'COMPILER-DIAGNOSTICS.txt').write_text(''.join(r['message'].get('rendered','')for r in diagnostics))
last=json_read(Q/'compile-plan.json');binding=last['lease'];sha=digest(Q/'compile-plan.json')
fd=cache_lease.open_bound(binding)
try:
    fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
    cache_lease.check_held(fd,binding,sha)
    token=os.pread(fd,4096,0)
    completion_path=Q/'lease-state'/(sha+'.json');completion=json_read(completion_path)
    require(completion['plan_sha256']==sha,'completion mismatch');check_file(completion['result'])
    queries=[]
    for i,name in enumerate(['metadata','compile']):
        observed=json_read(Q/'observer'/name/'result.json');unit=observed['final_accounting']['properties']['Id']
        out=bounded_process(['/usr/bin/systemctl','--user','show',unit,'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'],Q,last['environment'],F/('terminal-'+name),5,8192)
        require(out['returncode']==0 and not out['forced'],'fresh terminal query refused')
        values=dict(line.split('=',1)for line in read(out['stdout']['path'],8192).decode().splitlines()if '='in line)
        require(values.get('ActiveState')in ['inactive','failed']and values.get('MainPID')=='0'and values.get('ControlGroup')=='','fresh service not terminal')
        queries.append(dict(unit=unit,query=out,properties=values))
    phase.scm(last,F/'SCM-FINAL.json')
    cache_lease.check_held(fd,binding,sha)
    proof=dict(binding=binding,exclusive_nonblocking_acquired=True,last_token=token.decode(),token_sha256=hashlib.sha256(token).hexdigest(),completion=file_record(completion_path),result=completion['result'],terminal_queries=queries,no_token_rewrite=True,no_recovery=True)
finally:
    os.close(fd)
proof['exclusive_hold_closed']=True
write_new(F/'LEASE-RELEASE.json',proof)
result=dict(status='STOPPED AT FIRST COMPILE FAILURE',caller=file_record(Q/'CALLER-TARGET.json'),source_target=file_record(Q.parent/'TARGET.json'),source_patch=file_record(Q.parent/'SOURCE.patch'),planned_phases=72,attempted_phases=2,accepted_phases=1,failed_phases=1,unrun_phases=70,planned_exact_tests=63,executed_exact_tests=0,passed_exact_tests=0,unrun_exact_tests=63,retries=0,qualified_test_elf=False,raw_child_markers='UNRUN: compile failed before all child/EMFILE controls',cpu_seconds=sum(r['cpu_seconds']for r in rows),observer_wall_seconds=sum(r['observer_wall_seconds']for r in rows),payload_wall_seconds=sum(r['payload_wall_seconds']for r in rows),phases=rows,remaining_sequence=seq['sequence'][2:])
write_new(F/'RESULTS.json',result)
(F/'REPORT.md').write_text('''# Captured pipe identity: first qualification failure

Metadata passed; the fresh compile failed raw 101. The caller stopped without retry. Two phases ran, one passed and one failed. All 63 tests and the seven remaining non-test phases are unrun. No test ELF was qualified or retained, no isolated child completion marker exists, and no runtime/metadata/parity claim follows from the partial compile.

The new GlobalTool implementation in capture_identity_tests.rs:362 omits required receive_rpc (E0046). An existing executor.rs:39601 readlink_at_impl call still passes false after its parameter changed to Option<CaptureMetadata> (E0308). The complete compiler JSON, rendered diagnostics and unsuccessful Cargo completion remain bound. No assertion or compiler gate was relaxed.

The compile payload ran 41.397573 seconds, observer 44.906687 seconds, and used 86.086099 CPU seconds. Including metadata, the two observed phases used 87.699640 CPU seconds and 47.584012 summed observer-wall seconds. All original 600 CPU / 900 wall compile/metadata allowances, output/memory/free-space guards, source bytes, caller bytes and SCM identity remain unchanged.

The private lease last token names this actual failed compile and its authenticated append-only terminal completion. A fresh nonblocking exclusive lock succeeded; both retained service units were independently queried inactive/empty with MainPID zero. The audit closed its hold without rewriting the token or performing recovery. This is terminal lifecycle evidence for a real failure, not successful build accounting.

The source carrier remains the complete frozen B source plus the identical V6 lock. The original source/report/caller, prior missing-lock and static-freeze refusals, stdout/pipe witness, old V6 evidence and teardown hangs remain unchanged. The existing private build target is retained as failed-build residue, not indexed as immutable runtime evidence. Raw Cargo artifact paths are historical output records; none were executed or credited. A separately frozen source successor and caller authorization are required before another qualification attempt.
''')
# Final complete immutable closure. Raw target/lease paths are metadata, never held as immutable artifacts.
files={};links={};special=[]
def add(p):
    p=Path(p);info=p.lstat()
    if stat.S_ISREG(info.st_mode):files[str(p)]=file_record(p)
    elif stat.S_ISLNK(info.st_mode):links[str(p)]=dict(path=str(p),target=os.readlink(p))
    elif stat.S_ISSOCK(info.st_mode):special.append(dict(path=str(p),kind='completed_observer_socket_metadata_only',device=info.st_dev,inode=info.st_ino,uid=info.st_uid,mode=stat.S_IMODE(info.st_mode)))
    else:require(stat.S_ISDIR(info.st_mode),'unexpected output type')
held=json_read(Q/'CALLER-INPUTS.json')
for r in held['files']:check_file(r);files[r['path']]=r
for r in held['symlinks']:
    require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'held alias drift');links[r['path']]=r
for r in json_read(Q/'dependency-inputs.json'):check_file(r);files[r['path']]=r
for r in json_read(Q/'dependency-symlinks.json'):
    require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'dependency alias drift');links[r['path']]=r
for p in Q.iterdir():
    if p.is_file()and p.name!='lane.lease':add(p)
for root in ['observer','controls','loader','launch','lease-state']:
    for p in sorted((Q/root).rglob('*')):add(p)
for p in F.iterdir():add(p)
write_new(F/'INPUTS.json',dict(files=sorted(files.values(),key=lambda r:r['path']),symlinks=sorted(links.values(),key=lambda r:r['path']),socket_metadata=special,unexpanded_gitlinks=held['unexpanded_gitlinks'],excluded_mutable=['private target','private tmp/cache','raw private lease inode; exact token and identity bound in LEASE-RELEASE']))
for r in files.values():check_file(r)
for r in links.values():require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'final alias drift')
write_new(F/'READBACK.json',dict(results=file_record(F/'RESULTS.json'),report=file_record(F/'REPORT.md'),inputs=file_record(F/'INPUTS.json'),lease_release=file_record(F/'LEASE-RELEASE.json'),regular_files=len(files),symlinks=len(links),completed_socket_metadata=len(special),all_indexed_bytes_modes_unchanged=True,all63tests_unrun=True,qualification=False,source_product_mutations=False))
print(json.dumps({n:file_record(F/n)for n in ['REPORT.md','RESULTS.json','INPUTS.json','READBACK.json','LEASE-RELEASE.json']},indent=2))
