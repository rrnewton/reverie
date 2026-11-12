"""Owned preparation helpers; no product command runs on import."""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
MIB = 1024**2


def require(value, message):
    if not value:
        raise RuntimeError(message)


def read(path, limit=16 * MIB):
    with Path(path).open('rb') as source:
        value = source.read(limit + 1)
    require(len(value) <= limit, 'bounded read refused: ' + str(path))
    return value


def json_read(path, limit=16 * MIB):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate JSON key: ' + key)
            result[key] = value
        return result
    return json.loads(read(path, limit), object_pairs_hook=unique)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(MIB), b''):
            value.update(block)
    return value.hexdigest()


def write_new(path, value):
    with Path(path).open('x') as output:
        json.dump(value, output, indent=2, allow_nan=False)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())


def owned(path, root=HERE, directory=False):
    path = Path(path)
    require(path.is_absolute() and path.is_relative_to(root), 'path outside assigned output root')
    for part in [path, *path.parents]:
        if part.exists() or part.is_symlink():
            require(not part.is_symlink(), 'symlink in owned path: ' + str(part))
        if part == root:
            break
    if directory:
        info = path.stat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid(), 'directory ownership/type')
    return path


def file_record(path, limit=512 * MIB):
    path = Path(path)
    info = path.lstat()
    require(path.is_absolute() and stat.S_ISREG(info.st_mode), 'bound file is not regular')
    require(info.st_size <= limit, 'file exceeds complete hashing bound: ' + str(path))
    return dict(path=str(path), bytes=info.st_size, mode=stat.S_IMODE(info.st_mode),
                sha256=digest(path))


def check_file(entry, executable=False):
    path=Path(entry['path']);info=path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_size==entry['bytes'] and stat.S_IMODE(info.st_mode)==entry['mode'], 'file size/mode changed before hashing: '+str(path))
    actual = file_record(path)
    require(actual == entry, 'file binding changed: ' + entry['path'])
    if executable:
        require(os.access(entry['path'], os.X_OK), 'bound program is not executable')
    return actual


def check_source(context):
    record = context['source_manifest']
    check_file(record)
    rows = json_read(record['path'])
    require(isinstance(rows, list) and len(rows) >= 1732, 'incomplete full source snapshot')
    seen = set()
    for entry in rows:
        rel = Path(entry['path'])
        require(not rel.is_absolute() and '..' not in rel.parts and str(rel) not in seen, 'source path')
        seen.add(str(rel))
        path = REPO / rel
        mode = entry['mode']
        if mode == '160000':
            # Gitlink bytes are bound by the separately frozen recursive input manifest.
            require(path.is_dir() and not path.is_symlink(), 'gitlink directory changed')
            continue
        info = path.lstat()
        if mode == '120000':
            require(stat.S_ISLNK(info.st_mode), 'source symlink changed')
            actual = hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
        else:
            require(stat.S_ISREG(info.st_mode), 'source type changed: ' + str(rel))
            require(bool(info.st_mode & 0o111) == (mode == '100755'), 'source executable mode changed')
            actual = digest(path)
        require(actual == entry['sha256'], 'source bytes changed: ' + str(rel))
    require(context['recursive_submodule_inputs'], 'recursive dependency inputs unbound')
    for manifest in context['recursive_submodule_inputs']:
        check_file(manifest)
        dependencies=json_read(manifest['path'])
        require(dependencies, 'empty recursive dependency manifest')
        for item in dependencies:
            check_file(item)
    require(isinstance(context['input_symlinks'], list), 'input symlink manifest missing')
    for item in context['input_symlinks']:
        path = Path(item['path'])
        require(path.is_absolute() and stat.S_ISLNK(path.lstat().st_mode), 'input symlink type changed')
        require(os.readlink(path) == item['target'], 'input symlink target changed: ' + str(path))
    for item in context['inputs']:
        check_file(item)
    for path in context['absent_inputs']:
        require(not Path(path).exists() and not Path(path).is_symlink(), 'unbound override appeared: ' + path)
    for item in context['executables']:
        check_file(item, executable=True)
    target = Path(context['target_cache']['path'])
    owned(target, Path(context['target_cache']['owner_slot']), directory=True)
    info = target.stat()
    require([info.st_dev, info.st_ino, info.st_uid] == context['target_cache']['identity'], 'owned target identity changed')


def signal_owned(process, pidfd, number):
    # pidfd refers to this actual child generation, even if its numeric PID is reused.
    if process.poll() is None:
        try:
            signal.pidfd_send_signal(pidfd, number)
        except ProcessLookupError:
            pass


