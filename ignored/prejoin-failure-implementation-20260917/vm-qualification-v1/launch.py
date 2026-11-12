#!/usr/bin/python3
"""Run three source-bound KVM controls from the actual existing test ELF."""
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
PLAN_SHA256 = '4d891f4d29676543353b416078fd34a829942de696614e2eb0560cbaa2175893'


def main():
    raw = (HERE / 'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == PLAN_SHA256
    plan = json.loads(raw)
    helper = Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == plan['helpers']['sha256']
    functions = runpy.run_path(str(helper), run_name='reviewed_helpers_only')
    require, digest, read_bounded = (functions[name] for name in ['require', 'digest', 'read_bounded'])
    write_new = functions['write_new']
    require(plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')], 'wrong caller')
    require([step['name'] for step in plan['stages']] == ['vm'], 'unexpected stage')
    require(plan['required_count'] == 3 and len(set(plan['selected_tests'])) == 3, 'wrong population')
    root = Path(plan['run_root'])
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain previous attempt: ' + str(path))
    functions['check_inputs'](plan)
    functions['check_executable'](plan['artifact'])
    inventory = read_bounded(plan['existing_inventory'], 1024**2)
    listed = [line[:-6] for line in inventory.decode().splitlines() if line.endswith(': test')]
    require(len(listed) == 443 and len(set(listed)) == 443, 'compiled inventory changed')
    require(all(listed.count(name) == 1 for name in plan['selected_tests']), 'selected control absent')
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    require(environment['REVERIE_REQUIRE_KVM'] == '1', 'KVM refusal must fail the control')
    write_new(root / 'launch.json', dict(plan_sha256=PLAN_SHA256, caller_sha256=digest(__file__),
              execution=plan['execution'], cwd=str(Path.cwd()), environment=environment,
              artifact=plan['artifact'], selected_tests=plan['selected_tests'], scope=plan['scope']))
    step = plan['stages'][0]
    try:
        functions['check_inputs'](plan)
        functions['check_executable'](plan['artifact'])
        write_new(root / 'vm-dispatch.json', dict(argv=step['argv'], cwd=step['cwd'],
                  artifact=plan['artifact'], selected_tests=plan['selected_tests']))
        with (root / 'vm-observer.stdout').open('xb') as stdout, \
             (root / 'vm-observer.stderr').open('xb') as stderr:
            process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                     stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        result_path = Path(step['out']) / 'result.json'
        result = json.loads(read_bounded(result_path, 1024**2))
        write_new(root / 'vm-readback.json', dict(observer_exit=process.returncode,
                  result_path=str(result_path), result_sha256=digest(result_path), result=result))
        functions['require_terminal'](result, process.returncode, step, root, environment)
        raw = read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
        read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
        summaries = re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', raw.decode(), re.M)
        require(summaries == [('3', '0', '0', '0', '440')], 'wrong executed population')
        passed = re.findall(r'^test (.+) \.\.\. ok$', raw.decode(), re.M)
        require(sorted(passed) == sorted(plan['selected_tests']), 'wrong passing identities')
        functions['check_inputs'](plan)
        functions['check_executable'](plan['artifact'])
    except Exception as error:
        write_new(root / 'summary.json', dict(status='failed', error=str(error), scope=plan['scope']))
        raise
    write_new(root / 'summary.json', dict(status='passed', selected=plan['selected_tests'],
              counts=summaries[0], stdout_sha256=hashlib.sha256(raw).hexdigest(),
              cpu_nsec=result['final_accounting']['cpu_usage_nsec'],
              wall_seconds=result['elapsed_seconds'], scope=plan['scope']))
    print(json.dumps(dict(status='passed', summary=str(root / 'summary.json'))))


if __name__ == '__main__':
    main()
