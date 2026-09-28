#!/usr/bin/python3
"""Push the exact qualified commit normally, retaining every result."""
from pathlib import Path
import hashlib
import json
import os
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
PLAN_SHA256 = '47747acb3ede07f26bfc442e2e198997ef17de6b9ca2a30dde722e5c1e251630'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def command(argv, cwd, timeout=60):
    result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    return {'argv': argv, 'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}


def main():
    assert digest(HERE / 'plan.json') == PLAN_SHA256
    plan = json.loads((HERE / 'plan.json').read_text())
    for item in [plan['helpers'], *plan['inputs'], *plan['source_plans']]:
        assert digest(item['path']) == item['sha256'], item['path']
    helper = runpy.run_path(plan['helpers']['path'], run_name='reviewed_helpers_only')
    plans = [json.loads(Path(item['path']).read_text()) for item in plan['source_plans']]
    stage = plan['stage']
    assert str(Path(plan['interpreter']['declared_path']).resolve()) == plan['interpreter']['resolved_path']
    assert digest(plan['interpreter']['resolved_path']) == plan['interpreter']['sha256']
    assert Path('/usr/bin/with-proxy').read_text().splitlines()[0] == plan['interpreter']['shebang']
    assert stage['payload'] == [plan['interpreter']['resolved_path'], '/usr/bin/with-proxy', '/usr/bin/git', 'push', 'origin', 'HEAD:' + plan['remote_branch']]
    assert stage['argv'][stage['argv'].index('--log-bytes') + 2:] == stage['payload']
    assert plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')]
    root = Path(plan['run_root'])
    assert not root.exists() and not Path(stage['out']).parent.exists()
    root.mkdir(mode=0o700)
    Path(plan['environment_fixed']['TMPDIR']).mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])

    def check_source():
        for source_plan in plans:
            helper['check_inputs'](source_plan)
        helper['check_executable'](plans[0]['artifact'])
        for artifact in plans[1]['artifacts'].values():
            helper['check_executable'](artifact)
        for args, expected in [(['rev-parse', 'HEAD'], plan['head']),
                               (['branch', '--show-current'], plan['local_branch']),
                               (['status', '--porcelain=v1', '--untracked-files=no'], ''),
                               (['ls-files', '--stage', '--', '.githooks/pre-push'], '')]:
            result = command(['/usr/bin/git', *args], plan['source_root'], 10)
            assert result['exit'] == 0 and result['stdout'].strip() == expected, result
        assert not (Path(plan['source_root']) / '.githooks/pre-push').exists()
        assert not (Path(plan['source_root']) / '.githooks/pre-push').is_symlink()

    def remote(name):
        result = command(['/usr/bin/with-proxy', '/usr/bin/git', 'ls-remote', '--heads', 'origin',
                          plan['remote_branch'], plan['preserved_remote_branch']], plan['source_root'])
        write(root / (name + '.json'), result)
        assert result['exit'] == 0, result
        refs = {line.split()[1]: line.split()[0] for line in result['stdout'].splitlines()}
        assert refs[plan['preserved_remote_branch']] == plan['preserved_remote_tip'], refs
        return refs

    try:
        check_source()
        native = json.loads((HERE.parent / 'native-forward-v1/RESULT.json').read_text())
        qualification = json.loads((HERE.parent / 'qualification-execution-v6/RESULT.json').read_text())
        assert native['status'] == 'passed'
        assert qualification['status'] == 'passed' and qualification['accepted_count'] == 26
        assert qualification['accepted_vm_count'] == 4 and qualification['accepted_static_count'] == 22
        assert qualification['unexecuted_count'] == 0
        before = remote('remote-before')
        assert plan['remote_branch'] not in before, before
        write(root / 'launch.json', {'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
                                     'execution': plan['execution'], 'head': plan['head'],
                                     'stage': stage, 'environment': environment,
                                     'hook_expectation': plan['hook_expectation']})
        with (root / 'observer.stdout').open('xb') as stdout, (root / 'observer.stderr').open('xb') as stderr:
            process = subprocess.run(stage['argv'], cwd=stage['cwd'], env=environment,
                                     stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        result_path = Path(stage['out']) / 'result.json'
        result = json.loads(helper['read_bounded'](result_path, 1024**2))
        write(root / 'push-readback.json', {'observer_exit': process.returncode, 'result': result,
                                           'result_sha256': digest(result_path)})
        helper['require_terminal'](result, process.returncode, stage, root, environment)
        stdout = helper['read_bounded'](Path(stage['out']) / 'stdout', stage['reader_limit_bytes'])
        stderr = helper['read_bounded'](Path(stage['out']) / 'stderr', stage['reader_limit_bytes'])
        print(stdout.decode(), end='')
        print(stderr.decode(), end='')
        after = remote('remote-after')
        assert after[plan['remote_branch']] == plan['head'], after
        check_source()
        write(root / 'RESULT.json', {'status': 'passed', 'head': plan['head'], 'remote_refs': after,
                                     'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
                                     'launch_sha256': digest(root / 'launch.json'),
                                     'cpu_nsec': result['final_accounting']['cpu_usage_nsec'],
                                     'wall_seconds': result['elapsed_seconds'],
                                     'stdout_sha256': hashlib.sha256(stdout).hexdigest(),
                                     'stderr_sha256': hashlib.sha256(stderr).hexdigest(),
                                     'hook_expectation': plan['hook_expectation']})
        print(json.dumps({'status': 'passed', 'result_sha256': digest(root / 'RESULT.json')}))
    except Exception as error:
        write(root / 'FAILURE.json', {'status': 'failed', 'error': str(error)})
        raise


if __name__ == '__main__':
    main()
