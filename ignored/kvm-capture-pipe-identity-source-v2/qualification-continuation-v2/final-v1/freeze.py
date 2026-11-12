#!/usr/bin/python3
"""Freeze completed finite qualification without re-executing or repairing anything."""
import collections,fcntl,json,os,stat,sys
from pathlib import Path
F=Path(__file__).resolve().parent;N=F.parent
sys.path.insert(0,str(N))
from common import bounded_process,check_file,digest,file_record,json_read,read,require,write_new
import cache_lease,phase
Q=Path(json_read(N/'SETUP.json')['origin']);OLD=Q/'failure-final-v1';D=Q.parent
require(sys.flags.optimize==0,'ordinary Python required')
sequence=json_read(N/'EXECUTION-RESULT.json');selection=json_read(N/'SELECTORS.json');full=json_read(Q/'SELECTORS.json');prior=json_read(OLD/'RESULTS.json')
require(sequence['error'] is None and len(sequence['completed'])==55 and sequence['completed']==sequence['sequence'],'continuation did not complete')
require(sequence['sequence']==selection['proposed_order']+['core-check','clippy'],'continuation order differs')
require(prior['qualified_exact_tests']==10 and prior['refused_phases']==1,'prior history changed')
rows=[];phase_inputs={};phase_aliases={};phase_absent=set()
for name in sequence['sequence']:
 plan=json_read(N/(name+'-plan.json'));r=json_read(N/'controls'/name/'result.json');o=json_read(N/'observer'/name/'result.json')
 require(r['accepted'] and r['raw_status']==0 and r['terminal_authenticated'] and r['inputs_unchanged'],'phase not qualified: '+name)
 require(r['plan_sha256']==digest(N/(name+'-plan.json')),'plan hash differs')
 for record in [plan['source_manifest'],*plan['inputs'],*([plan['dependencies']]if plan.get('dependencies')else[])]:
  previous=phase_inputs.setdefault(record['path'],record);require(previous==record,'same path has conflicting phase identities')
 for link in plan.get('input_symlinks',[]):
  previous=phase_aliases.setdefault(link['path'],link);require(previous==link,'same alias has conflicting phase identities')
 phase_absent.update(plan['absent_inputs'])
 require(o['accounting_complete'] and not o['observer_error'] and not o['stop_reason'],'observer qualification missing')
 final=o['final_accounting'];p=final['properties'];require(final['cgroup_empty'] and p['MainPID']==0 and p['ControlGroup']=='' and p['ActiveState']in ['inactive','failed'],'nonterminal service')
 require(r['payload_exit']['reaped'] and r['payload_exit']['returncode']==0,'payload not reaped')
 row=dict(name=name,kind=plan['kind'],selected=plan['selected'],accepted=True,raw_status=0,plan=file_record(N/(name+'-plan.json')),receipt=file_record(N/'controls'/name/'result.json'),observer=file_record(N/'observer'/name/'result.json'),stdout=file_record(N/'observer'/name/'stdout'),stderr=file_record(N/'observer'/name/'stderr'),cpu_seconds=final['cpu_usage_nsec']/1e9,observer_wall_seconds=o['elapsed_seconds'],payload_wall_seconds=r['payload_exit']['elapsed_seconds'])
 raw=[json.loads(x)for x in read(N/'observer'/name/'stdout').splitlines()]
 if plan['kind']=='test':
  started=[x['name']for x in raw if x.get('type')=='test'and x.get('event')=='started'];ended=[x for x in raw if x.get('type')=='test'and x.get('event')in ['ok','failed','ignored']];suites=[x for x in raw if x.get('type')=='suite'and x.get('event')in ['ok','failed']]
  require(started==plan['selected'] and len(ended)==1 and ended[0]['name']==started[0] and ended[0]['event']=='ok','exact test outcome differs')
  require(len(suites)==1 and suites[0]['event']=='ok' and suites[0]['passed']==1 and suites[0]['failed']==suites[0]['ignored']==suites[0]['measured']==0,'suite denominator differs')
  row['raw_test_summary']=suites[0]
 else:
  require([x['success']for x in raw if x.get('reason')=='build-finished']==[True] and not [x for x in raw if x.get('reason')=='compiler-message'],'Cargo completion/diagnostic differs')
 rows.append(row)