def bounded_process(argv, cwd, env, prefix, seconds, output_limit=MIB, observer=False):
    """Bound outer transport; product lifetime remains owned by the unchanged observer."""
    prefix = Path(prefix)
    stdout_path, stderr_path = prefix.with_suffix('.stdout'), prefix.with_suffix('.stderr')
    started = time.monotonic()
    result = dict(argv=argv, cwd=str(cwd), wall_limit_seconds=seconds, forced=False)
    with stdout_path.open('xb') as stdout, stderr_path.open('xb') as stderr:
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=stderr)
        pidfd = os.pidfd_open(process.pid)
        try:
            while process.poll() is None:
                too_large = stdout_path.stat().st_size > output_limit or stderr_path.stat().st_size > output_limit
                expired = time.monotonic() - started >= seconds
                if too_large or expired:
                    result['forced'] = True
                    result['reason'] = 'outer_output_limit' if too_large else 'outer_wall_limit'
                    # The observer installs SIGALRM to enter its authenticated service cleanup.
                    signal_owned(process, pidfd, signal.SIGALRM if observer else signal.SIGTERM)
                    try:
                        process.wait(timeout=10 if observer else 1)
                    except subprocess.TimeoutExpired:
                        signal_owned(process, pidfd, signal.SIGKILL)
                        try:
                            process.wait(timeout=1)
                        except subprocess.TimeoutExpired:
                            result['reap_incomplete'] = True
                    break
                time.sleep(0.05)
            result['returncode'] = process.poll()
        finally:
            if process.poll() is None:
                signal_owned(process, pidfd, signal.SIGKILL)
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    result['reap_incomplete'] = True
            os.close(pidfd)
    result['elapsed_seconds'] = time.monotonic() - started
    result['stdout'] = file_record(stdout_path)
    result['stderr'] = file_record(stderr_path)
    write_new(prefix.with_suffix('.json'), result)
    return result


def terminal(result, phase, outer, prefix, env, receipt):
    final = result.get('final_accounting') or result.get('refusal_final_accounting')
    require(final is not None and final['cgroup_empty'] is True, 'missing final service accounting')
    values = final['properties']
    require(values['ActiveState'] in ('inactive', 'failed') and values['MainPID'] == 0, 'service not terminal')
    unit = values['Id']
    require(re.fullmatch(r'safehermit-[A-Za-z0-9_.-]+\.service', unit), 'invalid retained service unit')
    queries = []
    for index in range(2):
        q = bounded_process(['/usr/bin/systemctl', '--user', 'show', unit,
                             '--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'],
                            REPO, env, Path(str(prefix) + '-' + str(index)), 5, 8192)
        text = read(q['stdout']['path'], 8192).decode()
        properties = dict(line.split('=', 1) for line in text.splitlines() if '=' in line)
        q['properties'] = properties
        queries.append(q)
        require(q['returncode'] == 0 and not q['forced'], 'independent terminal query failed')
        require(properties.get('ActiveState') in ('inactive', 'failed') and properties.get('MainPID') == '0'
                and properties.get('ControlGroup') == '', 'fresh service query is not inactive/empty')
    write_new(Path(str(prefix) + '-readback.json'), queries)
    require(not outer['forced'] and outer['returncode'] in (0, 1), 'observer transport failure')
    require(result['accounting_complete'] is True and result['observer_error'] is None, 'observer accounting incomplete')
    # A terminal failing/limited cohort still has a production result-writer
    # phase. Record authentic complete terminal accounting before refusing
    # qualification for a limit; never replace the original raw status.
    require(type(result['wrapper_exit_code']) is int, 'missing raw terminal status')
    receipt['terminal_authenticated'] = True
    require(result['stop_reason'] is None, 'service stopped by a bound')
    for key in ['cleanup_error', 'cleanup_identity_error', 'library_readback_error', 'reference_cleanup_error', 'refusal_final_accounting_error']:
        require(not result.get(key), 'observer cleanup/readback error: ' + key)
    require(result['final_report']['truncated'] == 'false', 'retained stderr was truncated')
    require(0 <= final['cpu_usage_nsec'] < phase['aggregate_cpu_usec'] * 1000, 'CPU allowance exhausted')
    require(0 <= result['elapsed_seconds'] <= phase['wall_seconds'] + 40, 'observer wall allowance exhausted')
    launch = result['launch']
    require(launch['cpu_allowance_usec'] == phase['aggregate_cpu_usec'] and launch['wall_seconds'] == phase['wall_seconds']
            and launch['log_bytes'] == phase['lethal_stderr_bytes'], 'actual bounds differ')
    return result['wrapper_exit_code']
