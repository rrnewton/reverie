#!/usr/bin/python3
"""Freeze the one authorized run, including a first refusal and all unrun cases."""
import fcntl,hashlib,json,os,re,stat,sys
from pathlib import Path
Q=Path(__file__).resolve().parent;W=Q.parent;V=W/'v2-fix';F=Q/'final-v1'
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
require(sys.flags.optimize==0,'unoptimized reader required');require(not F.exists(),'preserve final packet')
j=load(Q/'EXECUTION-RESULT.json');specs=load(Q/'PHASES.json')['phases'];by_name={x['name']:x for x in specs}
attempt_names=[x['name'] for x in j['attempts']];require(attempt_names==[x['name'] for x in specs[:len(attempt_names)]],'attempt sequence changed')
F.mkdir(mode=0o700);rows=[];clock=[];modes=[];diagnostics=[];complete=[]
for attempt in j['attempts']:
 n=attempt['name'];s=by_name[n];plan=load(Q/(n+'-plan.json'));rp=Q/'controls'/n/'result.json';r=load(rp)
 op=Q/'observer'/n/'result.json';o=load(op);accounting=o.get('final_accounting')
 row=dict(name=n,kind=s['kind'],selected=s['selected'],plan=rec(Q/(n+'-plan.json')),receipt=rec(rp),observer=rec(op),raw_status=r.get('raw_status'),accepted=n in j['accepted'],phase_accepted=r['accepted'],terminal_authenticated=r['terminal_authenticated'],inputs_unchanged=r.get('inputs_unchanged'),accounting_complete=o.get('accounting_complete'),final_accounting=accounting,observer_elapsed_seconds=o.get('elapsed_seconds'),payload_elapsed_seconds=r.get('payload_exit',{}).get('elapsed_seconds'),readback=r.get('readback'),error=r.get('error'),readback_error=r.get('readback_error'),stdout=rec(Q/'observer'/n/'stdout'),stderr=rec(Q/'observer'/n/'stderr'))
 cp=Q/'lease-state'/(r['plan_sha256']+'.json')
 if cp.exists():
  c=load(cp);require(c['plan_sha256']==r['plan_sha256'] and c['result']==rec(rp),'completion binding differs');row['completion']=rec(cp);complete.append(n)
 raw=(Q/'observer'/n/'stderr').read_text();out=(Q/'observer'/n/'stdout').read_text()
 if s['kind']=='test':
  events=[dict(kind=m.group(1),value=int(m.group(2))) for m in re.finditer(r'^CLOCK_ORIGIN_TEST ([a-z_]+)=(\d+)$',raw,re.M)]
  if events:clock.append(dict(phase=n,selected=s['selected'],accepted=row['accepted'],events=events,stderr=row['stderr']))
  markers=re.findall(r'^INJECTION_STOP_COMPLETE (\w+)$',raw,re.M)
  if markers:modes.append(dict(phase=n,selected=s['selected'],accepted=row['accepted'],markers=markers,stderr=row['stderr']))
  for marker in s.get('required_markers',[]):
   if row['accepted']:require(raw.splitlines().count(marker)==1,'accepted phase lacks exact mode marker')
 if s['kind'] in ['compile','check']:
  messages=[json.loads(x) for x in out.splitlines() if json.loads(x).get('reason')=='compiler-message']
  diagnostics.append(dict(phase=n,messages=messages))
 rows.append(row)
accepted=[x for x in rows if x['accepted']];tests=[x for x in rows if x['kind']=='test'];passed=[x for x in tests if x['accepted']]
all_ok=len(rows)==45 and len(accepted)==45 and len(passed)==38 and j['error'] is None
for row in accepted:
 require(row['raw_status']==0 and row['terminal_authenticated'] and row['inputs_unchanged'] and row['accounting_complete'],'accepted phase lacks qualification')
