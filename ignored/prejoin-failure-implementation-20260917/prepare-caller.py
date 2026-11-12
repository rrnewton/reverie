from pathlib import Path
import ast
import hashlib
import json
import os
import re

root=Path.cwd()
artifact=root/'ignored/prejoin-failure-implementation-20260917'
source=artifact/'source-v1'
caller=artifact/'cargo-v1'
caller.mkdir(exist_ok=False)
old_dir=root/'ignored/proc-fd-design/cargo-v1'
old=json.loads((old_dir/'plan.json').read_text())
selected=json.loads((source/'selected-tests.json').read_text())
plan=dict(old)
plan.update(source_base=json.loads((source/'binding.json').read_text())['base'],
 source_binding=str(source/'binding.json'), source_manifest=str(source/'tracked-source-manifest.json'),
 selected_tests=selected['selected'], new_test_count=len(selected['new']), required_count=len(selected['selected']),
 run_root=str(caller/'run-1'), target_dir=str(root/'target/prejoin-native-v1'),
 observer_root=str(Path(old['observer']).parent/'measurement-prejoin-native-20260917/cargo-v1'),
 tmpdir=str(caller/'run-1/tmp'))
assert len(selected['selected'])==27 and len(selected['new'])==8

def bind(path):
 path=Path(path)
 stat=path.stat()
 return dict(path=str(path),resolved_path=str(path.resolve(strict=True)),bytes=stat.st_size,
             mode=stat.st_mode & 0o7777,sha256=hashlib.sha256(path.read_bytes()).hexdigest())

inputs=[]
for row in old['inputs']:
 if str(root/'ignored/proc-fd-design/source-v1') in row['path']:
  continue
 current=bind(row['path'])
 assert current==row, 'previously reviewed helper changed: '+row['path']
 inputs.append(current)
for name in ['binding.json','tracked-source-manifest.json','selected-tests.json','candidate.patch']:
 inputs.append(bind(source/name))
inputs.append(bind(root/'Cargo.lock'))
plan['locked_dependency_file']=bind(root/'Cargo.lock')
plan['optional_cargo_configs']={}
for path in [root/'.cargo/config',root/'.cargo/config.toml',Path('/home/newton/.cargo/config'),Path('/home/newton/.cargo/config.toml')]:
 if path.exists():
  inputs.append(bind(path))
  plan['optional_cargo_configs'][str(path)]='present and bound'
 else:
  plan['optional_cargo_configs'][str(path)]='absent'
plan['inputs']=inputs
plan['environment_fixed']=dict(old['environment_fixed'],CARGO_TARGET_DIR=plan['target_dir'],TMPDIR=plan['tmpdir'],THIRD_PARTY_BUILD_JOBS='2')
plan['preparation_changes']=[
 'This is the Reverie failure source-v1 candidate: 8 new native controls and 19 unchanged nearby controls. No Hermit or KVM guest is selected.',
 'The existing untracked Cargo.lock is separately byte-bound and compilation uses --locked --offline. A lock refusal is retained as a failed attempt; it does not authorize lock mutation or retry.',
 'CPU/wall bounds, 16 GiB service memory and zero swap match the previously reviewed observer. Cargo and third-party jobs are both 2.',
 'Actual limits are fatal wrapper stderr: compile 16 MiB, list/native 1 MiB; unchanged observer independently caps each retained stdio file at 64 MiB. Caller read limits are separate.',
 'All owned child handles in the new controls are real OS thread JoinHandles. None represents an OS process or proves Detcore scheduler behavior.'
]
plan['stages']=[]
for previous in old['stages']:
 stage=dict(previous)
 stage['out']=str(Path(plan['observer_root'])/stage['name'])
 if stage['name']=='compile':
  stage['payload']=[previous['payload'][0],'test','--locked','--offline','-p','reverie-kvm','--lib','--no-run','--message-format=json']
 elif stage['name']=='native':
  stage['payload']=['<verified-compiled-test-executable>','--exact']+plan['selected_tests']+['--test-threads=1','--nocapture']
 stage['argv']=['/usr/bin/python3','-B',plan['observer'],'--out',stage['out'],'--cpu-usec',str(stage['cpu_usec']),
                '--wall-seconds',str(stage['wall_seconds']),'--log-bytes',str(stage['stderr_limit_bytes'])]+stage['payload']
 plan['stages'].append(stage)
with (caller/'plan.json').open('x') as out:
 json.dump(plan,out,indent=2);out.write('\n')
plan_sha=hashlib.sha256((caller/'plan.json').read_bytes()).hexdigest()
launch=(old_dir/'launch.py').read_text()
launch=launch.replace(old_dir.joinpath('launch.py').read_text().split("PLAN_SHA256 = '")[1].split("'")[0],plan_sha)
launch=re.sub(r'\b36\b','27',launch)
marker="    root = Path(plan['source_root'])\n"
assert launch.count(marker)==1
launch=launch.replace(marker,"    for path, state in plan['optional_cargo_configs'].items():\n        if state == 'absent':\n            require(not Path(path).exists() and not Path(path).is_symlink(), 'new Cargo configuration: ' + path)\n"+marker)
launch=launch.replace("write_new(root / 'generated-lockfile.json'", "write_new(root / 'locked-dependency-readback.json'")
ast.parse(launch)
with (caller/'launch.py').open('x') as out:out.write(launch)
for path in [plan['run_root'],plan['observer_root'],plan['target_dir']]:
 assert not Path(path).exists() and not Path(path).is_symlink(), 'occupied destination: '+path
reservation=dict(status='reserved only; execution not released',observer=plan['observer'],observer_sha256=plan['observer_sha256'],
 outputs={stage['name']:stage['out'] for stage in plan['stages']},source_binding=bind(source/'binding.json'),
 caller=bind(caller/'launch.py'),plan=bind(caller/'plan.json'))
with (caller/'reservation.json').open('x') as out:json.dump(reservation,out,indent=2);out.write('\n')
print(json.dumps(reservation,indent=2))
