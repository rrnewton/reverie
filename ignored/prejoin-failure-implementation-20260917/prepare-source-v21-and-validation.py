from pathlib import Path
import ast,difflib,hashlib,json,runpy,shutil,subprocess
root=Path.cwd();area=root/'ignored/prejoin-failure-implementation-20260917';old=area/'source-v20-preparation';new=area/'source-v21-preparation'
def digest(path):
 h=hashlib.sha256()
 with Path(path).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def bind(path):
 p=Path(path);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
def write(path,data):
 with path.open('x') as f:f.write(data)
new.mkdir();binding=json.loads((old/'binding.json').read_text());parts=[];changed=[]
for row in binding['files']:
 p=root/row['path'];prev=old/'source'/row['path'];content=p.read_bytes()
 if digest(p)!=row['sha256']:
  changed.append(row['path']);parts.extend(difflib.unified_diff(prev.read_text().splitlines(True),p.read_text().splitlines(True),fromfile='a/'+row['path'],tofile='b/'+row['path']))
 row.update(bytes=len(content),sha256=digest(p),mode=oct(p.stat().st_mode&0o7777)[2:]);dest=new/'source'/row['path'];dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,dest)
assert changed==['reverie-kvm/src/runtime.rs','reverie-kvm/src/runtime/failure_tests.rs']
write(new/'increment.patch',''.join(parts))
patch=subprocess.run(['git','diff','--binary',binding['base'],'--']+[r['path'] for r in binding['files']],check=True,stdout=subprocess.PIPE).stdout
with (new/'candidate.patch').open('xb') as f:f.write(patch)
manifest=json.loads((old/'tracked-source-manifest.json').read_text())
for row in manifest:
 if 'gitlink' in row:continue
 p=root/row['path'];h=hashlib.sha256(p.readlink().as_posix().encode()).hexdigest() if row['mode']=='120000' else digest(p)
 if h!=row['sha256']:assert row['path'] in changed;row['sha256']=h
