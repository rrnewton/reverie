"""Independent file/record authentication only; no product command execution."""
import collections, hashlib, json, os, stat, subprocess, time
from pathlib import Path
O=Path(__file__).resolve().parent/'completion-3'
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
report={'scope':'Read-only completed KVM recovery and final typed report-reader authentication; no guest execution','accepted':False}
contexts=[]; originals={};plans={};observed_phases={}
NAMES=['pthread-canonical-kvm-typed-read']
def bind_nested(value):
 if isinstance(value,dict):
  if {'path','bytes','mode','sha256'}.issubset(value):bound(value)
  else:
   for child in value.values():bind_nested(child)
 elif isinstance(value,list):
  for child in value:bind_nested(child)
try:
 for name in NAMES:
  control=C/'guest-controls-run-4'/name;original=j(control/'result.json');originals[name]=original;bound(original['plan']);plan=j(original['plan']['path']);plans[name]=plan;bound(plan['context']);context=j(plan['context']['path']);contexts.append(context)
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
  else:
   rec=record(path);require(bool(path.stat().st_mode&0o111)==(row['mode']=='100755'),'actual source mode '+row['path']);h=hashlib.sha1(b'blob '+str(rec['bytes']).encode()+b'\0')
   with path.open('rb') as f:
    for chunk in iter(lambda:f.read(1024*1024),b''):h.update(chunk)
   require(rec['sha256']==row['sha256'] and h.hexdigest()==t['object'],'actual streamed Git source blob '+row['path']);continue
  require(sha(b)==row['sha256'] and hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()==t['object'],'actual Git source blob '+row['path'])
 for key,args in [('head',['rev-parse','HEAD']),('tree',['rev-parse','HEAD^{tree}']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
  raw=query(['/usr/bin/git',*args],'git-'+key);actual=sha(raw) if key=='index' else raw.decode().strip();require(actual==context['scm'][key],'fresh live SCM '+key)

 previous=j(Path(__file__).resolve().parent/'attempt-3/READBACK.json');require(previous['accepted'] is True and sha(read(Path(__file__).resolve().parent/'attempt-3/READBACK.json'))=='8cd9dff852d8ad81131dc648d79c290bdcb568bd171d5bac7fcf8e41488b71df','prior complete independent source/raw readback')
 for phase in previous['phases']:
  for key in ['original_result','plan','context','observer','stdout','stderr','payload','admission']:bound(phase[key])
 caller=C/'readback-kvm-canonical-v1.py';binding_path=C/'kvm-canonical-readback-v1-binding.json';recovery_path=C/'kvm-canonical-readback-v1/result.json'
 require(record(caller)['sha256']=='472baa2f975599260dfc9c25035c02d8b3d9e43e039bb4bc80925b0d760cbaff','exact recovery caller')
 require(record(binding_path)['sha256']=='5598d8127ee19112f8e6ccfc5c68540c2f7c54759acc98e8e433428a40c47107','exact recovery binding')
 binding=j(binding_path)
 for row in binding['inputs']:bound(row)
 require(record(recovery_path)['sha256']=='f3e447acab832862a5cc82019c8316ec6f3d0115944d4948bdcb9e8bebb44870','exact completed recovery result')
 recovery=j(recovery_path);bind_nested(recovery)
 require(recovery['accepted'] is True and recovery['terminal_authenticated'] is True and recovery['raw_status']==0 and recovery['final_source_inputs_unchanged'] is True and recovery['original_refusal_preserved'] is True and recovery['product_rerun'] is False and recovery['execution_rerun'] is False,'exact read-only recovery success')
 original=j(C/'guest-controls-run-3/pthread-canonical-kvm/result.json')
 require(original['accepted'] is False and recovery['readback']==original['readback'],'preserved false original and exact unchanged output interpretation')
 require(recovery['readback']['report']['compared_log_messages']=={'left':219,'right':219},'recovery exact equal INFO count')
 for when in ['before','after']:require(j(C/'kvm-canonical-readback-v1'/('scm-'+when+'-binding.json'))==context['scm'],'recovery SCM '+when)
 for file in sorted((C/'kvm-canonical-readback-v1').iterdir()):
  if file.name.startswith('service-post') and file.suffix=='.json':
   q=j(file)
   if isinstance(q,list):
    require(len(q)==2 and all(x.get('returncode')==0 and x.get('forced') is False for x in q),'both summarized terminal queries');bind_nested(q);continue
   require(q['returncode']==0 and q['forced'] is False,'recovery retained terminal query '+file.name);bind_nested(q)
   raw=read(q['stdout']['path']);v=dict(l.split('=',1) for l in raw.decode().splitlines() if '=' in l);require(v['ActiveState']=='inactive' and v['MainPID']=='0' and v['ControlGroup']=='','recovery query inactive/empty '+file.name)
 final=originals['pthread-canonical-kvm-typed-read'];require(final['readback']['producer_result']==record(recovery_path),'final reader consumes exact completed recovery')
 plan=plans['pthread-canonical-kvm-typed-read'];out=Path(plan['output']);argv=plan['phase']['argv'];require(argv[1:]==['--json','canonical-match',str(C/'observer/guest-v6-pthread-canonical-kvm/verification.json')],'actual final typed reader command')
 typed=j(out/'stdout');require(typed==original['readback']['report'],'actual current typed report output preserves full report')
 require(plan['phase']['aggregate_cpu_usec']==5000000 and plan['phase']['wall_seconds']==15 and read(out/'stderr')==b'','unchanged final typed reader bounds and diagnostics')
 report.update(accepted=True,prior_readback=record(Path(__file__).resolve().parent/'attempt-3/READBACK.json'),recovery_result=record(recovery_path),recovery_caller=record(caller),recovery_binding=record(binding_path),recovery_binding_inputs=len(binding['inputs']),final_typed_report=record(out/'stdout'),original_kvm_refusal=record(C/'guest-controls-run-3/pthread-canonical-kvm/result.json'),original_refusal_preserved=True,no_product_or_guest_rerun=True)
except BaseException as error:report['error']=repr(error)
finally:
 report.update(elapsed_readback_seconds=time.monotonic()-start,phases=phases,check_count=len(checks),failed_checks=[x for x in checks if not x['ok']],unique_hashed_files=len(cache),checked_file_records=list({x['path']:x for x in checked}.values()))
 save(O/'READBACK.json',report)
 print(json.dumps({'accepted':report['accepted'],'error':report.get('error'),'checks':len(checks),'elapsed_seconds':report['elapsed_readback_seconds'],'readback_sha256':sha((O/'READBACK.json').read_bytes())}))
