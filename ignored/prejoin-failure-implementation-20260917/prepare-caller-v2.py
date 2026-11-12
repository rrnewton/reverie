from pathlib import Path
import ast,hashlib,json,difflib
r=Path.cwd();a=r/'ignored/prejoin-failure-implementation-20260917';old=a/'cargo-v1';new=a/'cargo-v2';new.mkdir()
p=json.loads((old/'plan.json').read_text())
def h(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def bind(path):
 path=Path(path);s=path.stat()
 return dict(path=str(path),resolved_path=str(path.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=h(path))
p['status']='root authorized source-v2 corrections and unchanged bounded compile/list/27-native sequence; not source or landing approval'
for key in ['source_binding','source_manifest','run_root','observer_root','target_dir','tmpdir']:
 p[key]=p[key].replace('source-v1/','source-v2/').replace('cargo-v1','cargo-v2').replace('prejoin-native-v1','prejoin-native-v2')
p['execution']=['/usr/bin/python3','-B',str(new/'launch.py')]
for key in ['CARGO_TARGET_DIR','TMPDIR']:
 p['environment_fixed'][key]=p['environment_fixed'][key].replace('cargo-v1','cargo-v2').replace('prejoin-native-v1','prejoin-native-v2')
for row in p['inputs']:
 previous=dict(row);path=row['path'].replace('/source-v1/','/source-v2/')
 current=bind(path)
 if path==previous['path']:assert current==previous, 'unchanged input changed: '+path
 row.clear();row.update(current)
for stage in p['stages']:
 stage['out']=stage['out'].replace('cargo-v1','cargo-v2')
 stage['argv']=[s.replace('cargo-v1','cargo-v2') for s in stage['argv']]
p['preparation_changes'][0]=p['preparation_changes'][0].replace('source-v1','source-v2')
p['preparation_changes'].extend([
 'Source-v2 retains source-v1 unchanged and restores worker TID display through a typed WorkerFailure wrapper.',
 'RPC test rescue/reaping is now owned before its first precondition and remains installed during unwind. All 27 selectors and existing bounds are unchanged.',
 'The execution field now identifies this candidate cargo-v2/launch.py; the caller verifies its own path against the plan before dispatch.'
])
for path in [p['run_root'],p['observer_root'],p['target_dir']]:assert not Path(path).exists() and not Path(path).is_symlink()
with (new/'plan.json').open('x') as f:json.dump(p,f,indent=2);f.write('\n')
launch=(old/'launch.py').read_text().replace('d94d92c619a6239964a44c0f1a1355886056a4d375ce4a25adf5f566b33e27d1',h(new/'plan.json'))
marker="    require(Path(plan['run_root']) == HERE / 'run-1', 'unexpected run output destination')"
replacement="    require(plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')], 'execution names another caller')\n"+marker
assert launch.count(marker)==1;launch=launch.replace(marker,replacement)
marker="              'environment': environment, 'source_binding_sha256': digest(plan['source_binding']),"
replacement="              'execution': plan['execution'], 'cwd': str(Path.cwd()),\n"+marker
assert launch.count(marker)==1;launch=launch.replace(marker,replacement)
ast.parse(launch)
with (new/'launch.py').open('x') as f:f.write(launch)
with (new/'caller-changes.patch').open('x') as f:f.write('\n'.join(difflib.unified_diff((old/'launch.py').read_text().splitlines(),launch.splitlines(),fromfile='cargo-v1/launch.py',tofile='cargo-v2/launch.py',lineterm=''))+'\n')
reservation=dict(status='root authorized once; no execution yet',observer=p['observer'],observer_sha256=p['observer_sha256'],
 outputs={s['name']:s['out'] for s in p['stages']},source_binding=bind(a/'source-v2/binding.json'),caller=bind(new/'launch.py'),plan=bind(new/'plan.json'))
with (new/'reservation.json').open('x') as f:json.dump(reservation,f,indent=2);f.write('\n')
print(json.dumps(reservation,indent=2))