write(new/'tracked-source-manifest.json',json.dumps(manifest,indent=2)+'\n')
selected=json.loads((old/'selected-tests.json').read_text());previous=selected['selected'];additions=['runtime::failure_tests::independent_process_select_cannot_discard_rpc_runtime_failure']
assert not set(previous)&set(additions);selected.update(selected=sorted(previous+additions),retained=previous,added_native=additions,execution='Source v21 preparation only; all 42 prior native identities plus one actual Guest RPC select method. Actual inventory and execution pending.')
write(new/'selected-tests.json',json.dumps(selected,indent=2)+'\n')
report='''# Reverie source v21

The retained source v20 diagnostic passed all 42 selected native controls with an actual 451-method inventory and zero structured compiler diagnostics. Its actual ELF is separately retained. Root and independent source review nevertheless found a concrete untested escape: an independent callback can poll Guest::send_rpc in a select, record RuntimeError(RunAborted), then return through a ready alternative. The previous driver consumed recorded handler signals only when the entire callback remained Pending. This was a source finding; no claim is made that the old native population measured it. The v20 source, report, passing evidence and unexecuted lint-v11 preparation remain unchanged.

The driver now consumes the explicit handler signal immediately after polling the callback and checking the terminal future again, before accepting either Ready or Pending. RuntimeError, ThreadCancelled and TailInjected retain their distinct outcomes, with existing Tool-global/root/process failure priority preserved. An ordinary suspension still starts pending registered children only when no terminal or handler signal exists. No callback-readiness policy is substituted for an explicit signal, no response is fabricated and no request or child gate is reopened.

The new native method uses an actual independent NativeToolOwner, KvmGuest and select over Guest::send_rpc plus a ready alternative. Its failed-run case proves the callback actually chose the ready alternative while the driver still returns RuntimeError(RunAborted), leaves request state unadmitted, retains the original typed cause/worker identity and consumes both child/root hooks with their existing fatal statuses. Adjacent normal cases prove a voluntarily dropped pending RPC can select 73 and a ready RPC response still returns 37, without any failure publication; losing RPC ownership is dropped and exact consuming hook statuses/order are retained. The new method does not execute VM instructions or create a host worker. Existing actual Pending-RPC/owned-OS-join controls and their added receiver-drop assertions remain selected unchanged.

Only runtime.rs and runtime/failure_tests.rs change from v20. All 42 prior native identities and every existing assertion remain; one method brings the prepared native population to 43. The complete 11-file base patch and all 2,586 source entries are bound. The underlying v20 process notification, fork/thread/exec ownership, ordinary Guest RPC interruption and unchanged KvmGlobal consuming path remain intact. No lock, manifest, Hermit source, public pin, original four-VM/22-static selector or resource bound changes.

The original real fork contract still requires actual writes, waitability and natural exit, and the full unchanged 26-stage qualification remains pending on this successor. V19's measured failure and final three unexecuted static methods stay preserved. This is source preparation, not execution, Hermit strict INFO, determinism or canonical parity evidence.
'''
write(new/'REPORT.md',report)
binding.update(patch_sha256=digest(new/'candidate.patch'),patch_bytes=len(patch),increment_sha256=digest(new/'increment.patch'),report_sha256=digest(new/'REPORT.md'),source_manifest_sha256=digest(new/'tracked-source-manifest.json'),selected_tests_sha256=digest(new/'selected-tests.json'),previous_binding_sha256=digest(old/'binding.json'),retained_no_vm_selected_count=42,selected_no_vm_count=43,source_preparation='Consume explicit nonlocal handler signals before accepting callback results; source v21 unexecuted.')
write(new/'binding.json',json.dumps(binding,indent=2)+'\n')
print(json.dumps({name:bind(new/name) for name in ['binding.json','candidate.patch','increment.patch','REPORT.md']},indent=2))
for old_name,new_name in [('cargo-v14','cargo-v15'),('lint-v11','lint-v12')]:
 prior=area/old_name;out=area/new_name;out.mkdir();plan=json.loads((prior/'plan.json').read_text())
 for row in plan['inputs']:
  path=row['path'].replace('source-v20-preparation','source-v21-preparation');current=bind(path)
  if path==row['path']:assert current==row,path
  row.clear();row.update(current)
 for key in ['source_binding','source_manifest']:plan[key]=plan[key].replace('source-v20-preparation','source-v21-preparation')
 for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace(old_name,new_name)
 plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace(old_name,new_name);plan['execution']=['/usr/bin/python3','-B',str(out/'launch.py')]
 if 'scope' in plan:plan['scope']=plan['scope'].replace('v20','v21')
 if 'status' in plan:plan['status']='Prepared source v21 explicit handler-signal correction; no execution yet.'
 if old_name.startswith('cargo'):
  assert plan['selected_tests']==previous;plan['selected_tests']=selected['selected'];plan['required_count']=43
  plan.pop('new_test_count',None)
  plan['preparation_changes']=['All 42 prior native identities remain; one actual independent Guest RPC select method is added.','Same owned cache and resource limits, fresh output paths, every prior executed source and first failure retained.','Removed the unused ambiguous new_test_count field; exact selected_tests and required_count bind 43. No execution behavior depends on that removed field.','No manifest, lock, Hermit, VM/static selector or existing assertion change; no VM or guest execution in this sequence.']
  for name in ['cargo-v14/RESULT.json','cargo-v14/REPORT.md','cargo-v14/run-1/retained-elf-v20/lib','cargo-v14/run-1/retained-elf-v20/binding.json']:plan['inputs'].append(bind(area/name))
 for stage in plan['stages']:
  stage['out']=stage['out'].replace(old_name,new_name);stage['argv']=[v.replace(old_name,new_name) for v in stage['argv']]
  if stage['name']=='native':
   stage['payload']=['<verified-compiled-test-executable>','--exact']+plan['selected_tests']+['--test-threads=1','--nocapture'];stage['argv']=stage['argv'][:11]+stage['payload']
  assert stage['argv'][11:]==stage['payload']
 for key in ['run_root','observer_root']:assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
 write(out/'plan.json',json.dumps(plan,indent=2)+'\n');script=(prior/'launch.py').read_text().replace(digest(prior/'plan.json'),digest(out/'plan.json'))
 if old_name.startswith('cargo'):
  script=script.replace('== 42','== 43').replace("'count': 42","'count': 43").replace("('42', '0', '0', '0')","('43', '0', '0', '0')").replace('exactly 42 passing','exactly 43 passing').replace("'selected_count': 42","'selected_count': 43")
 ast.parse(script);write(out/'launch.py',script)
 helper=runpy.run_path(str(out/'launch.py') if old_name.startswith('cargo') else plan['helpers']['path'],run_name='preflight_only');helper['check_inputs'](plan)
 record=dict(caller=bind(out/'launch.py'),plan=bind(out/'plan.json'),execution=plan['execution'],outputs={s['name']:s['out'] for s in plan['stages']},scope='Preparation only; no execution.')
 write(out/'reservation.json',json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
