#!/usr/bin/python3
"""Run one current complete library population with the original serial limits."""
import hashlib
import json
import os
from pathlib import Path
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
PLAN_SHA256 = '33d6c0d66bbe64f3395c5b47b5036e71ffc17a4bf14d88c98b93cf3a7441e749'


def load_checked():
    raw = (HERE / 'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == PLAN_SHA256
    plan = json.loads(raw)
    helper = Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == plan['helpers']['sha256']
    functions = runpy.run_path(str(helper), run_name='reviewed_helpers_only')
    require, digest, read_bounded = (functions[name] for name in ['require', 'digest', 'read_bounded'])
    old_caller = Path(plan['original_full_caller'])
    require(digest(old_caller) == plan['original_full_caller_sha256'], 'original full caller changed')
    original = runpy.run_path(str(old_caller), run_name='original_full_helpers_only')
    old = json.loads(read_bounded(plan['original_full_plan'], 1024**2))
    require(plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')], 'wrong caller')
    require(Path(plan['run_root']) == HERE / 'run-1', 'wrong run output')
    require(set(plan['artifacts']) == set(plan['inventories']) == {'lib'}, 'unexpected target')
    artifact = plan['artifacts']['lib']
    names = json.loads(read_bounded(plan['inventories']['lib']['path'], 1024**2))['names']
    require(names == plan['registered_tests'] and
            len(names) == len(set(names)) == plan['registered_count'] == 453, 'current inventory changed')
    require(len(old['registered_tests']) == len(set(old['registered_tests'])) == 427 and
            set(old['registered_tests']) <= set(names), 'original method missing')
    args = ['--test-threads=1', '--nocapture', '-Z', 'unstable-options', '--format=json']
    require(old['stage']['payload'][1:] == args, 'original full libtest arguments changed')
    step = plan['stage']
    require(step['name'] == 'native-full' and step['libtest_argv'] == [artifact['path'], *args],
            'full run gained a filter or changed order/parallelism')
    require(step['payload'] == ['/usr/bin/python3', '-B', plan['admission_helper'],
            '--record', step['admission_record'], '--executable', artifact['path'],
            '--sha256', artifact['sha256'], '--', *args], 'actual admitted command differs')
    require(step['argv'] == ['/usr/bin/python3', '-B', plan['observer'], '--out', step['out'],
            '--cpu-usec', '30000000', '--wall-seconds', '60', '--log-bytes', '1048576',
            *step['payload']], 'observer command or resource bounds changed')
    for key in ['cpu_usec', 'wall_seconds', 'stderr_limit_bytes', 'reader_limit_bytes', 'cwd']:
        require(step[key] == old['stage'][key], 'original bound or working directory changed: ' + key)
    require(plan['service_memory_max_bytes'] == 16 * 1024**3 and plan['service_swap_max_bytes'] == 0,
            'service memory or swap limit changed')
    expected_environment = dict(old['environment_fixed'], CARGO_TARGET_DIR=plan['target_dir'],
                                TMPDIR=plan['tmpdir'], REVERIE_REQUIRE_KVM='1')
    require(plan['environment_fixed'] == expected_environment and
            plan['environment_keys'] == old['environment_keys'], 'unexpected environment change')
    functions['check_inputs'](plan)
    functions['check_executable'](artifact)
    check_source_ref(plan, require)
    return plan, functions, original, names


def check_source_ref(plan, require):
    result = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=plan['source_root'],
                            check=True, capture_output=True, text=True, timeout=5)
    require(result.stdout.strip() == plan['source_head'], 'source HEAD changed')
    result = subprocess.run(['git', 'diff', '--quiet', 'HEAD', '--'], cwd=plan['source_root'],
                            stdin=subprocess.DEVNULL, timeout=5)
    require(result.returncode == 0, 'tracked source or index changed')


def check_admission(plan, result, functions):
    require, read_bounded = (functions[name] for name in ['require', 'read_bounded'])
    step = plan['stage']
    path = Path(step['admission_record'])
    admission = json.loads(read_bounded(path, 65536))
    outcome = json.loads(read_bounded(path.with_suffix('.exit.json'), 65536))
    artifact = plan['artifacts']['lib']
    require(admission['device'] == '/dev/kvm' and admission['api_version'] == 12,
            'missing actual KVM API 12 admission')
    require(admission['test_binary'] == {key: artifact[key] for key in ['path', 'sha256', 'bytes', 'mode']},
            'admitted executable changed')
    require(admission['argv'] == step['libtest_argv'], 'admitted full argument list changed')
    require(admission['hermit_binary'] is None and admission['guest_output'] is None,
            'unexpected Hermit execution')
    groups = [line.split(':', 2)[2] for line in admission['cgroup'].splitlines()]
    service = result['authenticated_service']
    require(service['initial_properties']['ControlGroup'] in groups and
            admission['supervisor_pid'] == service['main_process']['pid'],
            'admission is not in the actual observed service')
    require(outcome == {'exit': result['wrapper_exit_code']}, 'admission/payload exit disagreement')
    return admission, outcome


def main():
    plan, functions, original, names = load_checked()
    require, digest, read_bounded = (functions[name] for name in ['require', 'digest', 'read_bounded'])
    write_new = functions['write_new']
    root = Path(plan['run_root'])
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'preserve earlier attempt: ' + str(path))
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    (root / 'admissions').mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    require(environment['REVERIE_REQUIRE_KVM'] == '1', 'hardware refusal must fail')
    write_new(root / 'launch.json', dict(plan_sha256=PLAN_SHA256, caller_sha256=digest(__file__),
              source_binding_sha256=digest(plan['source_binding']), source_head=plan['source_head'],
              landed_commit=plan['landed_commit'], execution=plan['execution'], environment=environment,
              artifacts=plan['artifacts'], registered_tests=names, scope=plan['scope']))
    step = plan['stage']
    parsed = None
    result = None
    try:
        functions['check_inputs'](plan)
        functions['check_executable'](plan['artifacts']['lib'])
        check_source_ref(plan, require)
        write_new(root / 'native-full-dispatch.json', dict(argv=step['argv'], cwd=step['cwd'],
                  environment=environment, artifact=plan['artifacts']['lib'], registered_tests=names))
        with (root / 'native-full-observer.stdout').open('xb') as stdout, \
             (root / 'native-full-observer.stderr').open('xb') as stderr:
            process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                     stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        result_path = Path(step['out']) / 'result.json'
        result = json.loads(read_bounded(result_path, 1024**2))
        write_new(root / 'native-full-readback.json', dict(observer_exit=process.returncode,
                  result_path=str(result_path), result_sha256=digest(result_path), result=result))
        raw = read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
        stderr = read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
        # Use the original full-suite JSON parser and retain every outcome before
        # checking exit status. Failure never triggers a retry or smaller cohort.
        parsed = original['parse_native_output'](raw, stderr, plan)
        write_new(root / 'native-outcomes.json', parsed)
        functions['check_inputs'](plan)
        functions['check_executable'](plan['artifacts']['lib'])
        check_source_ref(plan, require)
        write_new(root / 'source-after.json', dict(source_head=plan['source_head'],
                  source_binding_sha256=digest(plan['source_binding']),
                  source_manifest_sha256=digest(plan['source_manifest']),
                  inputs=[{'path': row['path'], 'sha256': digest(row['path'])} for row in plan['inputs']],
                  artifact=plan['artifacts']['lib']))
        functions['require_terminal'](result, process.returncode, step, root, environment)
        admission, admission_exit = check_admission(plan, result, functions)
        original['require_complete_population'](parsed, names)
        require(parsed['outcome_counts']['ignored'] == 0, 'full library ignored a test')
        require(not parsed['kvm_unavailable_diagnostics'], 'full library skipped hardware')
    except Exception as error:
        write_new(root / 'summary.json', dict(status='failed', error=str(error),
                  outcome_counts=None if parsed is None else parsed['outcome_counts'],
                  observer_result_present=result is not None, scope=plan['scope']))
        raise
    write_new(root / 'summary.json', dict(status='passed', exit=result['wrapper_exit_code'],
              aggregate_cpu_nsec=result['final_accounting']['cpu_usage_nsec'],
              wall_seconds=result['elapsed_seconds'], registered_count=len(names),
              outcome_counts=parsed['outcome_counts'], native_artifact=plan['artifacts']['lib'],
              admission_sha256=digest(step['admission_record']),
              kvm_unavailable_diagnostics=parsed['kvm_unavailable_diagnostics'], scope=plan['scope']))
    print(json.dumps(dict(status='passed', summary=str(root / 'summary.json'))), flush=True)


if __name__ == '__main__':
    main()
