#!/usr/bin/python3
"""Run exact Reverie VM and lifecycle controls in separately observed services."""
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
PLAN_SHA256 = 'fcc2b7098bf27729961edc287097e2199bae30fe728f578f6baecbff3c99dc2b'


def main():
    raw = (HERE / 'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == PLAN_SHA256
    plan = json.loads(raw)
    helper = Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == plan['helpers']['sha256']
    functions = runpy.run_path(str(helper), run_name='reviewed_helpers_only')
    require, digest, read_bounded = (functions[name] for name in ['require', 'digest', 'read_bounded'])
    write_new, check_executable = (functions[name] for name in ['write_new', 'check_executable'])
    require(plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')], 'wrong caller')
    require(len(plan['selected_tests']['lib']) == 4 and
            len(plan['selected_tests']['static-elf']) == 22, 'changed qualification population')
    selected = []
    for step in plan['stages']:
        artifact = plan['artifacts'][step['artifact']]
        require(step['payload'] == step['argv'][step['argv'].index('--log-bytes') + 2:],
                'payload differs from actual command')
        require(step['payload'] == [
            '/usr/bin/python3', '-B', plan['admission_helper'],
            '--record', step['admission_record'], '--executable', artifact['path'],
            '--sha256', artifact['sha256'], '--', '--exact', '--nocapture',
            '--test-threads', '1', step['test']], 'changed admitted test invocation')
        require(step['test'] in plan['selected_tests'][step['artifact']], 'unselected test')
        require(step['expected_summaries'] == (2 if step['test'].startswith('terminal_fork::') else 1),
                'changed recursive test observation')
        require(step['environment_overrides'] == {'TMPDIR': str(Path(plan['tmpdir']) / step['name'])},
                'unexpected per-test environment')
        selected.append(step['test'])
    require(len(selected) == 26 and len(set(selected)) == 26 and
            set(selected) == set(plan['selected_tests']['lib'] + plan['selected_tests']['static-elf']),
            'missing or duplicated test stage')
    for kind, entry in plan['inventories'].items():
        names = json.loads(read_bounded(entry['path'], 1024**2))['names']
        require(len(names) == entry['count'] and len(names) == len(set(names)), 'inventory changed')
        require(all(names.count(name) == 1 for name in plan['selected_tests'][kind]),
                'selected test absent from actual inventory')
    root = Path(plan['run_root'])
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain prior attempt: ' + str(path))
    functions['check_inputs'](plan)
    for artifact in plan['artifacts'].values():
        check_executable(artifact)
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    (root / 'admissions').mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    require(environment['REVERIE_REQUIRE_KVM'] == '1', 'hardware refusal must fail')
    require('REVERIE_LEADER_EXEC_CHILD' not in environment and
            'REVERIE_TERMINAL_ARTIFACTS' not in environment, 'unexpected inherited fixture state')
    write_new(root / 'launch.json', dict(plan_sha256=PLAN_SHA256, caller_sha256=digest(__file__),
              execution=plan['execution'], cwd=str(Path.cwd()), environment=environment,
              artifacts=plan['artifacts'], selected_tests=plan['selected_tests'], scope=plan['scope']))
    records = []
    active = None
    try:
        for step in plan['stages']:
            active = step['name']
            functions['check_inputs'](plan)
            for artifact in plan['artifacts'].values():
                check_executable(artifact)
            stage_environment = dict(environment, **step['environment_overrides'])
            Path(stage_environment['TMPDIR']).mkdir(mode=0o700)
            write_new(root / (active + '-dispatch.json'), dict(argv=step['argv'], cwd=step['cwd'],
                      environment=stage_environment, artifact=plan['artifacts'][step['artifact']],
                      selected_test=step['test']))
            with (root / (active + '-observer.stdout')).open('xb') as stdout, \
                 (root / (active + '-observer.stderr')).open('xb') as stderr:
                process = subprocess.run(step['argv'], cwd=step['cwd'], env=stage_environment,
                                         stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            result_path = Path(step['out']) / 'result.json'
            result = json.loads(read_bounded(result_path, 1024**2))
            write_new(root / (active + '-readback.json'), dict(observer_exit=process.returncode,
                      result_path=str(result_path), result_sha256=digest(result_path), result=result))
            # Any command failure stops dependent stages, after actual service
            # accounting and an independent inactive/empty readback.
            functions['require_terminal'](result, process.returncode, step, root, stage_environment)
            raw_stdout = read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
            raw_stderr = read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
            stdout, stderr = raw_stdout.decode(), raw_stderr.decode()
            require('skipping ' not in (stdout + stderr).lower(), 'test skipped hardware')
            count = plan['inventories'][step['artifact']]['count']
            expected = [('1', '0', '0', '0', str(count - 1))] * step['expected_summaries']
            summaries = re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', stdout, re.M)
            require(summaries == expected, 'wrong executed test population')
            # Terminal-fork's unchanged subprocess inherits output, yielding
            # two summaries and one named success line. Other recursive tests
            # capture and assert their child result, yielding one outer line.
            passed = re.findall(r'^test (.+) \.\.\. ok$', stdout, re.M)
            require(passed == [step['test']], 'wrong passing test identity')
            admission_path = Path(step['admission_record'])
            admission = json.loads(read_bounded(admission_path, 65536))
            admission_exit = json.loads(read_bounded(admission_path.with_suffix('.exit.json'), 65536))
            require(admission_exit == {'exit': 0}, 'admitted test did not return successfully')
            require(admission['device'] == '/dev/kvm' and admission['api_version'] == 12,
                    'missing real KVM device admission')
            expected_artifact = plan['artifacts'][step['artifact']]
            require(admission['test_binary'] == {key: expected_artifact[key]
                    for key in ['path', 'sha256', 'bytes', 'mode']}, 'admitted executable mismatch')
            require(admission['hermit_binary'] is None and admission['guest_output'] is None,
                    'unexpected Hermit execution in Reverie qualification')
            require(admission['argv'] == [expected_artifact['path'], '--exact', '--nocapture',
                    '--test-threads', '1', step['test']], 'admitted test arguments changed')
            groups = [line.split(':', 2)[2] for line in admission['cgroup'].splitlines()]
            actual_service = result['authenticated_service']['initial_properties']['ControlGroup']
            require(actual_service in groups and
                    admission['supervisor_pid'] == result['authenticated_service']['main_process']['pid'],
                    'hardware admission did not occur inside the observed service')
            functions['check_inputs'](plan)
            for artifact in plan['artifacts'].values():
                check_executable(artifact)
            record = dict(stage=active, test=step['test'], status='passed', counts=summaries,
                          stdout_sha256=hashlib.sha256(raw_stdout).hexdigest(),
                          stderr_sha256=hashlib.sha256(raw_stderr).hexdigest(),
                          admission_sha256=digest(admission_path),
                          cpu_nsec=result['final_accounting']['cpu_usage_nsec'],
                          wall_seconds=result['elapsed_seconds'])
            records.append(record)
            write_new(root / (active + '-outcome.json'), record)
            print(json.dumps(record), flush=True)
    except Exception as error:
        write_new(root / 'summary.json', dict(status='failed', active_stage=active,
                  error=str(error), completed=records, scope=plan['scope']))
        raise
    write_new(root / 'summary.json', dict(status='passed', completed=records, scope=plan['scope']))
    print(json.dumps(dict(status='passed', summary=str(root / 'summary.json'))))


if __name__ == '__main__':
    main()
