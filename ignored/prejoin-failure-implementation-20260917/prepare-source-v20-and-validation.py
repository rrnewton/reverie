from pathlib import Path
import ast,difflib,hashlib,json,runpy,shutil,subprocess
root=Path.cwd();area=root/'ignored/prejoin-failure-implementation-20260917';old=area/'source-v19-preparation';new=area/'source-v20-preparation';new.mkdir()
def digest(path):
 h=hashlib.sha256()
 with Path(path).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def bind(path):
 p=Path(path);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
def write(path,data):
 with path.open('x') as f:f.write(data)
binding=json.loads((old/'binding.json').read_text());parts=[];changed=[]
for row in binding['files']:
 p=root/row['path'];prev=old/'source'/row['path'];content=p.read_bytes()
 if digest(p)!=row['sha256']:
  changed.append(row['path']);parts.extend(difflib.unified_diff(prev.read_text().splitlines(True),p.read_text().splitlines(True),fromfile='a/'+row['path'],tofile='b/'+row['path']))
 row.update(bytes=len(content),sha256=digest(p),mode=oct(p.stat().st_mode&0o7777)[2:]);dest=new/'source'/row['path'];dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,dest)
assert changed==['reverie-kvm/src/executor.rs','reverie-kvm/src/failure.rs','reverie-kvm/src/runtime.rs','reverie-kvm/src/runtime/failure_tests.rs','reverie-kvm/src/runtime/native_test_support.rs','reverie-kvm/src/vm.rs']
write(new/'increment.patch',''.join(parts))
patch=subprocess.run(['git','diff','--binary',binding['base'],'--']+[r['path'] for r in binding['files']],check=True,stdout=subprocess.PIPE).stdout
with (new/'candidate.patch').open('xb') as f:f.write(patch)
manifest=json.loads((old/'tracked-source-manifest.json').read_text())
for row in manifest:
 if 'gitlink' in row:continue
 p=root/row['path'];h=hashlib.sha256(p.readlink().as_posix().encode()).hexdigest() if row['mode']=='120000' else digest(p)
 if h!=row['sha256']:assert row['path'] in changed;row['sha256']=h
