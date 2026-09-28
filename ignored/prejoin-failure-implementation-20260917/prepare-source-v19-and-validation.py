from pathlib import Path
import ast,difflib,hashlib,json,runpy,shutil,subprocess
root=Path.cwd(); area=root/'ignored/prejoin-failure-implementation-20260917'
old=area/'source-v18-preparation'; new=area/'source-v19-preparation'; new.mkdir()
def digest(path):
 h=hashlib.sha256()
 with Path(path).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''): h.update(b)
 return h.hexdigest()
def bind(path):
 path=Path(path); s=path.stat()
 return dict(path=str(path),resolved_path=str(path.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(path))
def write(path,data):
 with path.open('x') as f: f.write(data)
binding=json.loads((old/'binding.json').read_text()); increments=[]
for row in binding['files']:
 path=root/row['path']; previous=old/'source'/row['path']; current=path.read_bytes()
 if digest(path)!=row['sha256']:
  assert row['path']=='reverie-kvm/src/runtime/failure_tests.rs'
  assert previous.read_bytes().replace(b'Errno::ENOTSUP.into()',b'Errno::ENOTSUPP.into()')==current
  increments.extend(difflib.unified_diff(previous.read_text().splitlines(True),path.read_text().splitlines(True),fromfile='a/'+row['path'],tofile='b/'+row['path']))
 row.update(bytes=len(current),sha256=digest(path),mode=oct(path.stat().st_mode&0o7777)[2:])
 dest=new/'source'/row['path']; dest.parent.mkdir(parents=True,exist_ok=True); shutil.copy2(path,dest)
assert increments
write(new/'increment.patch',''.join(increments))
patch=subprocess.run(['git','diff','--binary',binding['base'],'--']+[r['path'] for r in binding['files']],check=True,stdout=subprocess.PIPE).stdout
with (new/'candidate.patch').open('xb') as f: f.write(patch)
manifest=json.loads((old/'tracked-source-manifest.json').read_text())
for row in manifest:
 if 'gitlink' in row: continue
 p=root/row['path']; h=hashlib.sha256(p.readlink().as_posix().encode()).hexdigest() if row['mode']=='120000' else digest(p)
 if h!=row['sha256']: assert row['path']=='reverie-kvm/src/runtime/failure_tests.rs'; row['sha256']=h
write(new/'tracked-source-manifest.json',json.dumps(manifest,indent=2)+'\n')
selected=json.loads((old/'selected-tests.json').read_text()); selected['execution']='Source v19 preparation only; same 40 identities. Source v18 failed compilation before inventory/native; its result is preserved.'
write(new/'selected-tests.json',json.dumps(selected,indent=2)+'\n')
report='''# Reverie source v19\n\nThis successor changes only the new native control's unavailable `Errno::ENOTSUP` name to this project's `Errno::ENOTSUPP`. The control still represents a worker's own runtime error, with the same expected fatal status and typed primary behavior. No product behavior, test case, assertion, selector, dependency or resource bound changes.\n\nSource v18's actual first compile failed with E0599 before inventory or native execution. The complete structured diagnostic, raw output, source/input checks and inactive/empty service accounting are preserved in cargo-v12/REPORT.md and RESULT.json. Its prepared lint-v9 remains unexecuted.\n\nThe complete candidate remains the reviewed started-sibling status correction described in the adjacent source-v17 packet, including its full v16-to-v17 increment; source-v18 contains the subsequent missing-None caller adaptation. This packet contains the complete base patch and only v18-to-v19 in increment.patch. The 40 prepared native identities retain all 39 prior methods. Actual current inventory/execution remain pending, and the original four real VM plus 22 static methods and all assertions remain required. No native result is claimed as guest parity.\n'''
write(new/'REPORT.md',report)
binding.update(patch_sha256=digest(new/'candidate.patch'),patch_bytes=len(patch),increment_sha256=digest(new/'increment.patch'),report_sha256=digest(new/'REPORT.md'),source_manifest_sha256=digest(new/'tracked-source-manifest.json'),selected_tests_sha256=digest(new/'selected-tests.json'),previous_binding_sha256=digest(old/'binding.json'),source_preparation='Identifier-only native-control correction after retained v18 compilation failure; v19 unexecuted.')
write(new/'binding.json',json.dumps(binding,indent=2)+'\n')
print(json.dumps({name:bind(new/name) for name in ['binding.json','candidate.patch','increment.patch','REPORT.md']},indent=2))
for old_name,new_name in [('cargo-v12','cargo-v13'),('lint-v9','lint-v10')]:
 old_plan_dir=area/old_name; new_plan_dir=area/new_name; new_plan_dir.mkdir()
 plan=json.loads((old_plan_dir/'plan.json').read_text())
 for row in plan['inputs']:
  path=row['path'].replace('source-v18-preparation','source-v19-preparation'); current=bind(path)
  if path==row['path']: assert current==row,path
  row.clear(); row.update(current)
 for key in ['source_binding','source_manifest']: plan[key]=plan[key].replace('source-v18-preparation','source-v19-preparation')
 for key in ['run_root','observer_root','tmpdir']: plan[key]=plan[key].replace(old_name,new_name)
 plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace(old_name,new_name)
 plan['execution']=['/usr/bin/python3','-B',str(new_plan_dir/'launch.py')]
 if 'scope' in plan: plan['scope']=plan['scope'].replace('v18','v19')
 for stage in plan['stages']:
  stage['out']=stage['out'].replace(old_name,new_name)
  stage['argv']=[x.replace(old_name,new_name) for x in stage['argv']]
  assert stage['argv'][11:]==stage['payload']
 for key in ['run_root','observer_root']: assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
 write(new_plan_dir/'plan.json',json.dumps(plan,indent=2)+'\n')
 script=(old_plan_dir/'launch.py').read_text().replace(digest(old_plan_dir/'plan.json'),digest(new_plan_dir/'plan.json')); ast.parse(script)
 write(new_plan_dir/'launch.py',script)
 helper=runpy.run_path(str(new_plan_dir/'launch.py') if old_name.startswith('cargo') else plan['helpers']['path'],run_name='preflight_only')
 helper['check_inputs'](plan)
 record=dict(caller=bind(new_plan_dir/'launch.py'),plan=bind(new_plan_dir/'plan.json'),execution=plan['execution'],outputs={s['name']:s['out'] for s in plan['stages']},scope='Preparation only; no execution.')
 write(new_plan_dir/'reservation.json',json.dumps(record,indent=2)+'\n'); print(json.dumps(record,indent=2))
