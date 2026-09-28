"""Independent file/record authentication only; no product command execution."""
import collections, hashlib, json, os, stat, subprocess, time
from pathlib import Path
O=Path(__file__).resolve().parent/'attempt-1'
O.mkdir(mode=0o700)
H=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917')
C=H/'ignored/prejoin-main-callers-v11'
HEAD='5bc8dfd9a2c024426aaefe504f250a71394bbb09'
TREE='d828fca5783a8183e1cff1d42937a2beb5d4f743'
checks=[]; cache={}; checked=[]; phases=[]; start=time.monotonic()
def require(value,label):
 checks.append({'check':label,'ok':bool(value)})
 if not value: raise RuntimeError(label)
def unique(pairs):
 d={}
 for k,v in pairs:
  if k in d:raise RuntimeError('duplicate JSON key '+k)
  d[k]=v
 return d
def read(p):
 p=Path(p)
 with p.open('rb') as f:b=f.read(16*1024*1024+1)
 require(len(b)<=16*1024*1024,'bounded JSON/text read '+str(p))
 return b
def j(p):return json.loads(read(p),object_pairs_hook=unique)
def sha(b):return hashlib.sha256(b).hexdigest()
def record(p):
 p=Path(p);s=p.lstat();key=(str(p),s.st_dev,s.st_ino,s.st_size,s.st_mtime_ns,s.st_ctime_ns,stat.S_IMODE(s.st_mode))
 require(stat.S_ISREG(s.st_mode),'regular bound file '+str(p))
 if key not in cache:
  h=hashlib.sha256()
  with p.open('rb') as f:
   for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
  t=p.lstat();require((s.st_dev,s.st_ino,s.st_size,s.st_mtime_ns,s.st_ctime_ns,s.st_mode)==(t.st_dev,t.st_ino,t.st_size,t.st_mtime_ns,t.st_ctime_ns,t.st_mode),'stable while hashing '+str(p))
  cache[key]={'path':str(p),'bytes':s.st_size,'mode':stat.S_IMODE(s.st_mode),'sha256':h.hexdigest()}
 return cache[key]
def bound(row):
 r=record(row['path']);require(all(r[k]==row[k] for k in ['path','bytes','mode','sha256']),'bound identity '+row['path']);checked.append(r);return r
def save(p,x):
 with p.open('x') as f:json.dump(x,f,indent=2);f.write('\n')
