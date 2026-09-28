from pathlib import Path
import ast,hashlib,json
root=Path.cwd();a=root/'ignored/prejoin-failure-implementation-20260917';old=a/'cargo-v2';new=a/'cargo-v4';new.mkdir()
def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def bind(path):
 path=Path(path);s=path.stat();return dict(path=str(path),resolved_path=str(path.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(path))
p=json.loads((old/'plan.json').read_text())
lock=root/'Cargo.lock';assert digest(lock)=='1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856'
for row in p['inputs']:
 current=bind(row['path'])
 if row['path']!=str(lock):assert current==row, 'source or input changed: '+row['path']
 row.clear();row.update(current)
p['locked_dependency_file']=bind(lock)
p['status']='root accepted exact existing-manifest lock refresh and authorized unchanged source-v2 27-test bounded compile/list/native sequence'
for key in ['run_root','observer_root','target_dir','tmpdir']:
 p[key]=p[key].replace('cargo-v2','cargo-v4').replace('prejoin-native-v2','prejoin-native-v4')
for key in ['CARGO_TARGET_DIR','TMPDIR']:
 p['environment_fixed'][key]=p['environment_fixed'][key].replace('cargo-v2','cargo-v4').replace('prejoin-native-v2','prejoin-native-v4')
p['execution']=['/usr/bin/python3','-B',str(new/'launch.py')]
for stage in p['stages']:
 stage['out']=stage['out'].replace('cargo-v2','cargo-v4')
 stage['argv']=[s.replace('cargo-v2','cargo-v4') for s in stage['argv']]
p['preparation_changes'] += ['Root accepted the sole lockfile source-line refresh to the unchanged declared liteinst2 revision. Source-v2 and all 27 selectors, --locked --offline, observer and resource bounds remain unchanged. The original refusal and lock refresh are retained under cargo-v2 and cargo-v3.']
p['accepted_lock_change']=str(a/'cargo-v3/LOCK-CHANGE.json')
p['inputs'] += [bind(a/'cargo-v3/LOCK-CHANGE.json'),bind(a/'cargo-v3/Cargo.lock.patch')]
for path in [p['run_root'],p['observer_root'],p['target_dir']]:assert not Path(path).exists() and not Path(path).is_symlink()
with (new/'plan.json').open('x') as f:json.dump(p,f,indent=2);f.write('\n')
script=(old/'launch.py').read_text().replace('b657a9ed68171522c25e02b727523d7aa03fbf57ddc361104adc58d52b4d2b2e',digest(new/'plan.json'))
ast.parse(script)
with (new/'launch.py').open('x') as f:f.write(script)
record=dict(source_binding=bind(Path(p['source_binding'])),lock=bind(lock),plan=bind(new/'plan.json'),caller=bind(new/'launch.py'),outputs={s['name']:s['out'] for s in p['stages']})
with (new/'reservation.json').open('x') as f:json.dump(record,f,indent=2);f.write('\n')
print(json.dumps(record,indent=2))
