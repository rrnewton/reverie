from pathlib import Path
import datetime,hashlib,json,os,stat,subprocess
l=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918');d=l/'ignored/kvm-cancellation-wake-v1/successor-v2';e=d/'execution-v1';f=d/'final-v1';f.mkdir()
def rec(p):
 s=p.lstat()
 with p.open('rb') as stream:h=hashlib.file_digest(stream,'sha256').hexdigest()
 return {'path':str(p),'bytes':p.stat().st_size,'mode':stat.S_IMODE(s.st_mode),'sha256':h}
def put(p,data):p.write_text(json.dumps(data,indent=2)+'\n')
context=json.loads((e/'CONTEXT-BEFORE.json').read_text());result=json.loads((e/'CONTROLLER-RESULT.json').read_text());assert result['complete'] and len(result['phases'])==7
records=[context['lockfile'],context['caller'],context['contract'],*[x['identity'] for x in context['tools']],*context['source'].values()]
for r in records:
 a=rec(Path(r['path']));assert all(a[k]==r[k] for k in ['bytes','mode','sha256']),r['path']
versions=[]
for r in context['tools']:
 v=subprocess.run(r['version_argv'],cwd=l,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
 assert v.returncode==0 and v.stdout.decode()==r['version_stdout'] and v.stderr.decode()==r['version_stderr']
 versions.append({'identity':rec(Path(r['identity']['path'])),'version_raw_status':v.returncode,'version_stdout':v.stdout.decode(),'version_stderr':v.stderr.decode()})
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=l,text=True).strip()==context['base']
assert subprocess.check_output(['git','branch','--show-current'],cwd=l,text=True).strip()==context['branch']
target=json.loads((d/'TARGET.json').read_text())
for r in target['records']:
 a=rec(Path(r['path']));assert all(a[k]==r[k] for k in ['bytes','mode','sha256'])