if all_ok:require([m for x in modes for m in x['markers']]==['PostExec','Replace','SameThenPrivate','TailSame','TailOther','PostExecTail','Emulate','EmulateErrno'],'eight mode sequence differs')
write(F/'CLOCK-OBSERVATIONS.json',clock);write(F/'MODE-OBSERVATIONS.json',modes);write(F/'COMPILER-DIAGNOSTICS.json',diagnostics)
lease=Q/'lane.lease';lb=rec(lease);fd=os.open(lease,os.O_RDONLY|os.O_CLOEXEC)
try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);available=True
finally:os.close(fd)
require(lb==rec(lease),'lease changed during read-only completion probe')
write(F/'LEASE-RELEASE.json',dict(lease_snapshot=lb,exclusive_lock_obtainable=available,token_retained=True,completed_phase_names=complete,all_attempted_normally_completed=len(complete)==len(rows),previous_private_leases_not_opened=True))
cpu=[x['final_accounting']['cpu_usage_nsec'] for x in rows if x['accounting_complete'] and x['final_accounting'] is not None]
results=dict(attempted_phases=len(rows),accepted_phases=len(accepted),failed_or_refused_phases=len(rows)-len(accepted),planned_phases=45,planned_declarations=38,attempted_declarations=len(tests),passed_declarations=len(passed),failed_or_refused_declarations=len(tests)-len(passed),unrun_declarations=38-len(tests),ignored_declarations=sum(x.get('readback',{}).get('summary',{}).get('ignored',0) for x in passed),unrun_phase_names=[x['name'] for x in specs[len(rows):]],all_component_phases_accepted=all_ok,retries=0,cpu_usage_nsec=sum(cpu),cpu_accounting_complete=len(cpu)==len(rows),observer_elapsed_seconds_sum=sum(x['observer_elapsed_seconds'] or 0 for x in rows),test_payload_elapsed_seconds_sum=sum(x['payload_elapsed_seconds'] or 0 for x in tests),phases=rows,clock_observations=rec(F/'CLOCK-OBSERVATIONS.json'),mode_observations=rec(F/'MODE-OBSERVATIONS.json'),prior_compiler_failure_preserved=rec(W/'qualification-v1/final-v1/READBACK.json'),no_hermit_or_parity_execution=True,precise_timer_parent_result_not_independent_child_completion_evidence=True)
for name in ['ARTIFACTS.json','BINDING.json']:
 p=Q/'retained-binaries'/name
 if p.exists():results['retained_'+name.split('.')[0].lower()]=rec(p)
results['driver_error']=j['error']
results['inventories']={key:load(Q/'controls'/('list-'+key)/'result.json')['readback']['count'] for key in ['ptrace','vsyscall'] if (Q/'controls'/('list-'+key)/'result.json').exists()}
legacy_path=Q/'observer/test-38/stdout'
if legacy_path.exists():
 legacy_json=[];other_lines=[]
 for number,line in enumerate(legacy_path.read_text().splitlines(),1):
  try:legacy_json.append(dict(line=number,value=json.loads(line)))
  except json.JSONDecodeError:other_lines.append(dict(line=number,sha256=hashlib.sha256(line.encode()).hexdigest()))
 write(F/'LEGACY-READBACK-LIMIT.json',dict(stdout=rec(legacy_path),json_records=legacy_json,non_json_lines=other_lines,original_receipt=rec(Q/'controls/test-38/result.json'),original_refusal_preserved=True,not_an_alternative_qualification_receipt=True,read_error='Direct cat of controls/test-38/outcomes.json failed: absent, because original parsing stopped before writing that file.'))
 results['legacy_output_inspection']=rec(F/'LEGACY-READBACK-LIMIT.json')
write(F/'RESULTS.json',results)
event_lines=[]
for row in clock:event_lines.append('- '+row['phase']+' '+row['selected'][0]+': '+', '.join(x['kind']+'='+str(x['value']) for x in row['events'])+'.')
mode_list=', '.join(m for x in modes for m in x['markers']) or 'none reached'
report=f'''# Exact clock and injection-stop qualification

The one authorized successor run attempted {len(rows)} of 45 phases, accepted {len(accepted)} and failed/refused {len(rows)-len(accepted)}. It executed {len(tests)} of 38 exact declarations: {len(passed)} accepted, {len(tests)-len(passed)} failed/refused, {38-len(tests)} unrun. No retry occurred. All earlier compiler/clock/debugger failures remain unchanged.

The source changes after the first failed compile are only the public PathPtr import and removal of the redundant Write import. Production, assertions, guest bytes, selection and bounds are unchanged. Seven new declarations encompass eight modes; actual emitted completion markers are: {mode_list}. Markers receive qualification credit only with their accepted exact-test result.

The final legacy-vsyscall declaration returned raw zero and printed its exact test and suite `ok` records, but the original strict JSON-lines reader refused the mixed stdout stream: the existing testing helper also emitted ANSI DEBUG/TRACE/INFO records there. Its accepted=false / JSONDecodeError receipt remains unchanged. The raw trace includes actual time interception and no skip marker. The separate raw-output projection is an audit aid, not a replacement qualification receipt. No case was rerun and no parser was relaxed.

Actual clock observations, in retained stream order:

{chr(10).join(event_lines) or 'No clock observation was reached.'}

Full raw stdout/stderr, exact per-phase argv, retained ELF/library/loader bindings, fresh inventory and terminal receipts are indexed. The original exact guest boundaries remain 1/65 or 65/129; no fixed offset, tolerance or count adjustment was applied. The post-exec 0 / timer 1 change remains the explicitly documented causal-oracle correction, not a relabelled prior pass.

Measured CPU sums to {results['cpu_usage_nsec']/1e9:.9f} seconds; observer wall sums to {results['observer_elapsed_seconds_sum']:.9f} seconds; exact-test payload wall sums to {results['test_payload_elapsed_seconds_sum']:.9f} seconds. CPU accounting is complete for {len(cpu)} of {len(rows)} attempted phases. Private lease completion is recorded separately; an exclusive read-only probe succeeds with token bytes unchanged.

This is finite component evidence, not independent source approval, a Hermit run, parity, or whole timer/scheduler clearance. Write checks its three meaningful arguments; the mapping control is not a six-register oracle. Existing specialized rejection/unit neighbors do not establish every positive activated frame, converted mapping-tail or interrupted private-syscall path. The unchanged precise-timer parent test has an early capability return and captures its child output internally: its parent result alone is not separately authenticated evidence of both child modes. No claim of those extra executions is made.
'''
(F/'REPORT.md').open('x').write(report)
files={};links={};sockets=[]
for x in load(Q/'INPUTS.json')['files']:
 require(rec(x['path'])==x,'frozen input drift: '+x['path']);files[x['path']]=x
