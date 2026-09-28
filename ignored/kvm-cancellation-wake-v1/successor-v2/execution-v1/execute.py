import datetime,hashlib,json,os,pathlib,signal,subprocess,sys,time
root=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
prepared=root/'ignored/kvm-cancellation-wake-v1/successor-v2'
out=prepared/'execution-v1'
context=json.loads((out/'CONTEXT-BEFORE.json').read_text())
def dump(path,obj):
 path.write_text(json.dumps(obj,indent=2)+'\n')
def identity(record):
 path=pathlib.Path(record['path'])
 with path.open('rb') as f: digest=hashlib.file_digest(f,'sha256').hexdigest()
 if digest!=record['sha256'] or path.stat().st_size!=record['bytes']:
  raise RuntimeError('input changed: '+str(path))
def inputs():
 for record in [context['lockfile'],context['caller'],context['contract'],*[x['identity'] for x in context['tools']],*context['source'].values()]: identity(record)
def inspect(unit):
 argv=['/usr/bin/systemctl','--user','show',unit,'--property=LoadState,ActiveState,SubState,ControlGroup,MainPID,Result,ExecMainCode,ExecMainStatus,CPUUsageNSec,MemoryPeak']
 r=subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10)
 text=r.stdout.decode(); props=dict(line.split('=',1) for line in text.splitlines() if '=' in line)
 return {'argv':argv,'raw_status':r.returncode,'stdout':text,'stderr':r.stderr.decode(),'properties':props}
phases=[];stopped=None
controller={'argv':sys.argv,'pid':os.getpid(),'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'phases':context['phases'],'outer_phase_guard':'frozen phase wall + 60 seconds; bounded stop/terminate/kill on controller refusal','caller_output_cap_bytes':2097152,'python_optimize':'0'}
dump(out/'CONTROLLER.json',controller)
try:
 for phase in context['phases']:
  inputs()
  phase_out=out/phase;phase_out.mkdir()
  argv=[sys.executable,str(prepared/'run.py'),phase]
  started=time.monotonic(); wall=600 if phase=='compile' else 30 if phase=='format-check' else 60
  env=dict(os.environ);env['PYTHONOPTIMIZE']='0'
  with (phase_out/'stdout').open('xb') as stdout,(phase_out/'stderr').open('xb') as stderr:
   p=subprocess.Popen(argv,cwd=root,env=env,stdout=stdout,stderr=stderr,start_new_session=True)
   unit='kvm-cancel-wake-'+phase+'-'+str(p.pid)+'.service'
   launch={'argv':argv,'pid':p.pid,'unit':unit,'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()}
   dump(phase_out/'LAUNCH.json',launch)
   print(json.dumps({'phase':phase,'status':'started','pid':p.pid,'unit':unit}),flush=True)
   group_paths=set(); samples=[]; refused=None; next_sample=0
   while p.poll() is None:
    elapsed=time.monotonic()-started
    if elapsed>=next_sample:
     snap=inspect(unit);samples.append(snap);next_sample=elapsed+2
     group=snap['properties'].get('ControlGroup')
     if group:group_paths.add('/sys/fs/cgroup'+group)
     dump(phase_out/'SERVICE-SAMPLES.json',samples)
    if elapsed>wall+60 or any((phase_out/name).stat().st_size>2097152 for name in ['stdout','stderr']):
     refused='controller deadline/output guard'
     stop=subprocess.run(['/usr/bin/systemctl','--user','stop',unit],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
     dump(phase_out/'STOP.json',{'raw_status':stop.returncode,'stdout':stop.stdout.decode(),'stderr':stop.stderr.decode()})
     try: p.wait(timeout=10)
     except subprocess.TimeoutExpired:
      os.killpg(p.pid,signal.SIGTERM)
      try:p.wait(timeout=10)
      except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait(timeout=10)
     break
    time.sleep(0.2)
   raw=p.wait()
  terminal=inspect(unit);props=terminal['properties']
  groups=[{'path':path,'exists':pathlib.Path(path).exists(),'procs':(pathlib.Path(path)/'cgroup.procs').read_text() if (pathlib.Path(path)/'cgroup.procs').exists() else None} for path in sorted(group_paths)]
  terminal['observed_cgroups']=groups
  terminal['settled']=props.get('ActiveState')=='inactive' and props.get('MainPID')=='0' and all(not x['exists'] or x['procs']=='' for x in groups)
  dump(phase_out/'TERMINAL.json',terminal)
  result={'phase':phase,'caller_raw_status':raw,'elapsed_seconds':time.monotonic()-started,'controller_refusal':refused,'terminal_settled':terminal['settled']}
  source_result=prepared/phase/'result.json'
  if source_result.exists():result['payload_result']=json.loads(source_result.read_text())
  phases.append(result);dump(out/'PHASES.json',phases)
  inputs();result['context_unchanged']=True;dump(out/'PHASES.json',phases)
  print(json.dumps(result),flush=True)
  if raw!=0 or refused or not terminal['settled']:
   stopped='failed phase '+phase;break
except Exception as error:
 stopped=repr(error)
 dump(out/'CONTROLLER-REFUSAL.json',{'error':stopped,'phases_completed':phases})
 print(json.dumps({'controller_refusal':stopped}),flush=True)
final={'complete':stopped is None and len(phases)==len(context['phases']),'stopped':stopped,'phases':phases,'ended_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()}
dump(out/'CONTROLLER-RESULT.json',final)
print(json.dumps({'complete':final['complete'],'stopped':stopped,'phases_collected':len(phases)}),flush=True)
sys.exit(0 if final['complete'] else 1)