after={'checked_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'all_source_lockfile_tool_and_caller_bytes_unchanged':True,'source':{n:rec(Path(r['path'])) for n,r in context['source'].items()},'lockfile':rec(Path(context['lockfile']['path'])),'tools':versions,'base':context['base'],'branch':context['branch'],'original_preparation_records_unchanged':len(target['records'])}
put(f/'INPUTS-AFTER.json',after)
binding=json.loads((d/'retained/BINDING.json').read_text());elf=rec(Path(binding['path']));assert all(elf[k]==binding[k] for k in ['bytes','sha256'])
selected=json.loads((d/'inventory/SELECTION.json').read_text());names=[];terms=[]
compile_rows=[json.loads(line) for line in (d/'compile/stdout').read_text().splitlines() if line.startswith('{')]
diagnostics=[r for r in compile_rows if r.get('reason')=='compiler-message']
assert not diagnostics
for phase in result['phases']:
 name=phase['phase'];assert phase['caller_raw_status']==0 and phase['payload_result']['raw_status']==0 and phase['context_unchanged'] and phase['terminal_settled'] and phase['controller_refusal'] is None and phase['payload_result']['refusal'] is None
 command=json.loads((d/name/'command.json').read_text());unit=command['service'][command['service'].index('--unit')+1]+'.service'
 query=subprocess.run(['/usr/bin/systemctl','--user','show',unit,'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
 props=dict(line.split('=',1) for line in query.stdout.decode().splitlines() if '=' in line)
 assert query.returncode==0 and props.get('LoadState')=='not-found' and props.get('ActiveState')=='inactive' and props.get('MainPID')=='0' and not props.get('ControlGroup'),(name,props)
 t=json.loads((e/name/'TERMINAL.json').read_text())
 for cgroup in t['observed_cgroups']:assert not Path(cgroup['path']).exists()
 terms.append({'phase':name,'unit':unit,'query_raw_status':query.returncode,'properties':props,'observed_cgroups':t['observed_cgroups']})
 if (d/name/'ACCEPTANCE.json').exists():
  accepted=json.loads((d/name/'ACCEPTANCE.json').read_text());assert accepted['expected']==accepted['succeeded'] or sorted(accepted['expected'])==sorted(accepted['succeeded']);assert accepted['raw_status']==accepted['ignored']==accepted['failed']==0;names+=accepted['succeeded']
assert len(names)==7 and sorted(names)==sorted(selected['selected'])
# The compile phase observed this actual user-service cgroup parent. All seven
# commands use the same default transient service slice without a Slice override.
compile_parent=Path(terms[0]['observed_cgroups'][0]['path']).parent
for t in terms:
 derived=compile_parent/t['unit'];assert not derived.exists();t['same_slice_expected_cgroup']={'path':str(derived),'exists':False,'basis':'parent directly observed for compile; same default transient service placement, no Slice override'}
put(f/'TERMINAL.json',{'units':terms,'all_seven_units_collected':True,'all_observed_and_same_slice_expected_cgroups_absent':True,'tool_handle':6853,'tool_handle_collected_raw_status':0,'tool_completion_chunk':'df1b69','controller_result':rec(e/'CONTROLLER-RESULT.json')})
summary={'source_preparation_target':rec(d/'TARGET.json'),'base':context['base'],'branch':context['branch'],'source':after['source'],'lockfile':after['lockfile'],'test_elf':elf,'retained_binding':rec(d/'retained/BINDING.json'),'actual_phases':result['phases'],'phase_count':7,'all_raw_zero':True,'library_test_inventory':selected['discovered'],'actual_declarations':names,'new_declarations_executed':4,'existing_neighbor_declarations_executed':3,'failed':0,'ignored':0,'compiler_diagnostics':len(diagnostics),'bounded_runner_elapsed_sum_seconds':sum(x['payload_result']['elapsed_seconds'] for x in result['phases']),'compile_bounded_runner_elapsed_seconds':result['phases'][0]['payload_result']['elapsed_seconds'],'scopes':['private in-progress L source at base91110d24 plus five bound component/vm files; not a committed final-head build','same existing offline locked cache; Cargo artifact retention binds this compiled test executable','7 selected host tests executed; 570 inventory declarations discovered, not 570 executed','no guest/KVM/ioctl, no full gate, scheduler or parity qualification','existing controlled-poison diagnostics preserved in raw stderr; no actual failed phase or retry']}
put(f/'RESULTS.json',summary)
report=f'''All seven admitted phases completed once with caller and payload raw status zero: offline locked library-test compile, discovery, four-new-test execution, three exact existing neighbor tests, and source format-check. Exactly seven selected host test declarations passed, zero failed and zero ignored. Discovery listed {selected['discovered']} declarations; that population was not executed. No guest or KVM ioctl was invoked by these selected controls.

Source is the uncommitted five-file composition at base {context['base']} on {context['branch']}. The vm.rs notification successor is SHA256 {after['source']['reverie-kvm/src/vm.rs']['sha256']}; all four root-owned component files retain their admitted bytes. Cargo.lock is {after['lockfile']['bytes']} bytes / {after['lockfile']['sha256']}. Those five files, lockfile, frozen caller/contract, actual cargo/rustc/rustfmt executables and complete version outputs were checked unchanged afterward.

The retained actual test ELF is {elf['sha256']} ({elf['bytes']} bytes). The compile used the existing offline locked cache and emitted zero structured compiler diagnostics in this test configuration. The removed dead-code allowance remains absent; use by unit tests is not production adapter integration. Compile took {summary['compile_bounded_runner_elapsed_seconds']:.6f} seconds in the bounded runner, including systemd launch/wait. The seven bounded-runner elapsed values sum to {summary['bounded_runner_elapsed_sum_seconds']:.6f} seconds; this is not guest CPU or pure compiler time.

The four new controls cover registration before/after publication, irrelevant worker-only wake and fresh subscription, cancellation/exec rearm/later cancellation, and synchronous callbacks observing published flags/status while acquiring every group registry and notification mutex. Three unchanged neighbor tests cover exec cancellation/rearm, poisoned failure-state recovery, and late start-gate cancellation. Their expected controlled-poison panic messages remain intact in stderr; all three returned successful test outcomes.

Outer controller handle 6853 was collected with raw zero. Each underlying caller was waited for, every systemd unit now reports not-found/inactive/MainPID0 with no ControlGroup, and every directly observed cgroup is absent. Short phases finished before their cgroup path was sampled; the same default-slice expected paths, derived from the compile phase's observed parent and the exact unit names, are separately marked and checked absent. No unobserved path is presented as a live sampled one.

No original tests, assertions, thresholds, comparator, cancellation predicate, status rule or SIGURG policy changed. There were no failed product phases, retries or test replays. Earlier v1 source/caller preparation remains preserved. This evidence qualifies the wake-only primitive and selected neighbors; the blocking/async adapters, full admission gate and measured backend parity remain outside this run.
'''
(f/'REPORT.md').write_text(report)
# Enumerate only this bounded private run and the already frozen source packet.
files=[]
for directory in [d/name for name in context['phases']]+[d/'retained',e]:
 files.extend(p for p in directory.rglob('*') if p.is_file())
files.extend(Path(r['path']) for r in target['records']);files.append(d/'TARGET.json')
files=sorted(set(files));evidence=[rec(p) for p in files];put(f/'EVIDENCE.json',{'records':evidence})
for r in evidence:assert rec(Path(r['path']))==r
readback={'records_authenticated':len(evidence),'all_byte_mode_hash_matches':True,'all_context_checks_passed':True,'exact_seven_successful_names_match_plan':True,'all_terminal_units_rechecked':True,'retained_elf_identity_matches':True,'no_frozen_preparation_file_changed':True}
put(f/'READBACK.json',readback)
final={'scope':'Actual bounded seven-phase cancellation-wake qualification, no guest/full-gate/parity claim','source_target':rec(d/'TARGET.json'),'records':[rec(p) for p in sorted(f.iterdir()) if p.is_file()]}
put(f/'TARGET.json',final)
print(json.dumps({'target':rec(f/'TARGET.json'),'results':rec(f/'RESULTS.json'),'report':rec(f/'REPORT.md'),'evidence_records':len(evidence),'readback':rec(f/'READBACK.json')}))