for x in load(Q/'INPUTS.json')['symlinks']:
 p=Path(x['path']);require(p.is_symlink() and os.readlink(p)==x['target'],'frozen alias drift');links[x['path']]=x
for p in sorted(Q.rglob('*')):
 if p==lease or p.is_relative_to(F):continue
 s=p.lstat()
 if stat.S_ISREG(s.st_mode):files[str(p)]=rec(p)
 elif stat.S_ISLNK(s.st_mode):links[str(p)]=dict(path=str(p),target=os.readlink(p))
 elif stat.S_ISSOCK(s.st_mode):sockets.append(dict(path=str(p),mode=stat.S_IMODE(s.st_mode),identity=[s.st_dev,s.st_ino,s.st_uid],completed_metadata_only=True))
for name in ['dependency-inputs.json']:
 p=Q/name
 if p.exists():
  for x in load(p):require(rec(x['path'])==x,'dependency drift');files[x['path']]=x
for p in (Q/'loader').glob('*.json'):
 d=load(p)
 if 'inputs' in d:
  for x in d['inputs']:require(rec(x['path'])==x,'loader input drift');files[x['path']]=x
  for x in d['input_symlinks']:
   require(Path(x['path']).is_symlink() and os.readlink(x['path'])==x['target'],'loader alias drift');links[x['path']]=x
for p in F.iterdir():
 if p.is_file():files[str(p)]=rec(p)
write(F/'INPUTS.json',dict(files=sorted(files.values(),key=lambda x:x['path']),symlinks=sorted(links.values(),key=lambda x:x['path']),completed_socket_metadata=sockets,mutable_targets_and_lease_files_excluded=True))
write(F/'TARGET.json',dict(scope='First actual finite successor qualification; no broader approval',source_target=rec(V/'TARGET.json'),source_patch=rec(V/'SOURCE.patch'),import_delta=rec(V/'V2-FIX.patch'),caller_target=rec(Q/'CALLER-TARGET.json'),results=rec(F/'RESULTS.json'),report=rec(F/'REPORT.md'),inputs=rec(F/'INPUTS.json'),lease_release=rec(F/'LEASE-RELEASE.json')))
write(F/'READBACK.json',dict(target=rec(F/'TARGET.json'),results=rec(F/'RESULTS.json'),report=rec(F/'REPORT.md'),inputs=rec(F/'INPUTS.json'),regular_files=len(files),literal_aliases=len(links),completed_sockets=len(sockets),all_frozen_inputs_unchanged=True,source_unchanged=True,all_component_phases_accepted=all_ok,lease_release=rec(F/'LEASE-RELEASE.json')))
print(json.dumps(dict(target=rec(F/'TARGET.json'),results=rec(F/'RESULTS.json'),report=rec(F/'REPORT.md'),readback=rec(F/'READBACK.json'),files=len(files),aliases=len(links),attempted_phases=len(rows),accepted_phases=len(accepted),passed_tests=len(passed),unrun_tests=38-len(tests))))