def query(argv,prefix,limit=16*1024*1024):
 q=subprocess.run(argv,cwd=H,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=5,env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'})
 require(q.returncode==0 and len(q.stdout)<=limit and len(q.stderr)<=limit,'read-only query '+str(argv))
 for side,b in [('stdout',q.stdout),('stderr',q.stderr)]:
  with (O/(prefix+'.'+side)).open('xb') as f:f.write(b)
 return q.stdout
import re
report={'scope':'Read-only authentication of original pthread and per-backend canonical repeat evidence; no guest, product reader, test, build, or mutation','accepted':False}
contexts=[]; originals={}; plans={}; observed_phases={}
NAMES=['original-pthread-inventory','original-pthread-binary-map','original-pthread','original-pthread-typed-results','pthread-canonical-ptrace','pthread-canonical-ptrace-typed-read','pthread-canonical-kvm']
def bind_nested(value):
 if isinstance(value,dict):
  if {'path','bytes','mode','sha256'}.issubset(value):bound(value)
  else:
   for child in value.values():bind_nested(child)
 elif isinstance(value,list):
  for child in value:bind_nested(child)
try:
 for name in NAMES:
  control=C/'guest-controls-run-3'/name;original=j(control/'result.json');originals[name]=original;bound(original['plan']);plan=j(original['plan']['path']);plans[name]=plan;bound(plan['context']);context=j(plan['context']['path']);contexts.append(context)
  refuse=name=='pthread-canonical-kvm'
  require(original['accepted'] is (not refuse) and original['raw_status']==0 and original['terminal_authenticated'] is True,'retained phase classification '+name)
  if refuse:
   require(original['source_error']=="RuntimeError('source SCM readback failed')" and original['final_source_inputs_unchanged'] is False and 'error' not in original,'preserved exact SCM refusal')
   q=j(control/'scm-after-head.json');require(q['argv']==['/usr/bin/git','-C',str(H),'rev-parse','HEAD'] and q['forced'] is True and q['reason']=='outer_wall_limit' and q['returncode']==-15 and q['wall_limit_seconds']==5,'original five-second SCM query timeout')
   bind_nested(q);report['preserved_scm_refusal']={'result':record(control/'result.json'),'query':q,'current_postcheck_is_separate':True}
  else:
   require(original['final_source_inputs_unchanged'] is True,'retained final source check '+name)
   require(j(control/'scm-after-binding.json')==context['scm'],'retained after SCM '+name)
  require(context['scm']['head']==HEAD and context['scm']['tree']==TREE,'source identity '+name)
  require(j(control/'before.json')=={'source':context['source_manifest'],'inputs':context['inputs'],'executables':context['executables']},'retained input snapshot '+name)
  require(j(control/'scm-before-binding.json')==context['scm'],'retained before SCM '+name)
  for row in [context['source_manifest'],context['observer'],*context['inputs'],*context['executables'],*context['recursive_submodule_inputs']]:bound(row)
  for manifest in context['recursive_submodule_inputs']:
   for row in j(manifest['path']):bound(row)
  for row in context['input_symlinks']:require(Path(row['path']).is_symlink() and os.readlink(row['path'])==row['target'],'symlink input '+row['path'])
  for path in context['absent_inputs']:require(not Path(path).exists() and not Path(path).is_symlink(),'absent override '+path)
  bind_nested(plan['phase_bindings']);bound(plan['frozen_template']);bind_nested(original['readback'])
  bound(original['observer_result']);observed=j(original['observer_result']['path']);observed_phases[name]=observed;out=Path(plan['output']);phase=plan['phase']
  require(observed['wrapper_exit_code']==0 and observed['stop_reason'] is None and observed['observer_error'] is None and observed['comparison_eligible'] is True and observed['accounting_complete'] is True,'actual service accepted '+name)
  final=observed['final_accounting'];props=final['properties'];require(final['cgroup_empty'] is True and props['ActiveState']=='inactive' and props['MainPID']==0 and props['ControlGroup']=='' and props['ExecMainStatus']==0,'historical inactive/empty '+name)
  service=observed['authenticated_service'];initial=service['initial_properties'];process=service['main_process']
  require(initial['MemoryMax']==phase['memory_bytes']==16*1024**3 and initial['MemorySwapMax']==phase['swap_bytes']==0 and initial['RuntimeMaxUSec']==phase['wall_seconds']*1000000,'actual service memory/swap/wall '+name)
  require(final['cpu_usage_nsec']<phase['aggregate_cpu_usec']*1000 and observed['elapsed_seconds']<=phase['wall_seconds']+40,'measured resource limits '+name)
  require(observed['launch']['cpu_allowance_usec']==phase['aggregate_cpu_usec'] and observed['launch']['wall_seconds']==phase['wall_seconds'] and observed['launch']['log_bytes']==phase['lethal_stderr_bytes']==16*1024**2,'observer planned limits '+name)
  require(observed['launch']['payload']['arguments']==['-B',str(C/'admit.py'),original['plan']['path'],original['plan']['sha256']],'actual launched plan '+name)
  require(not original['transport']['forced'] and original['transport']['returncode']==0,'outer transport '+name);bind_nested(original['transport'])
  admission=j(out/'admission.json');require(admission['phase_plan_sha256']==original['plan']['sha256'] and admission['argv']==phase['argv'] and admission['environment']==plan['environment'],'exact actual argv and fixed env '+name)
  require(admission['supervisor_pid']==process['pid']==initial['ExecMainPID']==props['ExecMainPID'] and process['cgroup']==initial['ControlGroup'] and props['Id'] in admission['cgroup'],'actual same service PID/cgroup '+name)
  payload=j(out/'payload-exit.json');require(payload==original['payload_exit'] and payload['returncode']==0 and payload['reaped'] is True and not payload['local_wait_timed_out'],'actual payload reaped '+name)
  if name in ['original-pthread','pthread-canonical-kvm']:require(admission['kvm_required'] is True and admission['kvm']['api_version']==12,'actual API12 hardware admission '+name)
  for n in range(2):
   raw=query(['/usr/bin/systemctl','--user','show',props['Id'],'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'],name+'-fresh-'+str(n),8192)
   now=dict(line.split('=',1) for line in raw.decode().splitlines() if '=' in line)
   require(now.get('ActiveState') in ['inactive','failed'] and now.get('MainPID')=='0' and now.get('ControlGroup')=='','fresh inactive/empty '+name+' '+str(n))
  phases.append({'name':name,'original_result':record(control/'result.json'),'original_accepted':original['accepted'],'plan':record(Path(original['plan']['path'])),'context':record(Path(plan['context']['path'])),'observer':record(Path(original['observer_result']['path'])),'stdout':record(out/'stdout'),'stderr':record(out/'stderr'),'payload':record(out/'payload-exit.json'),'admission':record(out/'admission.json'),'service':props['Id'],'supervisor_pid':process['pid'],'start_ticks':process['start_ticks'],'cpu_seconds':final['cpu_usage_nsec']/1e9,'wall_seconds':observed['elapsed_seconds'],'actual_argv':admission['argv'],'source_inputs':len(context['inputs']),'executables':len(context['executables'])})
 context=contexts[0];source=j(context['source_manifest']['path']);require(len(source)==1732,'full 1732 source entries')
 tree_raw=query(['/usr/bin/git','ls-tree','-rz',HEAD],'git-tree-entries');tree={}
 for entry in tree_raw.split(b'\0'):
  if entry:
   meta,path=entry.split(b'\t',1);mode,kind,oid=meta.decode().split();tree[path.decode()]={'mode':mode,'object':oid,'kind':kind}
 require(len(tree)==len(source) and {r['path'] for r in source}==set(tree),'full source path set')
 for row in source:
  path=H/row['path'];t=tree[row['path']];require(t['mode']==row['mode'],'exact source Git mode '+row['path'])
  if row['mode']=='160000':require(path.is_dir(),'gitlink exists '+row['path']);continue
  if row['mode']=='120000':require(path.is_symlink(),'source symlink '+row['path']);b=os.fsencode(os.readlink(path))
  else:b=read(path);require(bool(path.stat().st_mode&0o111)==(row['mode']=='100755'),'actual source mode '+row['path'])
  require(sha(b)==row['sha256'] and hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()==t['object'],'actual Git source blob '+row['path'])
 for key,args in [('head',['rev-parse','HEAD']),('tree',['rev-parse','HEAD^{tree}']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
  raw=query(['/usr/bin/git',*args],'git-'+key);actual=sha(raw) if key=='index' else raw.decode().strip();require(actual==context['scm'][key],'fresh live SCM '+key)
 inv=j(originals['original-pthread-inventory']['readback']['stdout']['path']);identities={}
 for bid,suite in inv['rust-suites'].items():
  require(bid==suite['binary-id'] and suite['status']=='listed','actual suite listed '+bid)
  for name,row in suite['testcases'].items():identities[bid+'$'+name]=row
 selected='hermit::kvm_harder$kvm_matches_ptrace_for_pthread_lifecycle';ignored='hermit::cli$run_dbt_verifies_fresh_physical_workdirs'
 require(inv['test-count']==len(identities)==499,'actual complete 499 inventory')
 require({k for k,v in identities.items() if v['filter-match']['status']=='matches'}=={selected} and identities[selected]['ignored'] is False,'actual one selected nonignored method')
 require(identities[ignored]['ignored'] is True and identities[ignored]['filter-match']=={'status':'mismatch','reason':'ignored'},'actual ignored DBT metadata')
 plan=plans['original-pthread'];out=Path(plan['output']);events=[json.loads(line,object_pairs_hook=unique) for line in read(out/'stdout').splitlines() if line]
 require(len(events)==7,'retained exact seven event stream')
 require([(x['type'],x['event']) for x in events]==[('suite','started'),('test','started'),('test','ok'),('suite','ok'),('suite','started'),('test','started'),('suite','ok')],'exact producer event ordering')
 require(events[1]['name']==events[2]['name']==selected and events[5]['name']==ignored,'exact selected and unselected identities')
 require(events[0]['nextest']==events[3]['nextest']=={'crate':'hermit','test_binary':'kvm_harder','kind':'test'} and events[4]['nextest']==events[6]['nextest']=={'crate':'hermit','test_binary':'cli','kind':'test'},'exact closed suite metadata')
 for index,counts in [(3,{'passed':1,'failed':0,'ignored':0,'measured':0,'filtered_out':3}),(6,{'passed':0,'failed':0,'ignored':1,'measured':0,'filtered_out':111})]:require(all(events[index][k]==v for k,v in counts.items()),'exact typed terminal suite '+str(index))
 typedout=Path(plans['original-pthread-typed-results']['output']);counts=j(typedout/'counts.json');cpu=j(typedout/'cpu.json')
 require(counts=={'schema':2,'executed_tests':1,'filtered_tests':115,'results':[{'attempts':1,'id':selected,'result':'pass'}]},'production exact count record')
 require(cpu['schema']==3 and len(cpu['attempts'])==1,'one CPU first attempt');a=cpu['attempts'][0]
 require(a['identity']=={'package':'hermit','binary':'hermit::kvm_harder','test':selected.split('$')[1],'attempt':1} and a['completion']=={'kind':'exit','code':0} and a['run_id']==cpu['run_id'] and a['cpu_source']=='wait4-subtree','exact original CPU identity')
 attemptfiles=list((out/'attempts').iterdir());require(len(attemptfiles)==1 and j(attemptfiles[0])==a and attemptfiles[0].name==a['key']+'.json','one actual CPU record equals typed writer')
 require(sum(r['user_cpu_usec']+r['system_cpu_usec'] for r in a['wait4'])==a['cpu_usage_usec']==542739 and all(r['status']==0 for r in a['wait4']),'CPU accounting exact wait4 sum')
 warnings=[x for x in read(out/'stderr').decode().splitlines() if x.startswith('warning:')];require(len(warnings)==7 and len(set(warnings))==1,'seven retained identical nextest warnings')
 report.update(original_inventory=record(Path(originals['original-pthread-inventory']['readback']['stdout']['path'])),original_events=events,typed_counts=counts,typed_cpu=cpu,ignored_inventory=identities[ignored],warnings=warnings)
 canonical={}
 for backend,count in [('ptrace',229),('kvm',219)]:
  name='pthread-canonical-'+backend;plan=plans[name];out=Path(plan['output']);r=j(out/'verification.json');template=j(plan['frozen_template']['path'])
  require(r==originals[name]['readback']['report'],'raw complete canonical report equals readback '+backend)
  require(r['comparison']==template['strict_policy'],'complete original canonical policy '+backend)
  require(r['verified'] is True and r['bitwise_parity'] is True and r['verdict']=='matched' and r['no_result_reason'] is None and r['infrastructure_error'] is None,'canonical exact success fields '+backend)
  require(r['compared_log_messages']=={'left':count,'right':count} and count>0,'explicit equal positive compared INFO counts '+backend)
  expected=b'threads=4 total=10\n';output={'exit_code':0,'signal':None,'stdout_sha256':sha(expected),'stdout_bytes':len(expected),'stderr_sha256':sha(b''),'stderr_bytes':0}
  require(read(out/'stdout')==expected and r['compared_outputs']=={'left':output,'right':output} and r['guest_exit_code']==0 and r['guest_signal'] is None,'exact original guest output/status per run '+backend)
  require(r['runtime']['run1']==r['runtime']['run2'] and r['runtime']['run1']['scheduler_turns']==19 and r['runtime']['run1']['syscalls']==81,'actual per-backend runtime totals '+backend)
  logs=originals[name]['readback']['retained_info_logs'];require(len(logs)==2 and {x['path'] for x in logs}=={str(x) for x in (out/'verify-logs').iterdir()},'exact two retained log files '+backend)
  for row in logs:
   bound(row);lines=read(row['path']).decode().splitlines();require(len(lines)==count and all(re.match(r'^\S+\s+INFO ',line) for line in lines),'full retained INFO record count '+backend)
  require(':: comparison=BitwiseInfoV1 relaxations=none' in read(out/'stderr').decode(),'no relaxation diagnostic '+backend)
  require(plan['phase']['aggregate_cpu_usec']==30000000 and plan['phase']['wall_seconds']==60,'unchanged canonical runtime bounds '+backend)
  argv=plan['phase']['argv'];require(argv[1:]==['--log=info','run','--backend='+backend,'--strict','--verify','--verify-strict','--base-env=minimal','--mount=type=tmpfs,target=/test','--workdir=/test','--keep-logs','--verify-log-dir',str(out/'verify-logs'),'--verify-json',str(out/'verification.json'),'--',contexts[0]['actual_pthread_guest_path']],'exact canonical argv '+backend)
  canonical[backend]={'raw_report':record(out/'verification.json'),'report':r,'logs':logs,'original_accepted':originals[name]['accepted']}
 guest=record(Path(contexts[0]['actual_pthread_guest_path']));require(guest['sha256']=='d12d7e69e764aa0c351c1ee694d754b88ca1eec3e86b130b25ebd86dcd3947b5' and guest['bytes']==20816 and guest['mode']==493,'actual original-method guest artifact')
 require(guest in plans['pthread-canonical-ptrace']['phase_bindings']['runtime_executables'] and guest in plans['pthread-canonical-kvm']['phase_bindings']['runtime_executables'],'same guest bound across both canonical phases')
 report.update(accepted=True,canonical=canonical,guest=guest,source_entries=1732,actual_inventory_count=499,scope_limit='KVM runtime/report authenticated, original SCM timeout refusal preserved; separate owner recovery and final KVM typed reader remain outside this readback. No cross-backend INFO comparison.')
except BaseException as error:report['error']=repr(error)
finally:
 report.update(elapsed_readback_seconds=time.monotonic()-start,phases=phases,check_count=len(checks),failed_checks=[x for x in checks if not x['ok']],unique_hashed_files=len(cache),checked_file_records=list({x['path']:x for x in checked}.values()))
 save(O/'READBACK.json',report)
 print(json.dumps({'accepted':report['accepted'],'error':report.get('error'),'checks':len(checks),'unique_hashed_files':len(cache),'elapsed_seconds':report['elapsed_readback_seconds'],'readback_sha256':sha((O/'READBACK.json').read_bytes())}))