write(new/'tracked-source-manifest.json',json.dumps(manifest,indent=2)+'\n')
selected=json.loads((old/'selected-tests.json').read_text());previous=selected['selected'];additions=['failure::tests::process_failure_subscriptions_preserve_fork_and_thread_ownership','runtime::failure_tests::independent_process_callbacks_preserve_completion_and_failure_scope']
assert not set(previous)&set(additions);selected.update(selected=sorted(previous+additions),retained=previous,added_native=additions,execution='Source v20 preparation only; all 40 prior native identities plus two new methods. Actual inventory and execution pending.')
write(new/'selected-tests.json',json.dumps(selected,indent=2)+'\n')
report='''# Reverie source v20\n\nThis correction addresses the measured original started-fork failure in qualification-execution-v3. That attempt passed four real VM methods, all ten exec-worker diagnostic modes, and all 17 leader_exit methods, then failed terminal_fork::backend_exec_failure_joins_live_fork. Its already-started independent child was interrupted before two required real write callbacks and natural exit. The original status, write, hook-count, waitability and exact diagnostic assertions remain unchanged. All first failures and the last three unexecuted methods are preserved.\n\nThe run retains one typed first cause and the initial traced owner still observes run-wide failure. FailureContext now also owns a process notification, shared by CLONE_THREAD and retained through runtime entry and exec. Actual fork preparation creates the fresh process context through the existing for_process operation. Independent process drivers observe their own process notification plus the explicit GlobalTool terminal notification. Selection reads LoadedStaticElf's existing is_traced_tree_root flag through a read-only executor accessor; it does not infer the role from numeric PID or guest-visible PPID.\n\nEvery ordinary KvmGuest::send_rpc separately observes run-wide and Tool-global failure before and after polling the real receive_rpc future. Failure sets the existing RuntimeError(RunAborted) handler signal and remains Pending until the production driver consumes it and drops the ordinary RPC future. No response is fabricated. Consuming KvmGlobal::send_rpc remains unchanged so deregistration and process cleanup can finish. The synchronous global terminal hook returns before process notification; later real failures still notify their own process without replacing the retained first cause. Derived RunAborted does not mark another process as failed. Existing cancellation status, gate commands, pending-child handling, tail injection and error aggregation remain unchanged.\n\nNativeToolOwner now uses the same actual process/root-selected driver and run-wide KvmGuest RPC path. The existing missing-status and cached-worker controls already wait for actual receive_rpc to poll Pending before releasing the owner, and now additionally require that RPC receiver to be dropped before the owned child join returns. Their old run-wide driver shortcut is removed for independent children. The existing held-publication control now also proves process notification cannot precede the synchronous global hook.\n\nTwo new native methods bring the prepared population to 42, retaining every old identity and assertion. One checks thread sharing, fresh fork notification, later process errors, derived cancellation and first-cause identity. The other uses the production native owner/driver/consuming hooks to check independent ready completion, own-process and Tool-global failure before and after callback polling, refusal before an ordinary RPC enters request state, and initial-root behavior with PID 71. These new methods do not themselves create a VM or simulate guest writes; original real fork/write/exit and all four VM plus 22 static methods remain required. The existing pending-RPC/owned-OS-join controls remain in the recurring native population.\n\nOnly six already-owned source paths change from v19; the complete 11-file base patch, source copies and full manifest are retained. No Hermit source, dependency lock, manifest, public pin, syscall errno/status assertion, signal policy or selected-test bound changed. Source formatting is preparation, not execution. This is not Hermit strict INFO, repeated-run determinism or canonical parity evidence.\n'''
write(new/'REPORT.md',report)
binding.update(patch_sha256=digest(new/'candidate.patch'),patch_bytes=len(patch),increment_sha256=digest(new/'increment.patch'),report_sha256=digest(new/'REPORT.md'),source_manifest_sha256=digest(new/'tracked-source-manifest.json'),selected_tests_sha256=digest(new/'selected-tests.json'),previous_binding_sha256=digest(old/'binding.json'),retained_no_vm_selected_count=40,selected_no_vm_count=42,source_preparation='Process notification and actual ordinary RPC interruption after retained v19 started-fork failure; v20 unexecuted.')
write(new/'binding.json',json.dumps(binding,indent=2)+'\n')
print(json.dumps({name:bind(new/name) for name in ['binding.json','candidate.patch','increment.patch','REPORT.md']},indent=2))
for old_name,new_name in [('cargo-v13','cargo-v14'),('lint-v10','lint-v11')]:
 prior=area/old_name;out=area/new_name;out.mkdir();plan=json.loads((prior/'plan.json').read_text())
 for row in plan['inputs']:
  path=row['path'].replace('source-v19-preparation','source-v20-preparation');current=bind(path)
  if path==row['path']:assert current==row,path
  row.clear();row.update(current)
 for key in ['source_binding','source_manifest']:plan[key]=plan[key].replace('source-v19-preparation','source-v20-preparation')
 for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace(old_name,new_name)
 plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace(old_name,new_name);plan['execution']=['/usr/bin/python3','-B',str(out/'launch.py')]
 if 'scope' in plan:plan['scope']=plan['scope'].replace('v19','v20')
 if 'status' in plan:plan['status']='Prepared source v20 process notification and RPC correction; no execution yet.'
 if old_name.startswith('cargo'):
  assert plan['selected_tests']==previous;plan['selected_tests']=selected['selected'];plan['required_count']=42
  for name in ['qualification-build-v4/run-1/retained-elf-v19/lib','qualification-build-v4/run-1/retained-elf-v19/static-elf','qualification-build-v4/run-1/retained-elf-v19/binding.json']:plan['inputs'].append(bind(area/name))
 for stage in plan['stages']:
  stage['out']=stage['out'].replace(old_name,new_name);stage['argv']=[v.replace(old_name,new_name) for v in stage['argv']]
  if stage['name']=='native':
   stage['payload']=['<verified-compiled-test-executable>','--exact']+plan['selected_tests']+['--test-threads=1','--nocapture'];stage['argv']=stage['argv'][:11]+stage['payload']
  assert stage['argv'][11:]==stage['payload']
 for key in ['run_root','observer_root']:assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
 write(out/'plan.json',json.dumps(plan,indent=2)+'\n');script=(prior/'launch.py').read_text().replace(digest(prior/'plan.json'),digest(out/'plan.json'))
 if old_name.startswith('cargo'):
  script=script.replace('== 40','== 42').replace("'count': 40","'count': 42").replace("('40', '0', '0', '0')","('42', '0', '0', '0')").replace('exactly 40 passing','exactly 42 passing').replace("'selected_count': 40","'selected_count': 42")
 ast.parse(script);write(out/'launch.py',script)
 helper=runpy.run_path(str(out/'launch.py') if old_name.startswith('cargo') else plan['helpers']['path'],run_name='preflight_only');helper['check_inputs'](plan)
 record=dict(caller=bind(out/'launch.py'),plan=bind(out/'plan.json'),execution=plan['execution'],outputs={s['name']:s['out'] for s in plan['stages']},scope='Preparation only; no execution.')
 write(out/'reservation.json',json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
