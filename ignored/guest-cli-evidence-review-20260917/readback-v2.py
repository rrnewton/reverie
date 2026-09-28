"""Independent file/record authentication only; no product command execution."""
import collections, hashlib, json, os, stat, subprocess, time
from pathlib import Path
O=Path(__file__).resolve().parent
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
report={'scope':'independent read-only record/source/ELF authentication; no product or reader-control execution','accepted':False}
try:
 frozen=[('readback-guest-cli-v1.py','cb07659afd656d90e08c03d8c27874656d940995cadfdfecc8a883e2e2051ee6'),('readback-guest-cli-v2.py','f6228effef242c3d2934e4a829a9205cd0a1ab8a6089df8918a2347e370771ad'),('guest-cli-readback-v2-binding.json','b027c2c0688fb22426083b7e36fcfa4f6610c1797c40e616eb94526aa6a38cd7'),('guest-cli-readback-v2.patch','ee2e7a7a42a321dea85ea8db8472f59b5549a4f2b38b2b05492be6e9c0d13e5f')]
 for name,digest in frozen:
  b=read(C/name);require(sha(b)==digest,'reviewed exact '+name)
  with (O/name).open('xb') as f:f.write(b)
 binding=j(C/'guest-cli-readback-v2-binding.json')
 for row in binding['inputs']:bound(row)
 report['explicit_binding_count']=len(binding['inputs'])
 contexts=[]
 for name,expected_error in [('original-kvm-cli',"RuntimeError('missing/extra/ignored Nextest terminal identities')"),('original-kvm-cli-typed-results',"RuntimeError('original tests remain failed')")]:
  control=C/'guest-controls-run-2'/name;original=j(control/'result.json');bound(original['plan']);plan=j(original['plan']['path']);bound(plan['context']);context=j(plan['context']['path']);contexts.append(context)
  require(original['accepted'] is False and original['raw_status']==0 and original['terminal_authenticated'] is True and original['final_source_inputs_unchanged'] is True and original['error']==expected_error,'preserved original refusal '+name)
  require(context['scm']['head']==HEAD and context['scm']['tree']==TREE,'source identity '+name)
  require(j(control/'before.json')=={'source':context['source_manifest'],'inputs':context['inputs'],'executables':context['executables']},'retained before source/input snapshot '+name)
  for side in ['before','after']:require(j(control/('scm-'+side+'-binding.json'))==context['scm'],'retained SCM '+name+' '+side)
  for row in [context['source_manifest'],*context['inputs'],*context['executables'],*context['recursive_submodule_inputs']]:bound(row)
  for manifest in context['recursive_submodule_inputs']:
   for row in j(manifest['path']):bound(row)
  for row in context['input_symlinks']:require(Path(row['path']).is_symlink() and os.readlink(row['path'])==row['target'],'symlink input '+row['path'])
  for path in context['absent_inputs']:require(not Path(path).exists() and not Path(path).is_symlink(),'absent override '+path)
  bound(original['observer_result']);observed=j(original['observer_result']['path']);out=Path(plan['output'])
  require(observed['wrapper_exit_code']==0 and observed['stop_reason'] is None and observed['observer_error'] is None and observed['comparison_eligible'] is True and observed['accounting_complete'] is True,'accepted actual service '+name)
  final=observed['final_accounting'];props=final['properties'];require(final['cgroup_empty'] is True and props['ActiveState']=='inactive' and props['MainPID']==0 and props['ControlGroup']=='' and props['ExecMainStatus']==0,'historical inactive/empty service '+name)
  require(final['cpu_usage_nsec']<plan['phase']['aggregate_cpu_usec']*1000 and observed['elapsed_seconds']<=plan['phase']['wall_seconds']+40,'unchanged measured bounds '+name)
  require(observed['launch']['cpu_allowance_usec']==plan['phase']['aggregate_cpu_usec'] and observed['launch']['wall_seconds']==plan['phase']['wall_seconds'] and observed['launch']['log_bytes']==plan['phase']['lethal_stderr_bytes'],'observer bounds equal plan '+name)
  require(plan['phase']['memory_bytes']==16*1024**3 and plan['phase']['swap_bytes']==0,'memory/swap plan '+name)
  require(observed['launch']['payload']['arguments']==['-B',str(C/'admit.py'),original['plan']['path'],original['plan']['sha256']],'actual launched plan '+name)
  for stream in ['stdout','stderr']:bound(original['transport'][stream])
  require(not original['transport']['forced'] and original['transport']['returncode']==0,'outer transport '+name)
  admission=j(out/'admission.json');require(admission['phase_plan_sha256']==original['plan']['sha256'] and admission['argv']==plan['phase']['argv'],'actual argument admission '+name)
  require(admission['environment']==plan['environment'],'actual fixed environment '+name)
  require(admission['supervisor_pid']==props['ExecMainPID'] and props['Id'] in admission['cgroup'],'same actual service '+name)
  payload=j(out/'payload-exit.json');require(payload==original['payload_exit'] and payload['returncode']==0 and payload['reaped'] is True and not payload['local_wait_timed_out'],'actual payload reaped '+name)
  if name=='original-kvm-cli':require(admission['kvm_required'] is True and admission['kvm']['api_version']==12,'actual KVM API 12 admission')
  for n in range(2):
   raw=query(['/usr/bin/systemctl','--user','show',props['Id'],'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'],name+'-fresh-'+str(n),8192)
   now=dict(line.split('=',1) for line in raw.decode().splitlines() if '=' in line)
   require(now.get('ActiveState') in ['inactive','failed'] and now.get('MainPID')=='0' and now.get('ControlGroup')=='','fresh inactive/empty '+name+' '+str(n))
  phases.append({'name':name,'original_refusal':record(control/'result.json'),'plan':record(Path(original['plan']['path'])),'observer':record(Path(original['observer_result']['path'])),'stdout':record(out/'stdout'),'stderr':record(out/'stderr'),'payload':record(out/'payload-exit.json'),'admission':record(out/'admission.json'),'service':props['Id'],'supervisor_pid':props['ExecMainPID'],'cpu_seconds':final['cpu_usage_nsec']/1e9,'wall_seconds':observed['elapsed_seconds'],'actual_argv':admission['argv'],'original_refusal_preserved':True})
 context=contexts[0];source=j(context['source_manifest']['path']);require(len(source)==1732,'full 1732 source entries')
 tree_raw=query(['/usr/bin/git','ls-tree','-rz',HEAD],'git-tree')
 tree={}
 for entry in tree_raw.split(b'\0'):
  if entry:
   meta,path=entry.split(b'\t',1);mode,kind,oid=meta.decode().split();tree[path.decode()]={'mode':mode,'object':oid,'kind':kind}
 require(len(tree)==len(source) and {r['path'] for r in source}==set(tree),'full tree paths')
 for row in source:
  path=H/row['path'];t=tree[row['path']];require(t['mode']==row['mode'],'source Git mode '+row['path'])
  if row['mode']=='160000':continue
  if row['mode']=='120000':require(path.is_symlink() and sha(os.fsencode(os.readlink(path)))==row['sha256'],'source symlink '+row['path'])
  else:require(record(path)['sha256']==row['sha256'] and bool(path.stat().st_mode&0o111)==(row['mode']=='100755'),'current source bytes '+row['path'])
 for key,args in [('head',['rev-parse','HEAD']),('tree',['rev-parse','HEAD^{tree}']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
  raw=query(['/usr/bin/git',*args],'git-'+key);actual=sha(raw) if key=='index' else raw.decode().strip();require(actual==context['scm'][key],'live SCM '+key)
 plan=j(C/'guest-v5-original-kvm-cli-plan.json');out=Path(plan['output']);names=plan['phase_bindings']['selected_names'];expected={'hermit::cli$'+n for n in names};require(len(names)==len(expected)==24,'exact original 24 selection')
 inv_result=j(C/'guest-controls-run-1/original-kvm-cli-inventory/result.json');require(inv_result['accepted'] is True,'actual accepted inventory');bound(inv_result['readback']['stdout']);inv=j(inv_result['readback']['stdout']['path']);identities={}
 for bid,suite in inv['rust-suites'].items():
  require(bid==suite['binary-id'] and suite['status']=='listed','actual listed suite '+bid)
  for name,row in suite['testcases'].items():identities[bid+'$'+name]=row
 selected={k for k,v in identities.items() if v['filter-match']['status']=='matches'};require(selected==expected and all(identities[k]['ignored'] is False for k in expected),'bound actual inventory selection')
 extra='hermit::cli$run_dbt_verifies_fresh_physical_workdirs';require(identities[extra]['ignored'] is True and identities[extra]['filter-match']=={'status':'mismatch','reason':'ignored'},'ignored extra derived from actual inventory')
 events=[json.loads(line,object_pairs_hook=unique) for line in read(out/'stdout').splitlines() if line];testrows=[e for e in events if e.get('type')=='test'];starts={};ends={}
 for i,e in enumerate(testrows):
  name=e['name'];kind=e['event'];require(kind in ['started','ok','failed','ignored'],'known actual test event')
  if kind=='started':require(name not in starts,'one actual start '+name);starts[name]=i
  else:require(name in starts and starts[name]<i and name not in ends,'one actual ordered terminal '+name);ends[name]=kind
 require(set(starts)==set(ends)==expected|{extra},'closed exact actual event identity population');require(all(ends[k]=='ok' for k in expected) and ends[extra]=='ignored','actual 24 passes and bound ignored entry')
 typed_out=Path(j(C/'guest-v5-original-kvm-cli-typed-results-plan.json')['output']);counts=j(typed_out/'counts.json');cpu=j(typed_out/'cpu.json')
 require(counts['schema']==2 and counts['executed_tests']==24 and counts['filtered_tests']==89 and len(counts['results'])==24,'production typed CLI counts')
 require({r['id'] for r in counts['results']}==expected and all(r['result']=='pass' and r['attempts']==1 for r in counts['results']),'production typed exact first passes')
 attempts=cpu['attempts'];require(cpu['schema']==3 and len(attempts)==24 and {r['identity']['test'] for r in attempts}==set(names),'production 24 CPU attempts')
 raw_attempts=list((out/'attempts').iterdir());require(len(raw_attempts)==24,'raw CPU attempt file count')
 for row in attempts:
  require(row['identity']=={'package':'hermit','binary':'hermit::cli','test':row['identity']['test'],'attempt':1} and row['completion']=={'kind':'exit','code':0} and row['run_id']==cpu['run_id'] and row['cpu_source']=='wait4-subtree','CPU actual first attempt '+row['identity']['test'])
  path=out/'attempts'/(row['key']+'.json');require(path in raw_attempts and j(path)==row,'raw typed CPU equality '+row['identity']['test'])
 cli_source=read(H/'hermit-cli/tests/cli.rs').decode();require('#[ignore = "requires the pinned-root isolation validation node and its /test marker"]\nfn run_dbt_verifies_fresh_physical_workdirs()' in cli_source,'original unchanged ignored source declaration')
 warnings=[x for x in read(out/'stderr').decode().splitlines() if x.startswith('warning:')];require(len(warnings)==30 and len(set(warnings))==1,'retained exact warning population')
 report.update(accepted=True,source_entries=1732,actual_inventory_count=inv['test-count'],selected_names=names,raw_event_counts={str(k):v for k,v in collections.Counter((e.get('type'),e.get('event')) for e in events).items()},ignored_unselected={'identity':extra,'inventory':identities[extra]},production_counts=counts,production_cpu_attempts=24,warning=warnings[0],caller_controls='11 pure negative controls source-reviewed; execution remains with owner',v1_finding='Unmatched or duplicate test starts were discarded; corrected by exact closed event pairs in v2.')
except BaseException as error:report['error']=repr(error)
finally:
 report.update(elapsed_readback_seconds=time.monotonic()-start,phases=phases,checks=checks,unique_hashed_files=len(cache),checked_file_records=checked)
 save(O/'READBACK-v2.json',report)
 print(json.dumps({'accepted':report['accepted'],'error':report.get('error'),'checks':len(checks),'unique_hashed_files':len(cache),'elapsed_seconds':report['elapsed_readback_seconds'],'readback_sha256':sha((O/'READBACK-v2.json').read_bytes())}))
