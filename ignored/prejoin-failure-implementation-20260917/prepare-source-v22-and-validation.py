from pathlib import Path
import ast,difflib,hashlib,json,runpy,shutil,subprocess
root=Path.cwd();area=root/'ignored/prejoin-failure-implementation-20260917';old=area/'source-v21-preparation';new=area/'source-v22-preparation'
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
selected=json.loads((old/'selected-tests.json').read_text());previous=selected['selected'];additions=['runtime::failure_tests::independent_process_select_then_await_keeps_failed_rpc_pending']
assert not set(previous)&set(additions);selected.update(selected=sorted(previous+additions),retained=previous,added_native=additions,execution='Source v22 preparation only; all 43 prior native identities plus one actual Guest RPC select-then-await method. Actual inventory and execution pending.')
write(new/'selected-tests.json',json.dumps(selected,indent=2)+'\n')
report="# Reverie source v22\n\nSource v21's retained diagnostic passed all 43 selected native controls, including the actual failed-run select-then-return case and adjacent normal responses; its actual inventory was 452 with zero structured compiler diagnostics. The actual ELF is separately retained. Independent review then found a distinct repeated-poll defect: after the interrupted RPC returned Pending, a callback could await select's losing RPC before returning to the driver. That would poll the completed async failure wait again and could panic. This was a source finding, not a failure measured by the earlier population. V21's passing evidence and unexecuted lint-v12 preparation remain unchanged.\n\nKvmGuest::send_rpc now completes its internal polling block with an optional response. Failure before or after response polling ends that block with None, dropping both the ordinary request and failure futures. It then records the existing RuntimeError(RunAborted) signal and awaits a permanently pending future. Further polls cannot resume either completed wait or enter receive_rpc. The normal Some(response) path returns the actual response unchanged. The v21 driver still consumes the explicit signal before accepting Ready or Pending; Tool-global/root/process terminal priority, thread cancellation and tail injection stay distinct. Consuming KvmGlobal RPC is unchanged.\n\nThe new native method uses an actual independent NativeToolOwner and KvmGuest. Its callback selects a ready alternative, keeps the losing Guest RPC, and awaits it again before returning to the driver. In the failed-run case the second poll remains pending, the driver consumes RuntimeError, request state is never admitted, and both consuming owners preserve the exact typed cause, worker identity and fatal statuses. An adjacent normal case observes the actual RPC Pending state, supplies response 37 after the alternative wins, and successfully awaits that same request; it retains normal hook order/status and publishes no failure. No VM or host worker is created by this new method. All previous actual Pending-RPC/owned-OS-join controls, before-request checks and the v21 ready-alternative control remain unchanged.\n\nOnly runtime.rs and runtime/failure_tests.rs change from v21. All 43 prior native identities and assertions remain; one method brings the prepared native population to 44. Complete source and the 11-file base patch remain bound. No manifest, lock, Hermit source, public pin, original four-VM/22-static selector or resource limit changes. The next validation retains the exact selected identities and all previous attempts.\n\nThe original real independent-fork contract still requires actual writes, waitability and natural exit; all 26 original VM/static methods remain pending on this successor. V19's measured first failure and final three unexecuted methods are preserved. Source preparation and native evidence do not establish Hermit strict INFO, repeated-run determinism or canonical parity.\n"
write(new/'REPORT.md',report)
binding.update(patch_sha256=digest(new/'candidate.patch'),patch_bytes=len(patch),increment_sha256=digest(new/'increment.patch'),report_sha256=digest(new/'REPORT.md'),source_manifest_sha256=digest(new/'tracked-source-manifest.json'),selected_tests_sha256=digest(new/'selected-tests.json'),previous_binding_sha256=digest(old/'binding.json'),retained_no_vm_selected_count=43,selected_no_vm_count=44,source_preparation='Keep interrupted Guest RPC permanently pending after dropping its request and failure waits; source v22 unexecuted.')
write(new/'binding.json',json.dumps(binding,indent=2)+'\n')
print(json.dumps({name:bind(new/name) for name in ['binding.json','candidate.patch','increment.patch','REPORT.md']},indent=2))
for old_name,new_name in [('cargo-v15','cargo-v16'),('lint-v12','lint-v13')]:
 prior=area/old_name;out=area/new_name;out.mkdir();plan=json.loads((prior/'plan.json').read_text())
 for row in plan['inputs']:
  path=row['path'].replace('source-v21-preparation','source-v22-preparation');current=bind(path)
  if path==row['path']:assert current==row,path
  row.clear();row.update(current)
 for key in ['source_binding','source_manifest']:plan[key]=plan[key].replace('source-v21-preparation','source-v22-preparation')
 for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace(old_name,new_name)
 plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace(old_name,new_name);plan['execution']=['/usr/bin/python3','-B',str(out/'launch.py')]
 if 'scope' in plan:plan['scope']=plan['scope'].replace('v20','v21')
 if 'status' in plan:plan['status']='Prepared source v22 stable RPC terminal-state correction; no execution yet.'
 if old_name.startswith('cargo'):
  assert plan['selected_tests']==previous;plan['selected_tests']=selected['selected'];plan['required_count']=44
  plan.pop('new_test_count',None)
  plan['preparation_changes']=['All 43 prior native identities remain; one actual independent Guest RPC select-then-await method is added.','Same owned cache and resource limits, fresh output paths, every prior executed source and first failure retained.','Removed the unused ambiguous new_test_count field; exact selected_tests and required_count bind 44. No execution behavior depends on that removed field.','No manifest, lock, Hermit, VM/static selector or existing assertion change; no VM or guest execution in this sequence.']
  for name in ['cargo-v15/RESULT.json','cargo-v15/REPORT.md','cargo-v15/run-1/retained-elf-v21/lib','cargo-v15/run-1/retained-elf-v21/binding.json']:plan['inputs'].append(bind(area/name))
 for stage in plan['stages']:
  stage['out']=stage['out'].replace(old_name,new_name);stage['argv']=[v.replace(old_name,new_name) for v in stage['argv']]
  if stage['name']=='native':
   stage['payload']=['<verified-compiled-test-executable>','--exact']+plan['selected_tests']+['--test-threads=1','--nocapture'];stage['argv']=stage['argv'][:11]+stage['payload']
  assert stage['argv'][11:]==stage['payload']
 for key in ['run_root','observer_root']:assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
 write(out/'plan.json',json.dumps(plan,indent=2)+'\n');script=(prior/'launch.py').read_text().replace(digest(prior/'plan.json'),digest(out/'plan.json'))
 if old_name.startswith('cargo'):
  script=script.replace('== 43','== 44').replace("'count': 43","'count': 44").replace("('43', '0', '0', '0')","('44', '0', '0', '0')").replace('exactly 43 passing','exactly 44 passing').replace("'selected_count': 43","'selected_count': 44")
 ast.parse(script);write(out/'launch.py',script)
 helper=runpy.run_path(str(out/'launch.py') if old_name.startswith('cargo') else plan['helpers']['path'],run_name='preflight_only');helper['check_inputs'](plan)
 record=dict(caller=bind(out/'launch.py'),plan=bind(out/'plan.json'),execution=plan['execution'],outputs={s['name']:s['out'] for s in plan['stages']},scope='Preparation only; no execution.')
 write(out/'reservation.json',json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
