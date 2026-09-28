from pathlib import Path
import hashlib,json,os,runpy,shutil,subprocess
A=Path(__file__).resolve().parent.parent
root=A.parent.parent
sha=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest()
def write(p,obj):
 with Path(p).open('x') as f: json.dump(obj,f,indent=2);f.write('\n')
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()=='12d4ce8c0bc426f1ae41416f5b4a699e2c300879'
assert subprocess.run(['git','diff','--quiet','HEAD','--'],cwd=root).returncode==0
for n,caller in [('qualification-build-v7','build.py'),('lint-v15','launch.py')]:
 d=A/n;p=json.loads((d/'plan.json').read_bytes());run=Path(p['run_root']);summary=json.loads((run/'summary.json').read_bytes());assert summary['status']=='passed'
 h=runpy.run_path(p['helpers']['path'],run_name='read_only_helpers');h['check_inputs'](p)
 stages=[];terminal=[]
 for s in p['stages']:
  o=Path(s['out']);r=json.loads((o/'result.json').read_bytes());assert r['wrapper_exit_code']==0 and r['observer_error'] is None and r['stop_reason'] is None and r['accounting_complete'] and r['final_accounting']['cgroup_empty']
  reads=[]
  for _ in range(2):
   cmd=['/usr/bin/systemctl','--user','show',r['authenticated_service']['unit'],'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup']
   q=subprocess.run(cmd,capture_output=True,text=True,timeout=10);v=dict(x.split('=',1) for x in q.stdout.splitlines());assert q.returncode==0 and v['ActiveState']=='inactive' and v['MainPID']=='0' and v['ControlGroup']==''
   reads.append(dict(argv=cmd,exit=q.returncode,stdout=q.stdout,stderr=q.stderr))
  terminal.append(dict(stage=s['name'],readbacks=reads))
  for f in ['stdout','stderr']: assert (o/f).stat().st_size<=s['reader_limit_bytes']
  stages.append(dict(stage=s['name'],payload=s['payload'],cpu_seconds=r['final_accounting']['cpu_usage_nsec']/1e9,wall_seconds=r['elapsed_seconds'],service=r['authenticated_service'],result_path=str(o/'result.json'),result_sha256=sha(o/'result.json'),stdout_sha256=sha(o/'stdout'),stderr_sha256=sha(o/'stderr'),stdout_bytes=(o/'stdout').stat().st_size,stderr_bytes=(o/'stderr').stat().st_size))
 write(d/'TERMINAL.json',terminal)
 result=dict(status='passed',head=p['source_head'],source_binding_sha256=sha(p['source_binding']),plan_sha256=sha(d/'plan.json'),caller_sha256=sha(d/caller),launch_sha256=sha(run/'launch.json'),stages=stages,terminal_sha256=sha(d/'TERMINAL.json'),full_source_and_inputs_verified=True,input_count=len(p['inputs']))
 if n.startswith('qualification'):
  artifacts=json.loads((run/'compiled-executables.json').read_bytes());dest=run/'retained-elf-v25';dest.mkdir(mode=0o700);retained={}
  for k,e in artifacts.items():
   h['check_executable'](e);src=Path(e['path']);to=dest/k
   with src.open('rb') as inp,to.open('xb') as out: shutil.copyfileobj(inp,out)
   os.chmod(to,e['mode']);assert sha(to)==e['sha256'];assert to.stat().st_ino!=src.stat().st_ino
   retained[k]=dict(path=str(to),bytes=to.stat().st_size,mode=to.stat().st_mode&0o7777,sha256=sha(to),separate_inode=True,emitted_path=str(src))
  write(dest/'binding.json',dict(head=p['source_head'],compiled_executables_sha256=sha(run/'compiled-executables.json'),artifacts=retained))
  inv={k:json.loads((run/('list-'+k+'-inventory.json')).read_bytes())['count'] for k in artifacts};assert inv=={'lib':454,'static-elf':288}
  raw=(Path(p['stages'][0]['out'])/'stdout').read_bytes();diagnostics=[json.loads(x) for x in raw.splitlines() if json.loads(x).get('reason')=='compiler-message'];assert not diagnostics
  result.update(inventories=inv,compiler_diagnostics=diagnostics,retained=retained,retained_elf_binding_sha256=sha(dest/'binding.json'),scope='Compile and complete actual inventories only. No test or VM execution.')
 else: result['scope']='Workspace formatting and all-target/all-feature Clippy with -D warnings. No test or VM execution.'
 h['check_inputs'](p);write(d/'RESULT.json',result)
 lines=['Final committed 12d4ce8c checks passed.','',result['scope'],'']
 for s in stages: lines.append(f"- {s['stage']}: {s['cpu_seconds']:.6f} CPU seconds, {s['wall_seconds']:.9f} observed wall seconds; payload exit 0, complete accounting and two fresh inactive/empty readbacks.")
 if 'inventories' in result: lines+=['','Actual inventories: 454 library methods (all 453 retained plus the one new eight-case control), 288 static integration methods unchanged. No structured compiler-message diagnostics. Both emitted ELFs have separate byte-verified retained copies.']
 lines+=['',f"All {len(p['inputs'])} explicit inputs and the full 2,596-entry source manifest still match. Output is bounded and untruncated. Earlier failures remain separate."]
 with (d/'REPORT.md').open('x') as f:f.write('\n'.join(lines)+'\n')
 print(n,json.dumps({'RESULT':sha(d/'RESULT.json'),'REPORT':sha(d/'REPORT.md'),'TERMINAL':sha(d/'TERMINAL.json'),'stages':[{k:s[k] for k in ['stage','cpu_seconds','wall_seconds']} for s in stages], 'retained':result.get('retained',{})}))
