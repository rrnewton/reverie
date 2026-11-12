#!/usr/bin/env python3
"""Offline evidence audit: reads retained inputs only; executes no product/caller code."""
import hashlib,json,pathlib,re,stat
OWNER=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/ignored')
C=OWNER/'m2-callers-v1'
OUT=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/m2-preliminary-evidence-review-20260917')
checks=[]; files={}; phases=[]; artifacts={}; inventories=[]
def ck(label,value,details=None):
 checks.append(dict(check=label,ok=bool(value),**({'details':details} if not value and details is not None else {})))
def record(path):
 p=pathlib.Path(path)
 if not p.is_relative_to(OWNER): raise ValueError('not retained owner evidence: '+str(p))
 if str(p) in files:return files[str(p)]
 s=p.lstat()
 if not stat.S_ISREG(s.st_mode):raise ValueError('not a regular retained file: '+str(p))
 h=hashlib.sha256()
 with p.open('rb') as f:
  for b in iter(lambda:f.read(1024**2),b''):h.update(b)
 d=dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=h.hexdigest())
 files[str(p)]=d;return d
def linked(r,label):
 actual=record(r['path']);ck(label,all(actual[k]==r[k] for k in ('bytes','sha256') if k in r) and ('mode' not in r or actual['mode']==r['mode']),dict(expected=r,actual=actual));return actual
def read(p):record(p);return json.loads(pathlib.Path(p).read_text())
def capture(r,label):
 ck(label+' exit',r['returncode']==0 and not r['forced']);linked(r['stdout'],label+' stdout');linked(r['stderr'],label+' stderr')
 ck(label+' no stderr',r['stderr']['bytes']==0)
 return pathlib.Path(r['stdout']['path']).read_bytes()
