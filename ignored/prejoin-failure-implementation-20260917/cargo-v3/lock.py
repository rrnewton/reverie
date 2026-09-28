#!/usr/bin/python3
"""One authorized offline workspace lock refresh; no compilation or test."""
from pathlib import Path
import difflib
import hashlib
import json
import os
import runpy
import subprocess
HERE = Path(__file__).resolve().parent
PLAN_SHA256 = '59e83a179ec2916d848a22e38eeb112dee2b38014567f888b5005a4acb51b3f6'
HELPER_SHA256 = '2dbb36eee6d034cc9b50746669bd7dd99a4a2c914dbba1ba078bf27ae843c9b0'

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