# Authenticate every phase input identity once, without repeatedly hashing identical files.
for record in phase_inputs.values():check_file(record)
for link in phase_aliases.values():require(Path(link['path']).is_symlink()and os.readlink(link['path'])==link['target'],'phase alias changed')
for name in phase_absent:require(not Path(name).exists()and not Path(name).is_symlink(),'new Cargo configuration')
phase.check(json_read(N/'clippy-plan.json'))  # Full source/dependency/target identity at the final boundary.
accepted_prior=[r for r in prior['phases']if r['accepted']];combined=accepted_prior+rows
require(len(combined)==72,'wrong combined finite phase count')
tests=[r for r in combined if r['kind']=='test'];require(len(tests)==63 and len({tuple(r['selected'])for r in tests})==63,'wrong unique test count')
require({r['name']for r in tests}==set(full['proposed_order']),'lost original test')
counts=collections.Counter(full['groups'][r['name']]['artifact']for r in tests)
marker='file-table EMFILE control completed';raw=read(N/'observer/neighbor-20/stderr').decode()
require(raw.splitlines().count(marker)==1 and 'isolated file-table control status=exit status: 0' in raw,'real unchanged EMFILE child incomplete')
write_new(F/'CHILD-CONTROLS.json',dict(new_capture_children=file_record(OLD/'CHILD-CONTROLS.json'),unchanged_real_install_emfile=dict(stderr=file_record(N/'observer/neighbor-20/stderr'),exact_completion_marker=marker,marker_count=1,child_exit_status=0),meaning='Actual markers supplement exact libtest outcomes; no child skip credited.'))
for item in json_read(Q/'retained-binaries/ARTIFACTS.json').values():require(item['cargo_artifact']['fresh'] is False,'original harness reused');check_file(item['file'],executable=True)
check_file(json_read(OLD/'ARTIFACT-READBACK.json')['retained_linked_library'])
write_new(F/'ARTIFACT-READBACK.json',dict(original_binding=file_record(Q/'retained-binaries/BINDING.json'),retained=file_record(Q/'retained-binaries/ARTIFACTS.json'),linked_rlib=file_record(OLD/'retained-linked-library.rlib'),four_harnesses_and_rlib_unchanged=True,no_rebuild_for_continuation=True,meaning='All tests used the original fresh V2 copies. Core/ptrace and Clippy checks are separate actual commands; no new test artifact credit.'))
last=json_read(N/'clippy-plan.json');sha=digest(N/'clippy-plan.json');oldplan=json_read(Q/'neighbor-05-plan.json');oldsha=digest(Q/'neighbor-05-plan.json')
oldfd=cache_lease.open_bound(oldplan['lease']);newfd=None
try:
 fcntl.flock(oldfd,fcntl.LOCK_EX|fcntl.LOCK_NB);cache_lease.check_held(oldfd,oldplan['lease'],oldsha)
 require(not (Path(oldplan['lease']['state_directory'])/(oldsha+'.json')).exists(),'old refusal token was completed')
 newfd=cache_lease.open_bound(last['lease']);fcntl.flock(newfd,fcntl.LOCK_EX|fcntl.LOCK_NB);cache_lease.check_held(newfd,last['lease'],sha)
 complete=N/'lease-state'/(sha+'.json');c=json_read(complete);require(c['plan_sha256']==sha,'new completion differs');check_file(c['result'])
 o=json_read(N/'observer/clippy/result.json');unit=o['final_accounting']['properties']['Id']
 query=bounded_process(['/usr/bin/systemctl','--user','show',unit,'--property=Id,LoadState,ActiveState,SubState,MainPID,ControlGroup'],N,last['environment'],F/'terminal-clippy',5,65536)
 require(query['returncode']==0 and not query['forced'],'last unit readback refused')
 for key in ['stdout','stderr']:require(query[key]['bytes']<=65536,'terminal query too large');check_file(query[key])
 vals=dict(x.split('=',1)for x in read(query['stdout']['path'],65537).decode().splitlines()if '='in x)
 require(vals.get('Id')==unit and vals.get('ActiveState')in ['inactive','failed']and vals.get('MainPID')=='0'and vals.get('ControlGroup')=='','new unit not terminal')
 phase.scm(last,F/'SCM-FINAL.json');cache_lease.check_held(oldfd,oldplan['lease'],oldsha);cache_lease.check_held(newfd,last['lease'],sha)
 release=dict(old_lease=dict(binding=oldplan['lease'],token=os.pread(oldfd,4096,0).decode(),unresolved_completion_absent=True,no_token_mutation=True),new_lease=dict(binding=last['lease'],token=os.pread(newfd,4096,0).decode(),completion=file_record(complete)),exclusive_nonblocking_readback_both=True,fresh_last_service=dict(query=query,properties=vals),old_startup_proof=file_record(OLD/'TERMINAL-READBACK.json'),no_recovery=True)
