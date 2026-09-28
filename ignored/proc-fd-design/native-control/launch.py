#!/usr/bin/python3
"""Run the reviewed compile/native plan once, after coordinator release."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
PLAN = HERE / 'plan.json'
PLAN_SHA256 = '5fef719f0b6fcc4f3e3da6731c6506d8349e89ebb1e4b034f74a0df12f0665b8'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def write_new(path, value):
    with path.open('x') as output:
        json.dump(value, output, indent=2, allow_nan=False)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())


def check_inputs(plan):
    for row in plan['inputs']:
        path = Path(row['path'])
        metadata = path.stat()
        require(str(path.resolve(strict=True)) == row['resolved_path'], str(path))
        require(metadata.st_size == row['bytes'], str(path))
        require(metadata.st_mode & 0o7777 == row['mode'], str(path))
        require(digest(path) == row['sha256'], str(path))


def read_bounded(path, maximum):
    with path.open('rb') as source:
        content = source.read(maximum + 1)
    require(len(content) <= maximum, 'retained output exceeds reader bound: ' + str(path))
    return content


def main():
    require(digest(PLAN) == PLAN_SHA256, 'measurement plan changed')
    plan = json.loads(PLAN.read_text())
    check_inputs(plan)
    root = Path(plan['run_root'])
    require(root == HERE / 'run-1', 'unexpected output destination')
    require(not root.exists() and not root.is_symlink(), 'retain every prior measurement')
    require([step['name'] for step in plan['steps']] == ['compile', 'native'], 'unexpected stages')
    root.mkdir(mode=0o700)
    Path(plan['native_workdir']).mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['host_environment_keys'] if key in os.environ}
    environment.update(plan['host_environment_fixed'])
    write_new(root / 'launch.json', {
        'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
        'environment': environment, 'observer': plan['observer'],
        'scope': 'Native fdinfo semantics only; no Hermit or KVM guest is launched.',
    })
    records = []
    fixture_hash = None
    try:
        for step in plan['steps']:
            check_inputs(plan)
            if fixture_hash is not None:
                require(digest(plan['fixture_elf']) == fixture_hash, 'compiled fixture changed')
            # The unchanged observer owns service limits and final cleanup.
            # Do not terminate it early while it is accounting for descendants.
            with (root / (step['name'] + '-observer.stdout')).open('xb') as stdout:
                with (root / (step['name'] + '-observer.stderr')).open('xb') as stderr:
                    process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                             stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            result_path = Path(step['out']) / 'result.json'
            require(result_path.is_file(), 'observer did not retain result: ' + step['name'])
            result = json.loads(read_bounded(result_path, 1024 * 1024))
            record = {'step': step['name'], 'observer_exit': process.returncode,
                      'result_path': str(result_path), 'result_sha256': digest(result_path), 'result': result}
            records.append(record)
            write_new(root / (step['name'] + '-readback.json'), record)
            require(result['accounting_complete'] is True, 'incomplete accounting')
            require(result['observer_error'] is None and result['stop_reason'] is None, 'observer failure/bound')
            require(result['final_accounting']['cgroup_empty'] is True, 'service cgroup not empty')
            properties = result['final_accounting']['properties']
            require(properties['ActiveState'] in ['inactive', 'failed'], 'service remains active')
            require(properties['MainPID'] == 0 and properties['ControlGroup'] == '', 'service retains processes')
            require(result['final_report']['truncated'] == 'false', 'retained diagnostic truncated')
            require(not result.get('library_readback_error') and not result.get('reference_cleanup_error'),
                    'reference/library cleanup error')
            require(process.returncode == 0 and result['wrapper_exit_code'] == 0, step['name'] + ' failed')
            require(properties['ActiveState'] == 'inactive', 'successful service is not inactive')
            if step['name'] == 'compile':
                fixture_hash = digest(plan['fixture_elf'])
                write_new(root / 'fixture-elf.json', {'path': plan['fixture_elf'], 'sha256': fixture_hash,
                          'bytes': Path(plan['fixture_elf']).stat().st_size,
                          'source_sha256': plan['fixture_source_sha256']})
            else:
                raw = read_bounded(Path(step['out']) / 'stdout', 1024 * 1024)
                require(read_bounded(Path(step['out']) / 'stderr', 1024 * 1024) == b'', 'native stderr not empty')
                observations = [json.loads(line) for line in raw.splitlines()]
                cases = [row['case'] for row in observations if row.get('status') == 'pass']
                require(cases == plan['expected_cases'], 'native cases missing, duplicated or reordered')
                require(observations[-1] == {'summary': 'fdinfo-observation-ok', 'passed': 9, 'fork_children': 1},
                        'native final summary disagrees')
                write_new(root / 'observations.json', observations)
            check_inputs(plan)
        require(digest(plan['fixture_elf']) == fixture_hash, 'compiled fixture changed')
    except Exception as error:
        write_new(root / 'summary.json', {'status': 'failed', 'error': str(error),
                  'completed_stages': [row['step'] for row in records], 'fixture_sha256': fixture_hash,
                  'scope': 'Preserved native/control failure; no retry or guest execution.'})
        raise
    write_new(root / 'summary.json', {'status': 'passed', 'fixture_sha256': fixture_hash,
              'scope': 'Native fdinfo observation control only; no backend implementation or parity claim.',
              'observed': [{'step': row['step'], 'exit_code': row['result']['wrapper_exit_code'],
                            'cpu_usage_nsec': row['result']['final_accounting']['cpu_usage_nsec'],
                            'elapsed_seconds': row['result']['elapsed_seconds']}
                           for row in records]})
    print(json.dumps({'summary': str(root / 'summary.json'), 'status': 'passed'}))


if __name__ == '__main__':
    main()
