#!/usr/bin/python3
"""Retain the first compiler failure; never execute a product or repair a token."""
import fcntl
import hashlib
import json
import os
import stat
import sys
from pathlib import Path

Q=Path(__file__).resolve().parent;W=Q.parent;F=Q/'final-v1'
def require(value,message):
 if not value:raise RuntimeError(message)
def load(p):return json.loads(Path(p).read_text())
def rec(p):
 p=Path(p);s=p.lstat();require(stat.S_ISREG(s.st_mode),'not regular: '+str(p));h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=h.hexdigest())
def write(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
require(sys.flags.optimize==0,'unoptimized reader required')
require(not F.exists(),'preserve final packet');F.mkdir(mode=0o700)
journal=load(Q/'EXECUTION-RESULT.json');require([x['name'] for x in journal['attempts']]==['metadata','compile'],'unexpected attempts')
phases=[]
for item in journal['attempts']:
 n=item['name'];r=load(Q/'controls'/n/'result.json');o=load(Q/'observer'/n/'result.json');a=o['final_accounting'];p=load(Q/(n+'-plan.json'))
 require(r['terminal_authenticated'] and r['inputs_unchanged'] and o['accounting_complete'] and a['cgroup_empty'],'terminal/input proof missing')
 completion=Q/'lease-state'/(rec(Q/(n+'-plan.json'))['sha256']+'.json');c=load(completion)
 require(c['plan_sha256']==r['plan_sha256'] and c['result']==rec(Q/'controls'/n/'result.json'),'normal completion differs')
 phases.append(dict(name=n,kind=p['kind'],plan=rec(Q/(n+'-plan.json')),receipt=rec(Q/'controls'/n/'result.json'),observer=rec(Q/'observer'/n/'result.json'),completion=rec(completion),raw_status=r['raw_status'],accepted=r['accepted'],terminal_authenticated=True,inputs_unchanged=True,cpu_usage_nsec=a['cpu_usage_nsec'],observer_elapsed_seconds=o['elapsed_seconds'],payload_elapsed_seconds=r['payload_exit']['elapsed_seconds'],final_accounting=a,stdout=rec(Q/'observer'/n/'stdout'),stderr=rec(Q/'observer'/n/'stderr')))
require(phases[0]['accepted'] and phases[0]['raw_status']==0 and not phases[1]['accepted'] and phases[1]['raw_status']==101,'original outcome changed')
messages=[json.loads(x) for x in (Q/'observer/compile/stdout').read_text().splitlines() if json.loads(x).get('reason')=='compiler-message']
write(F/'COMPILER-DIAGNOSTICS.json',messages)
require(any(x['message'].get('code',{}).get('code')=='E0603' for x in messages),'expected import error missing')
lease=Q/'lane.lease';before=rec(lease)
fd=os.open(lease,os.O_RDONLY|os.O_CLOEXEC)
try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);available=True
finally:os.close(fd)
require(before==rec(lease),'lease mutated by terminal inspection')
write(F/'LEASE-RELEASE.json',dict(lease_snapshot=before,exclusive_lock_obtainable=available,inspection_read_only=True,token_retained=True,normal_completion=phases[-1]['completion'],old_unresolved_C_lease_not_opened=True))
results=dict(attempted_phases=2,accepted_phases=1,failed_phases=1,planned_phases=45,planned_declarations=38,attempted_declarations=0,passed_declarations=0,failed_declarations=0,unrun_declarations=38,retries=0,cpu_usage_nsec=sum(x['cpu_usage_nsec'] for x in phases),observer_elapsed_seconds_sum=sum(x['observer_elapsed_seconds'] for x in phases),phases=phases,diagnostics=rec(F/'COMPILER-DIAGNOSTICS.json'),unrun_phase_names=[x['name'] for x in load(Q/'PHASES.json')['phases']][2:],no_qualified_harness=True,no_guest_or_timer_measurement=True,no_oracle_or_source_changes=True)
write(F/'RESULTS.json',results)
report=f'''# First qualification: compiler failure preserved

The fresh qualification stopped at compile, raw 101 / accepted=false. Metadata passed. Exactly two phases were attempted; one passed and one failed. All 38 selected test declarations and the remaining 43 phases are unrun. No clock, timer, injection-memory, mode or legacy-vsyscall assertion was executed. No qualified test ELF was retained.

The compiler reports E0603 at injection_stop_tests.rs:31: the test imports PathPtr through the private args module. The public re-export exists at reverie-syscalls/src/lib.rs:35. It also reports the redundant std::io::Write import at line 7. Both are test imports; this receipt does not establish successful compilation of the complete candidate.

Compile payload elapsed {phases[1]['payload_elapsed_seconds']:.9f} seconds; its observer elapsed {phases[1]['observer_elapsed_seconds']:.9f} seconds and measured CPU was {phases[1]['cpu_usage_nsec']/1e9:.9f} seconds. Across metadata and compile, observer wall sums to {results['observer_elapsed_seconds_sum']:.9f} seconds and measured CPU to {results['cpu_usage_nsec']/1e9:.9f} seconds. Both services are inactive/dead with MainPID zero and empty control groups, with complete accounting.

The structured phase reader retains its original `missing successful Cargo completion` refusal. It stops before writing its diagnostic projection on an unsuccessful Cargo completion; this packet separately extracts the actual unchanged compiler-message records. Raw stdout/stderr remain authoritative. The driver returned 1 and did not retry.

Source, frozen inputs, assertions and original bounds remain unchanged. Normal private lease completion is authenticated; the last token remains recorded and an exclusive read-only lock probe succeeds after driver completion. The unrelated old C unresolved token was never opened. No failure is relabelled and no product, source, runtime, scheduler or parity approval is implied.
'''
(F/'REPORT.md').open('x').write(report)
files={};aliases={};sockets=[]
old=load(Q/'INPUTS.json')
for x in old['files']:
 actual=rec(x['path']);require(actual==x,'frozen input changed: '+x['path']);files[x['path']]=x
