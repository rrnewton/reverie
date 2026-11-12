#!/usr/bin/python3
"""Execute one explicitly released Clippy check after bound native controls pass."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

HERE = Path(__file__).resolve().parent
PLAN = HERE / 'plan.json'
PLAN_SHA256 = 'a13af803508b0175c78755499b366801312da4cf8fbd584b7162c2b11bc89ec5'


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


def check_native_prerequisite(plan, environment):
    prerequisite = plan['native_prerequisite']
    native_plan = json.loads(read_bounded(prerequisite['plan'], 1024**2))
    native_root = Path(prerequisite['run_root'])
    require(native_plan['run_root'] == str(native_root)
            and native_plan['source_binding'] == plan['source_binding']
            and native_plan['source_manifest'] == plan['source_manifest']
            and native_plan['target_dir'] == plan['target_dir'], 'native prerequisite source or target differs')
    require(prerequisite['required_selected_count'] == native_plan['required_count'] == 37
            and len(set(native_plan['selected_tests'])) == 37, 'native population differs')
    summary = json.loads(read_bounded(native_root / 'summary.json', 1024**2))
    launch = json.loads(read_bounded(native_root / 'launch.json', 1024**2))
    require(summary['status'] == 'passed' and summary['selected_count'] == 37,
            'native controls have not passed')
    require(launch['plan_sha256'] == digest(prerequisite['plan'])
            and launch['caller_sha256'] == digest(prerequisite['caller'])
            and launch['source_binding_sha256'] == digest(plan['source_binding']),
            'native execution source or caller binding differs')
    artifact = json.loads(read_bounded(native_root / 'compiled-executable.json', 1024**2))
    require(summary['artifact'] == artifact, 'native executable records disagree')
    executable = Path(artifact['path'])
    require(executable.resolve(strict=True).is_relative_to(Path(plan['target_dir']))
            and stat.S_ISREG(executable.stat().st_mode), 'native executable escaped owned target')
    check_executable(artifact)
    native_stage = next(step for step in native_plan['stages'] if step['name'] == 'native')
    raw = read_bounded(Path(native_stage['out']) / 'stdout', 1024**2)
    outcomes = re.findall(r'^test ([A-Za-z0-9_:]+) \.\.\. (ok|FAILED)$', raw.decode(), re.M)
    require(len(outcomes) == 37 and len(set(name for name, _ in outcomes)) == 37
            and set(name for name, _ in outcomes) == set(native_plan['selected_tests'])
            and all(status == 'ok' for _, status in outcomes), 'native individual controls did not all pass')
    require(re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured;',
                       raw.decode(), re.M) == [('37', '0', '0', '0')], 'native summary differs')
    target = Path(plan['target_dir'])
    require(not target.is_symlink() and target.is_dir() and target.resolve(strict=True) == target
            and target.stat().st_uid == os.getuid(), 'target is not an owned unredirected directory')
    require([step['name'] for step in native_plan['stages']] == ['compile', 'list', 'native'],
            'native stage population differs')
    readbacks = []
    for step in native_plan['stages']:
        result_path = Path(step['out']) / 'result.json'
        result = json.loads(read_bounded(result_path, 1024**2))
        require(result['wrapper_exit_code'] == 0 and result['accounting_complete'] is True
                and result['observer_error'] is None and result['stop_reason'] is None
                and result['final_accounting']['cgroup_empty'] is True, 'native stage did not complete cleanly')
        props = result['final_accounting']['properties']
        require(props['ActiveState'] == 'inactive' and props['MainPID'] == 0
                and props['ControlGroup'] == '', 'native stage retained processes')
        unit = props['Id']
        require(re.fullmatch(r'safehermit-[A-Za-z0-9T_-]+\.service', unit), 'unexpected native service name')
        command = ['/usr/bin/systemctl', '--user', 'show', unit,
                   '--property=LoadState,ActiveState,SubState,MainPID,ControlGroup']
        post = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
        require(len(post.stdout) <= 8192 and len(post.stderr) <= 8192, 'unexpected native service output size')
        fields = dict(line.split('=', 1) for line in post.stdout.decode().splitlines() if '=' in line)
        require(post.returncode == 0 and fields.get('ActiveState') == 'inactive'
                and fields.get('MainPID') == '0' and fields.get('ControlGroup') == '',
                'native service remains active')
        readbacks.append({'stage': step['name'], 'result_path': str(result_path),
                          'result_sha256': digest(result_path), 'properties': fields, 'exit': post.returncode})
    return {'artifact': artifact, 'services': readbacks, 'selected_count': 37,
            'individual_output_sha256': hashlib.sha256(raw).hexdigest()}


def main():
    require(digest(PLAN) == PLAN_SHA256, 'concrete Clippy plan changed')
    plan = json.loads(read_bounded(PLAN, 1024**2))
    root = Path(plan['run_root'])
    require(root == HERE / 'run-1', 'unexpected Clippy output destination')
    require(plan['stage']['name'] == 'clippy', 'unexpected Clippy stage')
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain every earlier attempt: ' + str(path))
    check_inputs(plan)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    native = check_native_prerequisite(plan, environment)
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    write_new(root / 'native-prerequisite-readback.json', native)
    write_new(root / 'launch.json', {'plan_sha256': PLAN_SHA256, 'caller_sha256': digest(__file__),
              'source_binding_sha256': digest(plan['source_binding']), 'environment': environment,
              'scope': 'One native Clippy check only; no tests or guests executed by this check.'})
    step = plan['stage']
    try:
        check_inputs(plan)
        check_executable(native['artifact'])
        write_new(root / 'clippy-dispatch.json', {'argv': step['argv'], 'cwd': step['cwd'],
                  'clippy_components': plan['clippy_components'], 'native_artifact': native['artifact']})
        with (root / 'clippy-observer.stdout').open('xb') as stdout:
            with (root / 'clippy-observer.stderr').open('xb') as stderr:
                process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                         stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
        result_path = Path(step['out']) / 'result.json'
        require(result_path.is_file(), 'observer did not retain a Clippy result')
        result = json.loads(read_bounded(result_path, 1024**2))
        write_new(root / 'clippy-readback.json', {'observer_exit': process.returncode,
                  'result_path': str(result_path), 'result_sha256': digest(result_path), 'result': result})
        # Validate source and the native artifact on success or a Clippy refusal.
        check_inputs(plan)
        check_executable(native['artifact'])
        require_terminal(result, process.returncode, step, root, environment)
        diagnostics = []
        for name in ['stdout', 'stderr']:
            raw = read_bounded(Path(step['out']) / name, step['reader_limit_bytes'])
            diagnostics.append({'name': name, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()})
    except Exception as error:
        write_new(root / 'summary.json', {'status': 'failed', 'error': str(error),
                  'scope': 'Retained Clippy failure; no repair, retry or guest.'})
        raise
    write_new(root / 'summary.json', {'status': 'passed', 'exit': result['wrapper_exit_code'],
              'aggregate_cpu_nsec': result['final_accounting']['cpu_usage_nsec'],
              'wall_seconds': result['elapsed_seconds'], 'diagnostics': diagnostics,
              'scope': 'Clippy success only; no runtime or cross-backend qualification.'})
    print(json.dumps({'status': 'passed', 'summary': str(root / 'summary.json')}))


if __name__ == '__main__':
    main()