finally:
 if newfd is not None:os.close(newfd)
 os.close(oldfd)
release['both_readback_holds_closed']=True;write_new(F/'LEASE-RELEASE.json',release)
result=dict(status='FINITE COMPONENT QUALIFICATION COMPLETE, PRIOR FAILURES RETAINED',source_target=file_record(D/'TARGET.json'),source_patch=file_record(D/'SOURCE.patch'),source_delta=file_record(D/'V1-TO-V2.patch'),original_caller=file_record(Q/'CALLER-TARGET.json'),continuation_caller=file_record(N/'CALLER-TARGET.json'),original_partial=file_record(OLD/'READBACK.json'),continuation_phases=55,continuation_exact_tests=53,combined_accepted_phases=72,combined_unique_exact_tests=63,selected_by_artifact=dict(counts),product_test_failures=0,ignored_tests=0,accepted_test_repeats=0,original_observer_refusals=1,actual_phase_attempts_current_source=73,current_source_whole_cpu_seconds=None,accepted_phase_cpu_seconds=sum(r['cpu_seconds']for r in combined),accepted_observer_wall_seconds=sum(r['observer_wall_seconds']for r in combined),all_attempt_observer_wall_seconds=sum(r['observer_wall_seconds']for r in combined)+next(r['observer_wall_seconds']for r in prior['phases']if not r['accepted']),accepted_payload_wall_seconds=sum(r['payload_wall_seconds']for r in combined),exact_test_payload_wall_seconds=sum(r['payload_wall_seconds']for r in tests),inventories=prior['inventories'],prior_v1_compile_failure_preserved=True,prior_startup_cpu_accounting_missing=True,phases=combined,retained_refusal=next(r for r in prior['phases']if not r['accepted']),qualification_scope='63 exact component declarations + required format/core/ptrace/Clippy and fresh build/inventories; no Hermit parity or independent source approval')
write_new(F/'RESULTS.json',result)
(F/'REPORT.md').write_text(f'''# Captured pipe identity V2: completed finite qualification

All **63 unique exact declarations** passed, with no failed or ignored product test: {dict(counts)}. The six new capture controls, all 20 original metadata/ownership neighbors and all 37 V6 declarations are included. Actual inventories are {prior['inventories']}. The fresh compile, format, inventories, core/ptrace check and Clippy qualified with no structured diagnostics. This is 72 accepted finite phases composed from the first 17 accepted phases and the 55-phase continuation.

The original neighbor-05 observer startup refusal remains raw 137 with missing accounting and no payload authorization; it is not relabelled. There were 73 actual phase attempts on V2, including that refusal. The continuation ran only 53 previously unqualified selectors and the two unrun checks; none of the ten accepted tests, compile, format or inventories was repeated. Its four test ELFs and the separately retained linked rlib match the original fresh V2 compilation exactly. The older V1 compile failures remain separate and unchanged.

Accepted phases consumed {result['accepted_phase_cpu_seconds']:.6f} CPU seconds and {result['accepted_observer_wall_seconds']:.6f} summed observer-wall seconds. Including the refused startup, summed observer wall is {result['all_attempt_observer_wall_seconds']:.6f} seconds; total CPU remains unknown because that startup had no final accounting. Exact test payloads sum to {result['exact_test_payload_wall_seconds']:.6f} seconds. These sums are not elapsed end-to-end wall time.

Raw streams prove the isolated child completion markers for closed standard descriptors, root capture-owner cleanup through normal/unwind paths, real capture setup EMFILE and unchanged file-table install EMFILE. The latter reached its post-limit-restoration checks for local table/handles/flags/offsets and epoll MOD/DEL, then a successful unconstrained install. The direct/Tool captured-stream versus ordinary-pipe identity control passed with required KVM admission. Original failed stdout/pipe equality measurements are preserved; component passes do not replace or rerun them.

All source/lock/compiler/dependency and retained-ELF/loader bindings remain exact. Test assertions and full warning/format gates were unchanged. Tests kept 30 CPU / 60 observer-wall seconds and the explicitly reviewed 30-second inner timeout with 2-second kill grace; the original isolated child bounds remain. Core/Clippy kept 600 CPU / 900 wall. Memory, swap, output and free-space bounds were unchanged.

The continuation used its own normal private lease while its parent held the old exact lease exclusively without rewriting the refused token. Final readback acquired both leases exclusively: the new last Clippy completion is normal and the old token is still unresolved with no completion record. The final service is inactive/empty; both audit holds are closed. Original missing CPU accounting was not reconstructed and no generic recovery exception was added.

This authored implementation and measured qualification are ready for independent source review. They confer no Hermit same-run parity, whole timer/scheduler, cancellation-fixture or activation approval. Live product source and SCM were unchanged. The initial continuation-v1 preparation that omitted admission checks remains unexecuted; v2 restores those checks and is the only executed continuation. No old failure, qualification gate or comparator was weakened.
''')
files={};links={};gitlinks={};sockets={}
def add(p):
 p=Path(p);s=p.lstat()
 if stat.S_ISREG(s.st_mode):files[str(p)]=file_record(p)
 elif stat.S_ISLNK(s.st_mode):links[str(p)]=dict(path=str(p),target=os.readlink(p))
 elif stat.S_ISSOCK(s.st_mode):sockets[str(p)]=dict(path=str(p),kind='completed_observer_socket_metadata_only',device=s.st_dev,inode=s.st_ino,uid=s.st_uid,mode=stat.S_IMODE(s.st_mode))
 else:require(stat.S_ISDIR(s.st_mode),'unexpected output type')