source=read(OWNER/'m2-source-v2/binding.json');sr=read(OWNER/'m2-source-v2/RESULT.json')
linked(sr['binding'],'source result -> binding');linked(source['source_manifest'],'source manifest');linked(source['complete_patch'],'source complete patch')
ck('source composition base',source['base']=='14d63ed54b7284b7f8bc29d44c7610a809815621')
ck('source composition tree',source['expected_composed_tree']=='597bad02a2a18bfb159ea65e191c76b88dd22c82')
ck('source changed paths count',len(source['changed_paths'])==33)
expected_counts={'detcore':702,'syscaller':0,'hermit':385,'hermit-bin':312,'hermit-dap':3,'verification-report':0}
comparison=read(C/'INVENTORY-COMPARISON.json')
for phase in ['compile-native']+['list-'+a for a in expected_counts]:
 base=C/'controls-run-1'/phase; ob=C/'observer'/phase
 result=read(base/'result.json');pr=linked(result['plan'],phase+' plan');plan=read(pr['path']);linked(plan['context'],phase+' context');context=read(plan['context']['path']);before=read(base/'before.json')
 ck(phase+' accepted recorded',result['accepted'] is True and result['terminal_authenticated'] is True and result['raw_status']==0)
 ck(phase+' source binding',result['source']==context['source_manifest']==before['source']==source['source_manifest'])
 ck(phase+' final source checks completed',result['final_source_inputs_unchanged'] is True)
 # Compare retained historical input records, not current mutable source/dependency paths.
 ck(phase+' historical input manifest equality',before['inputs']==context['inputs'])
 ck(phase+' historical executable manifest equality',before['executables']==context['executables'])
 for x in context['caller_files']+[context['observer']]:linked(x,phase+' retained helper '+pathlib.Path(x['path']).name)
 for when in ('before','after'):
  scm=read(base/('scm-'+when+'-binding.json'));ck(phase+' '+when+' SCM equals context',scm==context['scm'])
  for kind in ('head','tree','branch','index'):
   query=read(base/('scm-'+when+'-'+kind+'.json'));raw=capture(query,phase+' '+when+' '+kind)
   actual=hashlib.sha256(raw).hexdigest() if kind=='index' else raw.decode().strip()
   ck(phase+' '+when+' actual '+kind,actual==context['scm'][kind])
 linked(result['observer_result'],phase+' observer result');obs=read(result['observer_result']['path'])
 admission=read(ob/'admission.json');receipt=read(ob/'before-exec-receipt.json');spec=read(ob/'before-exec.json');payload=read(ob/'payload-exit.json')
 ck(phase+' payload result',payload==result['payload_exit'] and payload['returncode']==0 and payload['reaped'] is True and payload['local_wait_timed_out'] is False)
 ck(phase+' admitted argv',admission['argv']==plan['phase']['argv']);ck(phase+' plan SHA admitted',admission['phase_plan_sha256']==pr['sha256'])
 ck(phase+' no hardware admission claimed',plan['admission'] is False and admission['kvm_required'] is False)
 ck(phase+' requested environment',admission['environment']==plan['environment'] and all(admission['actual_environment'].get(k)==v for k,v in plan['environment'].items()))
 protected=('CARGO_','RUST','HERMIT_','NEXTEST_','LD_','THIRD_PARTY_','PKG_CONFIG_')
 extra={k:v for k,v in admission['actual_environment'].items() if k not in plan['environment']}
 ck(phase+' no undeclared protected override',not any(k.startswith(protected) or k in ('CC','CXX','CFLAGS','CPPFLAGS','CXXFLAGS','LDFLAGS') for k in extra))
 ck(phase+' declared runtime identities',all(x in admission['executable_bindings'] for x in plan['phase_bindings']['runtime_executables']))
 auth=obs['authenticated_service'];initial=auth['initial_properties'];main=auth['main_process'];fa=obs['final_accounting'];final=fa['properties'];launch=obs['launch'];p=plan['phase']
 ck(phase+' service PID identity',admission['supervisor_pid']==main['pid']==receipt['pid']==initial['MainPID']==initial['ExecMainPID']==final['ExecMainPID'])
 ck(phase+' service start identity',receipt['start_ticks']==main['start_ticks'] and initial['ExecMainStartTimestampMonotonic']==final['ExecMainStartTimestampMonotonic'])
 ck(phase+' cgroup identity',admission['cgroup']=='0::'+main['cgroup']+'\n' and initial['ControlGroup']==main['cgroup'] and auth['unit']==initial['Id']==final['Id'])
 ck(phase+' before-exec payload binding',receipt['authorization_received'] is True and receipt['payload']==spec['payload']==launch['payload'] and spec['payload']['arguments'][-2:]==[pr['path'],pr['sha256']])
 ck(phase+' before-exec SHA',record(ob/'before-exec.json')['sha256']==launch['before_exec_spec_sha256'])
 for name,sha in launch['observer_sources'].items():ck(phase+' observer source '+name,record(C/'observer'/name)['sha256']==sha)
 expected=(600_000_000,900,16777216) if phase=='compile-native' else (5_000_000,15,1048576)
 ck(phase+' exact bounds',p['aggregate_cpu_usec']==launch['cpu_allowance_usec']==expected[0] and p['wall_seconds']==launch['wall_seconds']==expected[1] and p['lethal_stderr_bytes']==launch['log_bytes']==expected[2] and p['postread_limit_per_stream_bytes']==expected[2] and p['runtime_stdio_guard_bytes']==67108864 and p['memory_bytes']==initial['MemoryMax']==17179869184 and p['swap_bytes']==initial['MemorySwapMax']==0 and initial['RuntimeMaxUSec']==expected[1]*1_000_000)
 ck(phase+' observer terminal acceptance',obs['accounting_complete'] is True and obs['comparison_eligible'] is True and obs['wrapper_exit_code']==0 and obs['stop_reason'] is None and obs['observer_error'] is None and not obs['kill_commands'])
 ck(phase+' actual terminal accounting',fa['cgroup_empty'] is True and fa['exec_main_status']==0 and fa['exec_main_code']==1 and final['ActiveState']=='inactive' and final['SubState']=='dead' and final['ControlGroup']=='' and final['MainPID']==0 and fa['cpu_usage_nsec']==final['CPUUsageNSec'])
 ck(phase+' actual usage within bounds',0<=fa['cpu_usage_nsec']<expected[0]*1000 and 0<obs['elapsed_seconds']<expected[1])
 ck(phase+' retained stderr untruncated',obs['final_report']['truncated']=='false')
 transport=result['transport'];capture(transport,phase+' observer transport');ck(phase+' transport original bound',transport['wall_limit_seconds']==p['wall_seconds']+50)
 posts=read(base/'service-post-readback.json');ck(phase+' two retained terminal queries',len(posts)==2)
 for i,q in enumerate(posts):
  raw=capture(q,phase+' saved terminal '+str(i));props=dict(line.split('=',1) for line in raw.decode().splitlines())
  ck(phase+' saved terminal properties '+str(i),props==q['properties'] and props['ActiveState']=='inactive' and props['SubState']=='dead' and props['MainPID']=='0' and props['ControlGroup']=='' and q['wall_limit_seconds']==5 and auth['unit'] in q['argv'])
 rb=result['readback'];linked(rb['stdout'],phase+' raw stdout');linked(rb['stderr'],phase+' raw stderr');ck(phase+' postread caps',rb['stdout']['bytes']<=p['postread_limit_per_stream_bytes'] and rb['stderr']['bytes']<=p['postread_limit_per_stream_bytes'])
 out=pathlib.Path(rb['stdout']['path']).read_bytes();row=dict(phase=phase,result=record(base/'result.json'),plan=pr,context=record(plan['context']['path']),source=source['source_manifest'],observer_result=record(ob/'result.json'),raw_stdout=rb['stdout'],raw_stderr=rb['stderr'],raw_status=0,accepted=True,cpu_seconds=fa['cpu_usage_nsec']/1e9,wall_seconds=obs['elapsed_seconds'],service=auth['unit'],pid=main['pid'],start_ticks=main['start_ticks'],requested_environment_extra_keys=sorted(extra),terminal_queries=record(base/'service-post-readback.json'))
 if phase=='compile-native':
  events=[json.loads(x) for x in out.decode().splitlines() if x];exes=[x for x in events if x.get('reason')=='compiler-artifact' and x.get('executable')];messages=[x for x in events if x.get('reason')=='compiler-message'];finished=[x for x in events if x.get('reason')=='build-finished']
  ck('raw compile zero structured messages',messages==[] and rb['compiler_message_count']==0 and read(ob/'compiler-messages.json')==[])
  ck('raw compile six exact executable events',len(exes)==6 and rb['actual_executable_events']==6 and read(ob/'compiler-executables.json')==exes)
  ck('raw compile one successful completion',len(finished)==1 and finished[0]['success'] is True)
  ck('compile exact feature/target command',p['argv'][2:]==['test','--locked','--offline','-p','hermit-detcore','-p','hermit','--lib','--bins','--features','hermit/third-party-backends,hermit/kvm-native-test-support','--no-run','--message-format=json'])
  for sel in plan['phase_bindings']['artifact_selectors']:
   a=rb['artifacts'][sel['id']];matches=[e for e in exes if e['manifest_path']==sel['manifest'] and e['target']['name']==sel['target'] and e['target']['kind']==sel['kind'] and e['profile']['test']==sel['test']]
   ck('compiler unique actual '+sel['id'],matches==[a['compiler_event']] and a['compiler_event']['executable']==a['emitted']['path'] and a['compiler_event']['fresh'] is False)
   retained=linked(a['retained'],'retained ELF '+sel['id']);ck('emitted/retained bytes '+sel['id'],all(a['emitted'][k]==retained[k] for k in ('bytes','mode','sha256')))
   with pathlib.Path(retained['path']).open('rb') as f:ck('ELF magic '+sel['id'],f.read(4)==b'\x7fELF')
   ck('test features '+sel['id'],a['compiler_event']['features']==([] if sel['id'] in ('detcore','syscaller') else ['dbt','default','e9patch','kvm-native-test-support','sabre','third-party-backends']))
   artifacts[sel['id']]=a
  row['compiler_message_count']=0;row['actual_executable_events']=6
 else:
  identity=phase[5:];lines=out.decode().splitlines();names=[];others=[]
  for line in lines:
   if line.endswith(': test'):names.append(line[:-6])
   elif not line or re.fullmatch(r'\d+ tests?, 0 benchmarks?',line):pass
   else:others.append(line)
  ck(phase+' raw inventory syntax',not others);ck(phase+' names exact unique',len(names)==len(set(names)) and sorted(names)==rb['actual_names'] and len(names)==expected_counts[identity] and rb['enumeration_only'] is True)
  ck(phase+' retained actual ELF argv',p['argv']==[artifacts[identity]['retained']['path'],'--list','--format','terse'] and artifacts[identity]['retained'] in plan['phase_bindings']['runtime_executables'])
  cmp=next(x for x in comparison['rows'] if x['artifact']==identity);linked(cmp['before_result'],phase+' old actual list');linked(cmp['after_result'],phase+' comparison current record');old=read(cmp['before_result']['path'])['readback']['actual_names'];removed=sorted(set(old)-set(names));added=sorted(set(names)-set(old))
  ck(phase+' actual named delta',removed==cmp['removed_names'] and added==cmp['added_names'] and len(old)==cmp['old_count'] and len(names)==cmp['actual_m2_count'])
  row['inventory_count']=len(names);inventories.append(dict(artifact=identity,names=sorted(names),old_count=len(old),added=added,removed=removed,source_results=[cmp['before_result'],cmp['after_result']]))
 phases.append(row)
result=dict(scope='Retained evidence only: compile and six direct list phases, no product execution or live source/cache inspection by reviewer',checks=checks,passed=sum(x['ok'] for x in checks),failed=sum(not x['ok'] for x in checks),source_binding=record(OWNER/'m2-source-v2/binding.json'),source_manifest=source['source_manifest'],source_base=source['base'],source_tree=source['expected_composed_tree'],artifacts=artifacts,phases=phases,inventories=inventories,files=list(files.values()),limitations=['Historical full input checks are successful retained caller assertion-path evidence. This audit compares recorded before/context/SCM results and concrete retained files, not live source or external dependency trees.','No guest, integration, full Nextest, canonical helper normal executable, or final-source qualification is inferred.','Additional service/inherited environment keys are recorded; requested values and protected override checks hold, not an empty inherited environment claim.'])
with (OUT/'COMPILE-LISTS-READBACK.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
print(json.dumps(dict(passed=result['passed'],failed=result['failed'],failed_checks=[x for x in checks if not x['ok']],files=len(files),artifact_ids=list(artifacts),phases=[{k:r[k] for k in ('phase','cpu_seconds','wall_seconds')} for r in phases]),indent=2))
