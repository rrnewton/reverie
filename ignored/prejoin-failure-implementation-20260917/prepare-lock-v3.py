from pathlib import Path
import ast,hashlib,json
root=Path.cwd();artifact=root/'ignored/prejoin-failure-implementation-20260917';old=artifact/'cargo-v2';new=artifact/'cargo-v3';new.mkdir()
def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def bind(path):
 path=Path(path);s=path.stat();return dict(path=str(path),resolved_path=str(path.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(path))
plan=json.loads((old/'plan.json').read_text())
lock=root/'Cargo.lock';assert digest(lock)=='d432b018c022722ac6a8d121a652402dd6089f0eb92e5ab6bdbc489992e00469'
with (new/'Cargo.lock.before').open('xb') as f:f.write(lock.read_bytes())
for row in plan['inputs']:assert bind(row['path'])==row, 'input changed: '+row['path']
plan['status']='root authorized workspace lock refresh only, before a separately reviewed locked compile'
plan['run_root']=str(new/'lock-run-1');plan['tmpdir']=str(new/'lock-run-1/tmp')
plan['observer_root']=str(Path(plan['observer']).parent/'measurement-prejoin-native-20260917/cargo-v3')
plan['target_dir']=str(root/'target/prejoin-lock-v3')
plan['environment_fixed']['TMPDIR']=plan['tmpdir'];plan['environment_fixed']['CARGO_TARGET_DIR']=plan['target_dir']
plan['execution']=['/usr/bin/python3','-B',str(new/'lock.py')]
plan['inputs']+=[bind(old/'launch.py'),bind(new/'Cargo.lock.before')]
plan['prior_refusal']=str(old/'FAILURE.json')
plan['permitted_output_mutation']=str(lock)
stage=dict(name='lock',cpu_usec=5000000,wall_seconds=15,stderr_limit_bytes=1048576,reader_limit_bytes=1048576,cwd=str(root),out=str(Path(plan['observer_root'])/'lock'))
stage['payload']=[plan['environment_fixed']['PATH'].split(':')[0]+'/cargo','update','--workspace','--offline']
stage['argv']=['/usr/bin/python3','-B',plan['observer'],'--out',stage['out'],'--cpu-usec','5000000','--wall-seconds','15','--log-bytes','1048576']+stage['payload']
plan['stages']=[stage]
with (new/'lock-plan.json').open('x') as f:json.dump(plan,f,indent=2);f.write('\n')
script='''#!/usr/bin/python3
"""One authorized offline workspace lock refresh; no compilation or test."""
from pathlib import Path
import difflib
import hashlib
import json
import os
import runpy
import subprocess
HERE = Path(__file__).resolve().parent
PLAN_SHA256 = '__PLAN_HASH__'
HELPER_SHA256 = '__HELPER_HASH__'

def main():
    raw = (HERE/'lock-plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == PLAN_SHA256
    plan = json.loads(raw)
    helper = HERE.parent/'cargo-v2/launch.py'
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == HELPER_SHA256
    helpers = runpy.run_path(str(helper), run_name='locked_plan_helpers_only')
    check_inputs, write_new = helpers['check_inputs'], helpers['write_new']
    require, digest, read_bounded = helpers['require'], helpers['digest'], helpers['read_bounded']
    require(plan['execution'] == ['/usr/bin/python3','-B',str(HERE/'lock.py')], 'wrong lock caller')
    require(len(plan['stages']) == 1 and plan['stages'][0]['payload'][1:] == ['update','--workspace','--offline'], 'changed operation')
    stage=plan['stages'][0]
    for path in [plan['run_root'],plan['observer_root'],plan['target_dir']]:
        require(not Path(path).exists() and not Path(path).is_symlink(), 'earlier attempt at '+path)
    check_inputs(plan)
    root=Path(plan['run_root']);root.mkdir(mode=0o700);Path(plan['tmpdir']).mkdir(mode=0o700)
    env={key:os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    env.update(plan['environment_fixed'])
    write_new(root/'launch.json',dict(plan_sha256=PLAN_SHA256,caller_sha256=digest(__file__),execution=plan['execution'],
              argv=stage['argv'],cwd=stage['cwd'],environment=env,old_lock_sha256=digest(HERE/'Cargo.lock.before')))
    record=dict(status='failed before completion')
    try:
        with (root/'observer.stdout').open('xb') as stdout, (root/'observer.stderr').open('xb') as stderr:
            process=subprocess.run(stage['argv'],cwd=stage['cwd'],env=env,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr)
        result_path=Path(stage['out'])/'result.json'
        result=json.loads(read_bounded(result_path,1024**2))
        record.update(observer_exit=process.returncode,result_path=str(result_path),result_sha256=digest(result_path),result=result)
        write_new(root/'lock-readback.json',record)
        helpers['require_terminal'](result,process.returncode,stage,root,env)
        read_bounded(Path(stage['out'])/'stdout',1024**2)
        read_bounded(Path(stage['out'])/'stderr',1024**2)
        record['status']='workspace lock refresh completed'
    except Exception as error:
        record['error']=str(error)
        raise
    finally:
        lock=Path(plan['permitted_output_mutation'])
        before=(HERE/'Cargo.lock.before').read_bytes()
        after=lock.read_bytes()
        with (HERE/'Cargo.lock.after').open('xb') as output:output.write(after)
        delta=''.join(difflib.unified_diff(before.decode().splitlines(keepends=True),after.decode().splitlines(keepends=True),fromfile='Cargo.lock.before',tofile='Cargo.lock.after'))
        with (HERE/'Cargo.lock.patch').open('x') as output:output.write(delta)
        record.update(old_lock_sha256=hashlib.sha256(before).hexdigest(),new_lock_sha256=hashlib.sha256(after).hexdigest(),
                      diff_sha256=digest(HERE/'Cargo.lock.patch'),scope='Dependency lock refresh only; no compilation, test inventory or execution.')
        # The one explicitly permitted mutation must not mask source/config changes.
        remaining=dict(plan,inputs=[row for row in plan['inputs'] if row['path'] != str(lock)])
        try:
            check_inputs(remaining)
            record['source_and_other_inputs_unchanged']=True
        except Exception as error:
            record['source_and_other_inputs_unchanged']=False
            record['postcheck_error']=str(error)
            record['status']='failed input identity check'
        write_new(root/'summary.json',record)
    print(json.dumps(dict(status=record['status'],summary=str(root/'summary.json'))))
    require(record['source_and_other_inputs_unchanged'],'source/config changed during lock refresh')

if __name__ == '__main__':
    main()
'''.replace('__PLAN_HASH__',digest(new/'lock-plan.json')).replace('__HELPER_HASH__',digest(old/'launch.py'))
ast.parse(script)
with (new/'lock.py').open('x') as f:f.write(script)
print(json.dumps(dict(old_lock=bind(new/'Cargo.lock.before'),plan=bind(new/'lock-plan.json'),caller=bind(new/'lock.py'),output=stage['out']),indent=2))
