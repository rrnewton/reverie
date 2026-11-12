#!/usr/bin/python3
"""Execute one explicitly released, source-bound native Cargo plan."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

HERE = Path(__file__).resolve().parent
PLAN = HERE / 'plan.json'
PLAN_SHA256 = '4b8f799d674f2af53f86d319669cf65912a1d6bcc1168bf19def99c11db6e97e'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def read_bounded(path, maximum):
    with Path(path).open('rb') as source:
        content = source.read(maximum + 1)
    require(len(content) <= maximum, 'retained output exceeds reader bound: ' + str(path))
    return content


def write_new(path, value):
    with Path(path).open('x') as output:
        json.dump(value, output, indent=2, allow_nan=False)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())


def check_inputs(plan):
    for row in plan['inputs']:
        path = Path(row['path'])
        metadata = path.stat()
        require(str(path.resolve(strict=True)) == row['resolved_path'], 'input resolution: ' + str(path))
        require(metadata.st_size == row['bytes'], 'input size: ' + str(path))
        require(metadata.st_mode & 0o7777 == row['mode'], 'input mode: ' + str(path))
        require(digest(path) == row['sha256'], 'input bytes: ' + str(path))
    root = Path(plan['source_root'])
    # Frozen Git links are dependency pins, not a claim of checked-out expanded
    # submodule contents. KVM's local source files are individually byte-bound.
    for row in json.loads(read_bounded(plan['source_manifest'], 16 * 1024**2)):
        if 'gitlink' in row:
            continue
        path = root / row['path']
        require(path.is_relative_to(root), 'source path escaped checkout')
        if row['mode'] == '120000':
            require(path.is_symlink(), 'source symlink changed: ' + str(path))
            value = hashlib.sha256(path.readlink().as_posix().encode()).hexdigest()
        else:
            require(not path.is_symlink() and stat.S_ISREG(path.stat().st_mode), 'source type: ' + str(path))
            require(bool(path.stat().st_mode & 0o111) == (row['mode'] == '100755'), 'source executable bit: ' + str(path))
            value = digest(path)
        require(value == row['sha256'], 'source bytes: ' + str(path))
    for row in json.loads(read_bounded(plan['source_binding'], 1024**2))['files']:
        path = root / row['path']
        require(path.stat().st_mode & 0o7777 == int(row['mode'], 8), 'changed source mode: ' + str(path))
        require(path.stat().st_size == row['bytes'] and digest(path) == row['sha256'], 'changed source identity: ' + str(path))


def require_terminal(result, observer_exit, step, root, environment):
    # Preserve the actual result before accepting or refusing any measurement.
    require(result['accounting_complete'] is True, 'incomplete service accounting')
    require(result['final_accounting']['cgroup_empty'] is True, 'service cgroup is not empty')
    props = result['final_accounting']['properties']
    require(props['ActiveState'] in ['inactive', 'failed'], 'service remains active')
    require(props['MainPID'] == 0 and props['ControlGroup'] == '', 'service retains processes')
    unit = props['Id']
    require(re.fullmatch(r'safehermit-[A-Za-z0-9T_-]+\.service', unit), 'unexpected service name')
    command = ['/usr/bin/systemctl', '--user', 'show', unit,
               '--property=LoadState,ActiveState,SubState,MainPID,ControlGroup']
    post = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    require(len(post.stdout) <= 8192 and len(post.stderr) <= 8192, 'unexpected systemctl output size')
    fields = dict(line.split('=', 1) for line in post.stdout.decode().splitlines() if '=' in line)
    write_new(root / (step['name'] + '-service-post.json'),
              {'argv': command, 'exit': post.returncode, 'properties': fields,
               'stderr': post.stderr.decode(errors='replace')})
    require(post.returncode == 0, 'terminal systemctl read failed')
    require(fields.get('ActiveState') in ['inactive', 'failed'] and fields.get('MainPID') == '0'
            and fields.get('ControlGroup') == '', 'post-read service retains work')
    require(result['observer_error'] is None and result['stop_reason'] is None, 'observer failure or bound')
    require(result['final_report']['truncated'] == 'false', 'fatal diagnostic cap reached')
    require(not result.get('library_readback_error') and not result.get('reference_cleanup_error'), 'observer cleanup error')
    require(observer_exit == 0 and result['wrapper_exit_code'] == 0, step['name'] + ' failed')
    require(props['ActiveState'] == 'inactive' and fields.get('ActiveState') == 'inactive', 'successful service is not inactive')


def select_executable(raw, plan):
    artifacts = []
    finished = []
    for line in raw.splitlines():
        row = json.loads(line)
        if row.get('reason') == 'build-finished':
            finished.append(row.get('success'))
        if row.get('reason') == 'compiler-artifact' and row.get('executable') is not None:
            if row.get('target', {}).get('name') == 'reverie_kvm' and row.get('profile', {}).get('test') is True:
                require(row.get('manifest_path') == str(Path(plan['source_root']) / 'reverie-kvm/Cargo.toml'),
                        'test executable manifest identity differs')
                require(row['target'].get('kind') == ['lib'], 'unexpected test target kind')
                artifacts.append(row)
    require(finished == [True], 'Cargo did not report one successful completed build')
    require(len(artifacts) == 1, 'Cargo did not report exactly one KVM library-test executable')
    executable = Path(artifacts[0]['executable'])
    require(executable.is_absolute() and not executable.is_symlink(), 'test executable path is not an owned regular file')
    resolved = executable.resolve(strict=True)
    require(resolved.is_relative_to(Path(plan['target_dir'])) and stat.S_ISREG(executable.stat().st_mode),
            'test executable escaped owned target')
    require(os.access(executable, os.X_OK), 'test executable is not executable')
    return {'path': str(executable), 'sha256': digest(executable), 'bytes': executable.stat().st_size,
            'mode': executable.stat().st_mode & 0o7777, 'cargo_artifact': artifacts[0]}


def check_executable(artifact):
    path = Path(artifact['path'])
    require(not path.is_symlink() and path.stat().st_size == artifact['bytes']
            and path.stat().st_mode & 0o7777 == artifact['mode']
            and digest(path) == artifact['sha256'], 'compiled test executable changed')


def check_prior_compile(plan, environment):
    prior = plan['prior_compile']
    prior_plan = json.loads(read_bounded(prior['plan'], 1024**2))
    prior_readback = json.loads(read_bounded(prior['readback'], 1024**2))
    prior_result = json.loads(read_bounded(prior['result'], 1024**2))
    prior_summary = json.loads(read_bounded(prior['summary'], 1024**2))
    require(prior_plan['target_dir'] == plan['target_dir']
            == plan['environment_fixed']['CARGO_TARGET_DIR'], 'cache is not the previously owned target')
    target = Path(plan['target_dir'])
    require(not target.is_symlink() and target.is_dir()
            and target.resolve(strict=True) == target and target.stat().st_uid == os.getuid(),
            'target cache is not an owned, unredirected directory')
    require(prior['expected_exit'] == 101 and prior_result['wrapper_exit_code'] == 101
            and prior_readback['cargo_exit'] == 101, 'prior failed compile identity changed')
    require(prior_summary['status'] == 'failed' and prior_summary['active_stage'] == 'compile'
            and prior_summary['retained_stages'] == ['compile'] and prior_summary['artifact'] is None,
            'prior sequence did not stop at compilation')
    require(prior_readback['listed_or_executed_test_names'] == []
            and prior_readback['kvm_library_test_executables'] == [], 'prior native execution differs')
    require(prior_result['accounting_complete'] is True
            and prior_result['final_accounting']['cgroup_empty'] is True
            and prior_result['observer_error'] is None and prior_result['stop_reason'] is None,
            'prior compile lacks complete, empty terminal accounting')
    props = prior_result['final_accounting']['properties']
    require(props['ActiveState'] == 'inactive' and props['MainPID'] == 0
            and props['ControlGroup'] == '', 'prior compile retained processes')
    unit = props['Id']
    require(re.fullmatch(r'safehermit-[A-Za-z0-9T_-]+\.service', unit), 'unexpected prior service name')
    command = ['/usr/bin/systemctl', '--user', 'show', unit,
               '--property=LoadState,ActiveState,SubState,MainPID,ControlGroup']
    post = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    require(len(post.stdout) <= 8192 and len(post.stderr) <= 8192, 'unexpected prior service output size')
    fields = dict(line.split('=', 1) for line in post.stdout.decode().splitlines() if '=' in line)
    require(post.returncode == 0 and fields.get('ActiveState') == 'inactive'
            and fields.get('MainPID') == '0' and fields.get('ControlGroup') == '',
            'prior service is no longer inactive and empty')
    return {'argv': command, 'exit': post.returncode, 'properties': fields,
            'stderr': post.stderr.decode(errors='replace'), 'target_dir': str(target),
            'scope': 'Prior owned cache reuse; source and actual Cargo artifact remain independently verified.'}


def main():
    require(digest(PLAN) == PLAN_SHA256, 'concrete execution plan changed')
    plan = json.loads(read_bounded(PLAN, 1024**2))
    require(Path(plan['run_root']) == HERE / 'run-1', 'unexpected run output destination')
    require([step['name'] for step in plan['stages']] == ['compile', 'list', 'native'], 'unexpected stage order')
    require(plan['required_count'] == 36 and len(set(plan['selected_tests'])) == 36, 'selected population changed')
    root = Path(plan['run_root'])
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain every earlier attempt: ' + str(path))
    check_inputs(plan)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    prior_terminal = check_prior_compile(plan, environment)
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    write_new(root / 'prior-compile-terminal-readback.json', prior_terminal)
    write_new(root / 'launch.json', {'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
              'environment': environment, 'source_binding_sha256': digest(plan['source_binding']),
              'scope': 'One native KVM library build/list/selected run; no Hermit or KVM guest.'})
    records = []
    artifact = None
    active_stage = None
    try:
        for step in plan['stages']:
            active_stage = step['name']
            check_inputs(plan)
            argv = list(step['argv'])
            if artifact is not None:
                check_executable(artifact)
                require(argv.count('<verified-compiled-test-executable>') == 1, 'unbound executable placeholder')
                argv[argv.index('<verified-compiled-test-executable>')] = artifact['path']
            else:
                require(active_stage == 'compile' and '<verified-compiled-test-executable>' not in argv,
                        'native dispatch before successful build')
            write_new(root / (active_stage + '-dispatch.json'), {'argv': argv, 'cwd': step['cwd'],
                      'artifact': artifact, 'selected_tests': plan['selected_tests'] if active_stage == 'native' else None})
            # The unchanged reviewed observer owns lifetime, service bounds and
            # cleanup. Do not interrupt its final accounting with a shorter wait.
            with (root / (active_stage + '-observer.stdout')).open('xb') as stdout:
                with (root / (active_stage + '-observer.stderr')).open('xb') as stderr:
                    process = subprocess.run(argv, cwd=step['cwd'], env=environment,
                                             stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            result_path = Path(step['out']) / 'result.json'
            require(result_path.is_file(), 'observer did not retain a result: ' + active_stage)
            result = json.loads(read_bounded(result_path, 1024**2))
            record = {'stage': active_stage, 'observer_exit': process.returncode,
                      'result_path': str(result_path), 'result_sha256': digest(result_path), 'result': result}
            records.append(record)
            write_new(root / (active_stage + '-readback.json'), record)
            require_terminal(result, process.returncode, step, root, environment)
            raw = read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
            read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
            if active_stage == 'compile':
                artifact = select_executable(raw, plan)
                write_new(root / 'compiled-executable.json', artifact)
                lockfile = Path(plan['source_root']) / 'Cargo.lock'
                require(lockfile.is_file() and digest(lockfile) == plan['dependency_lockfile']['sha256'],
                        'Cargo changed the bound dependency lock')
                write_new(root / 'dependency-lockfile.json', {'path': str(lockfile),
                          'bytes': lockfile.stat().st_size, 'sha256': digest(lockfile)})
            elif active_stage == 'list':
                listed = [line[:-6] for line in raw.decode().splitlines() if line.endswith(': test')]
                require(all(listed.count(name) == 1 for name in plan['selected_tests']), 'a selected native test is absent or duplicated')
                write_new(root / 'selected-list-readback.json', {'selected': plan['selected_tests'], 'count': 36,
                          'listed_total': len(listed), 'raw_sha256': hashlib.sha256(raw).hexdigest()})
            else:
                text = raw.decode()
                summaries = re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', text, re.M)
                require(len(summaries) == 1 and summaries[0][:4] == ('36', '0', '0', '0'),
                        'native summary lacks exactly 36 passing, unignored tests')
                write_new(root / 'native-summary-readback.json', {'selected': plan['selected_tests'], 'count': 36,
                          'summary_counts': summaries[0], 'raw_sha256': hashlib.sha256(raw).hexdigest()})
            check_inputs(plan)
            if artifact is not None:
                check_executable(artifact)
    except Exception as error:
        write_new(root / 'summary.json', {'status': 'failed', 'active_stage': active_stage, 'error': str(error),
                  'retained_stages': [row['stage'] for row in records], 'artifact': artifact,
                  'scope': 'Retained failure; no source repair, retry, guest or parity claim.'})
        raise
    write_new(root / 'summary.json', {'status': 'passed', 'artifact': artifact, 'selected_count': 36,
              'observed': [{'stage': row['stage'], 'exit': row['result']['wrapper_exit_code'],
                            'aggregate_cpu_nsec': row['result']['final_accounting']['cpu_usage_nsec'],
                            'wall_seconds': row['result']['elapsed_seconds']} for row in records],
              'scope': 'Native selected controls only; no Hermit guest or cross-backend qualification.'})
    print(json.dumps({'status': 'passed', 'summary': str(root / 'summary.json')}))


if __name__ == '__main__':
    main()