for x in old['symlinks']:
 p=Path(x['path']);require(p.is_symlink() and os.readlink(p)==x['target'],'frozen alias changed');aliases[x['path']]=x
for p in sorted(Q.rglob('*')):
 if p==lease or p.is_relative_to(F):continue
 s=p.lstat()
 if stat.S_ISREG(s.st_mode):files[str(p)]=rec(p)
 elif stat.S_ISLNK(s.st_mode):aliases[str(p)]=dict(path=str(p),target=os.readlink(p))
 elif stat.S_ISSOCK(s.st_mode):sockets.append(dict(path=str(p),mode=stat.S_IMODE(s.st_mode),identity=[s.st_dev,s.st_ino,s.st_uid],completed_metadata_only=True))
for x in load(Q/'dependency-inputs.json'):
 require(rec(x['path'])==x,'actual dependency changed');files[x['path']]=x
for p in F.iterdir():
 if p.is_file():files[str(p)]=rec(p)
write(F/'INPUTS.json',dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(aliases.values(),key=lambda x:x['path']),completed_socket_metadata=sockets,mutable_target_and_lease_excluded=True))
write(F/'TARGET.json',dict(source_target=rec(W/'TARGET.json'),caller_target=rec(Q/'CALLER-TARGET.json'),report=rec(F/'REPORT.md'),results=rec(F/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),lease_release=rec(F/'LEASE-RELEASE.json'),scope='First fresh compiler failure, no selected test executed'))
write(F/'READBACK.json',dict(target=rec(F/'TARGET.json'),report=rec(F/'REPORT.md'),results=rec(F/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),regular_files=len(files),literal_aliases=len(aliases),completed_sockets=len(sockets),all_original_inputs_unchanged=True,source_unchanged=True,lease_release=rec(F/'LEASE-RELEASE.json'),attempted_phases=2,accepted_phases=1,unrun_tests=38,no_retry=True))
print(json.dumps(dict(target=rec(F/'TARGET.json'),report=rec(F/'REPORT.md'),results=rec(F/'RESULTS.json'),readback=rec(F/'READBACK.json'),files=len(files),aliases=len(aliases),cpu_seconds=results['cpu_usage_nsec']/1e9,observer_wall_seconds=results['observer_elapsed_seconds_sum'])))