held=json_read(N/'CALLER-INPUTS.json')
for r in held['files']:check_file(r);files[r['path']]=r
for r in held['symlinks']:require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'held alias drift');links[r['path']]=r
for r in held['unexpanded_gitlinks']:require(Path(r['path']).is_dir()and not Path(r['path']).is_symlink(),'held gitlink drift');gitlinks[r['path']]=r
for r in json_read(OLD/'INPUTS.json')['socket_metadata']:sockets[r['path']]=r
for p in N.iterdir():
 if p.is_file()and p.name!='lane.lease':add(p)
for d in ['observer','controls','loader','launch','lease-state']:
 for p in sorted((N/d).rglob('*')):add(p)
for p in F.iterdir():add(p)
write_new(F/'INPUTS.json',dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(links.values(),key=lambda x:x['path']),unexpanded_gitlinks=sorted(gitlinks.values(),key=lambda x:x['path']),completed_socket_metadata=sorted(sockets.values(),key=lambda x:x['path']),excluded_mutable=['private Cargo target contents; original paths retained as metadata','tmp/cache','raw lease inodes; exact tokens and identity bound in LEASE-RELEASE']))
for r in files.values():check_file(r)
for r in links.values():require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'final alias drift')
write_new(F/'TARGET.json',dict(scope='Completed finite capture pipe identity source qualification, not independent approval',source=file_record(D/'TARGET.json'),full_patch=file_record(D/'SOURCE.patch'),delta=file_record(D/'V1-TO-V2.patch'),report=file_record(F/'REPORT.md'),results=file_record(F/'RESULTS.json'),inputs=file_record(F/'INPUTS.json'),lease_release=file_record(F/'LEASE-RELEASE.json'),artifact_readback=file_record(F/'ARTIFACT-READBACK.json'),child_controls=file_record(F/'CHILD-CONTROLS.json')))
write_new(F/'READBACK.json',dict(target=file_record(F/'TARGET.json'),report=file_record(F/'REPORT.md'),results=file_record(F/'RESULTS.json'),inputs=file_record(F/'INPUTS.json'),regular_files=len(files),symlinks=len(links),gitlinks=len(gitlinks),socket_metadata=len(sockets),all_indexed_bytes_modes_unchanged=True,accepted_phases=72,unique_exact_tests=63,prior_failures_preserved=True,old_lease_unresolved=True,source_scm_mutations=False))
print(json.dumps({n:file_record(F/n)for n in ['TARGET.json','REPORT.md','RESULTS.json','INPUTS.json','READBACK.json','LEASE-RELEASE.json']},indent=2))
