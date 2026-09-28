#!/usr/bin/python3
"""Observe one safehermit service's aggregate CPU; never supplies a parity verdict."""
import argparse
import ctypes
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import socket
import secrets

import before_exec
import subprocess
import time

from unit_reference import UnitReference

HERE = Path(__file__).resolve().parent
WRAPPER = Path('/home/newton/work/dev-hermit/bin/safehermit')
SYSTEMCTL = Path('/usr/bin/systemctl')
CGROUP_ROOT = Path('/sys/fs/cgroup')
INPUT_BINDING_SHA256 = 'af15f634235359032b1ea544207713c32318906d51447ff6de171ada09c8b5b7'
STARTUP_SECONDS = 25
FINAL_SECONDS = 5
POLL_SECONDS = .01
EVIDENCE_BYTES = 32 * 1024**3
MIN_FREE_BYTES = 100 * 1024**3
STDIO_BYTES = 64 * 1024**2


class Refusal(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise Refusal(reason)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def write_new(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())


def check_inputs():
    require(digest(HERE / 'source-inputs.json') == INPUT_BINDING_SHA256, 'supervision input binding changed')
    records = json.loads((HERE / 'source-inputs.json').read_text())['inputs']
    for entry in records:
        path = Path(entry['path'])
        require(path.stat().st_size == entry['bytes'] and digest(path) == entry['sha256'],
                'bound supervision source changed: ' + str(path))


class DlInfo(ctypes.Structure):
    _fields_ = [('filename', ctypes.c_char_p), ('base', ctypes.c_void_p),
                ('symbol', ctypes.c_char_p), ('address', ctypes.c_void_p)]


def loaded_systemd_library(reference):
    libc = ctypes.CDLL(None)
    libc.dladdr.argtypes = [ctypes.c_void_p, ctypes.POINTER(DlInfo)]
    libc.dladdr.restype = ctypes.c_int
    information = DlInfo()
    require(libc.dladdr(ctypes.cast(reference.lib.sd_bus_open_user, ctypes.c_void_p), ctypes.byref(information)) != 0,
            'could not resolve the loaded sd_bus_open_user library')
    require(information.filename is not None, 'loaded systemd library path missing')
    path = Path(os.fsdecode(information.filename))
    require(path.is_absolute(), 'loaded systemd library path is not absolute')
    path = path.resolve(strict=True)
    metadata = path.stat()
    actual = {'path': str(path), 'bytes': metadata.st_size, 'sha256': digest(path)}
    expected = json.loads((HERE / 'source-inputs.json').read_text())['loaded_systemd_library']
    require(actual == expected, 'actual loaded systemd library differs from frozen source input')
    return dict(actual, mapped_filename=os.fsdecode(information.filename), device=metadata.st_dev, inode=metadata.st_ino)


def proc_identity(pid):
    directory = Path('/proc') / str(pid)
    raw = (directory / 'stat').read_text()
    fields = raw[raw.rindex(')') + 2:].split()
    groups = (directory / 'cgroup').read_text().splitlines()
    require(len(groups) == 1 and groups[0].startswith('0::/'), 'process lacks unique cgroup-v2 membership')
    return {'pid': pid, 'start_ticks': int(fields[19]), 'parent_pid': int(fields[1]),
            'state': fields[0], 'cgroup': groups[0][3:]}


def report_fields(path):
    if not path.exists():
        return {}
    require(not path.is_symlink() and path.stat().st_size <= 1024 * 1024, 'invalid safehermit report file')
    raw = path.read_bytes()
    fields = {}
    # Ignore the incomplete final line while the wrapper is still writing.
    for line in raw.split(b'\n')[:-1]:
        match = re.fullmatch(rb'safehermit: ([a-z0-9_.]+)=(.*)', line)
        if match:
            key, value = (part.decode() for part in match.groups())
            require(key not in fields, 'duplicate safehermit report field: ' + key)
            fields[key] = value
    return fields


def authenticate_report(fields, launch):
    needed = {'run_id', 'binary', 'binary_source', 'binary_sha256', 'unit',
              'bound.wall', 'bound.cgroup', 'bound.bytes', 'log_dir', 'log_file', 'env.forwarded'}
    require(needed <= fields.keys(), 'safehermit startup report incomplete')
    match = re.fullmatch(r'([0-9]{8}T[0-9]{6}Z)-([1-9][0-9]*)', fields['run_id'])
    require(match is not None and int(match[2]) == launch['pid'], 'report does not name the launched wrapper PID')
    when = datetime.datetime.strptime(match[1], '%Y%m%dT%H%M%SZ').replace(tzinfo=datetime.timezone.utc).timestamp()
    require(int(launch['epoch']) <= when <= int(time.time()), 'stale or future safehermit run identity')
    require(fields['unit'] == 'safehermit-' + fields['run_id'], 'report unit does not match its own run identity')
    require(fields['binary'] == launch['binary'] and fields['binary_sha256'] == launch['binary_sha256']
            and fields['binary_source'] == 'argv:positional-path', 'reported executable differs from frozen launch')
    require(fields['bound.wall'] == 'APPLIED:' + str(launch['wall_seconds']) + 's', 'original independent wall bound missing')
    require(fields['bound.cgroup'] == 'APPLIED:MemoryMax=16G MemorySwapMax=0', 'safehermit service memory/cgroup bound missing')
    require(fields['bound.bytes'] == 'APPLIED:' + str(launch['log_bytes']) + ' (LETHAL: the run is cgroup-killed at the cap)',
            'original lethal stderr bound missing')
    require(fields['env.forwarded'].startswith('APPLIED:'), 'safehermit environment was not forwarded')
    for key in ['log_dir', 'log_file']:
        path = Path(fields[key])
        require(path.is_absolute() and path.resolve().is_relative_to(Path(launch['safehermit_log_root'])),
                'safehermit retained log escaped own destination')
    return fields['unit'] + '.service'



def typed_exec_start(reference, deadline):
    path = reference.method('GetUnit', deadline)
    remaining = min(.25, deadline - time.monotonic())
    require(remaining > 0, 'ExecStart receipt deadline expired')
    argv = ['/usr/bin/busctl', '--user', '--json=short', 'get-property',
            'org.freedesktop.systemd1', path, 'org.freedesktop.systemd1.Service', 'ExecStart']
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=remaining, check=False)
    require(result.returncode == 0, 'typed systemd ExecStart query failed: ' + result.stderr.decode(errors='replace'))
    value = json.loads(result.stdout)
    require(set(value) == {'type', 'data'} and value['type'] == 'a(sasbttttuii)', 'unexpected typed ExecStart schema')
    require(isinstance(value['data'], list) and len(value['data']) == 1, 'service does not have one exact ExecStart')
    require(isinstance(value['data'][0], list) and len(value['data'][0]) == 10, 'malformed ExecStart record')
    return {'value': value, 'query_argv': argv, 'stdout_sha256': hashlib.sha256(result.stdout).hexdigest(),
            'raw_stdout': result.stdout.decode(), 'returncode': result.returncode}


def authenticate_exec_start(receipt, launch, pid):
    row = receipt['value']['data'][0]
    require(row[0] == launch['binary'] and row[1] == [launch['binary'], *launch['arguments']], 'typed service executable or argv differs')
    require(row[2] is False, 'service command ignores its actual exit status')
    require(type(row[7]) is int and row[7] == pid, 'ExecStart process identity differs from MainPID')
    require(type(row[4]) is int and row[4] * 1000 >= launch['monotonic_ns'], 'stale ExecStart process timestamp')


def unit_properties(reference, deadline, final=False):
    path = reference.method('GetUnit', deadline)
    result = {}
    for name in ['Id', 'LoadState', 'ActiveState', 'SubState']:
        result[name] = reference.property(path, 'org.freedesktop.systemd1.Unit', name, deadline)
    names = ['ControlGroup', 'MainPID', 'ExecMainPID', 'ExecMainCode', 'ExecMainStatus',
             'ExecMainStartTimestampMonotonic', 'CPUUsageNSec']
    if not final:
        names += ['RuntimeMaxUSec', 'MemoryMax', 'MemorySwapMax', 'IgnoreSIGPIPE']
    for name in names:
        result[name] = reference.property(path, 'org.freedesktop.systemd1.Service', name, deadline)
    if not final:
        result['ExecStart'] = typed_exec_start(reference, deadline)
    require(time.monotonic() <= deadline, 'unit property receipt deadline expired')
    return result


def authenticate_service(values, unit, launch):
    require(values['Id'] == unit and values['LoadState'] == 'loaded', 'unit identity or load state differs')
    require(values['ActiveState'] == 'active' and values['SubState'] == 'running', 'service was not observed running')
    require(values['RuntimeMaxUSec'] == launch['wall_seconds'] * 1000000, 'service wall limit differs from original allowance')
    require(values['MemoryMax'] == 16 * 1024**3 and values['MemorySwapMax'] == 0, 'service memory limit differs from wrapper report')
    require(values['IgnoreSIGPIPE'] == 1, 'service SIGPIPE disposition differs from helper exec contract')
    group = values['ControlGroup']
    require(isinstance(group, str) and group.startswith('/') and group != '/' and '..' not in group.split('/'),
            'invalid unit ControlGroup')
    require(Path(group).name == unit, 'ControlGroup does not name this service')
    path = CGROUP_ROOT / group.lstrip('/')
    require(path.resolve() == path and path.is_dir(), 'ControlGroup path absent or redirected')
    pid = values['MainPID']
    require(type(pid) is int and pid > 0 and pid == values['ExecMainPID'], 'service main process is missing or contradictory')
    identity = proc_identity(pid)
    require(identity['cgroup'] == group, 'main process membership contradicts ControlGroup')
    require(identity['start_ticks'] >= launch['wrapper_identity']['start_ticks'], 'stale service process generation')
    require(values['ExecMainStartTimestampMonotonic'] * 1000 >= launch['monotonic_ns'], 'stale service start timestamp')
    authenticate_exec_start(values['ExecStart'], launch, pid)
    wrapper_now = proc_identity(launch['pid'])
    require(wrapper_now['start_ticks'] == launch['wrapper_identity']['start_ticks'], 'wrapper process generation changed')
    require(wrapper_now['cgroup'] != group, 'outer wrapper cgroup was mistaken for the service')
    return path, identity


def read_usage(group, previous=None):
    values = {}
    for line in (group / 'cpu.stat').read_text().splitlines():
        key, value = line.split()
        require(key not in values and value.isdecimal(), 'malformed or duplicate cgroup CPU accounting')
        values[key] = int(value)
    require('usage_usec' in values, 'aggregate service usage missing')
    usage = values['usage_usec']
    require(previous is None or usage >= previous, 'aggregate service CPU counter regressed')
    members = []
    for word in (group / 'cgroup.procs').read_text().split():
        require(word.isdecimal() and int(word) > 0, 'invalid cgroup member PID')
        try:
            identity = proc_identity(int(word))
        except (FileNotFoundError, ProcessLookupError):
            # A member can exit after enumeration or after opening its proc file.
            # ENOENT and ESRCH both mean this diagnostic identity disappeared;
            # the cgroup CPU counter above still includes the exited member.
            continue
        require(identity['cgroup'] == '/' + str(group.relative_to(CGROUP_ROOT)), 'member process contradicts service cgroup')
        members.append(identity)
    return {'monotonic_ns': time.monotonic_ns(), 'usage_usec': usage, 'members': members}


def final_accounting(values, initial, unit, group, maximum_usage):
    require(values['Id'] == unit and values['LoadState'] == 'loaded', 'final unit identity missing or contradictory')
    require(values['ActiveState'] in ['inactive', 'failed'] and values['SubState'] in ['dead', 'failed'], 'service is not terminal')
    require(values['MainPID'] == 0, 'final service still has a main process')
    for name in ['ExecMainPID', 'ExecMainStartTimestampMonotonic']:
        require(values[name] == initial[name], 'final service process identity changed: ' + name)
    require(values['ControlGroup'] in ['', initial['ControlGroup']], 'final ControlGroup changed')
    if group.exists():
        events = dict(line.split() for line in (group / 'cgroup.events').read_text().splitlines())
        require(events.get('populated') == '0', 'final service cgroup still populated')
    usage = values['CPUUsageNSec']
    require(type(usage) is int and 0 <= usage < 2**64 - 1, 'final CPU accounting unavailable')
    require(usage >= maximum_usage * 1000, 'final CPU accounting regressed below an observed sample')
    require(values['ExecMainCode'] in [1, 2, 3], 'final process termination code unavailable')
    require(type(values['ExecMainStatus']) is int and 0 <= values['ExecMainStatus'] <= 255, 'final process status unavailable')
    return {'source': 'systemd CPUUsageNSec after service exit', 'cpu_usage_nsec': usage,
            'cgroup_empty': True, 'exec_main_code': values['ExecMainCode'], 'exec_main_status': values['ExecMainStatus'],
            'properties': values}


def validate_terminal_report(fields, wrapper_exit, final, stop_reason):
    require(fields.get('exit_code') == str(wrapper_exit), 'captured wrapper status differs from report')
    require(fields.get('truncated') in ['true', 'false'], 'final stderr accounting missing')
    require(fields.get('bytes_written', '').isdecimal(), 'final stderr byte accounting missing')
    require(fields.get('unit_result') in ['success', 'exit-code', 'signal', 'core-dump', 'timeout', 'oom-kill'],
            'authoritative service result absent')
    code, status = final['exec_main_code'], final['exec_main_status']
    if fields['unit_result'] == 'success':
        require((code, status, wrapper_exit) == (1, 0, 0), 'success report contradicts real process status')
    elif fields['unit_result'] == 'exit-code':
        require(code == 1 and status != 0 and wrapper_exit == status, 'exit report contradicts real process status')
    elif fields['unit_result'] in ['signal', 'core-dump']:
        expected_code = 2 if fields['unit_result'] == 'signal' else 3
        require(code == expected_code and 1 <= status <= 64, 'signal report contradicts real process status')
        require(fields.get('exec_main_pid') == str(final['properties']['ExecMainPID'])
                and fields.get('exec_main_code') == str(code) and fields.get('exec_main_status') == str(status),
                'signal status receipt disagrees with final unit')
        require(wrapper_exit == (125 if fields['truncated'] == 'true' else 128 + status), 'wrapper signal status differs')
    elif fields['unit_result'] == 'timeout':
        require(wrapper_exit == 124, 'wall timeout report contradicts wrapper status')
    if stop_reason == 'aggregate_cpu_limit':
        require(wrapper_exit != 0, 'CPU termination was hidden as success')


def evidence_guard(out):
    size = sum(p.lstat().st_size for p in out.rglob('*') if p.is_file() and not p.is_symlink())
    free = os.statvfs(out)
    require(size < EVIDENCE_BYTES, 'evidence byte guard reached')
    require(free.f_bavail * free.f_frsize >= MIN_FREE_BYTES, 'free-space guard reached')
    for name in ['stdout', 'stderr', 'samples.jsonl']:
        path = out / name
        require(not path.exists() or path.stat().st_size < STDIO_BYTES, 'independent output guard reached: ' + name)


def kill_owned_unit(unit, commands):
    argv = [str(SYSTEMCTL), '--user', 'kill', '--kill-whom=all', '--signal=KILL', unit]
    started = time.monotonic_ns()
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=2, check=False)
    commands.append({'argv': argv, 'returncode': result.returncode, 'stdout': result.stdout.decode(errors='replace'),
                     'stderr': result.stderr.decode(errors='replace'), 'started_monotonic_ns': started})
    require(result.returncode == 0, 'owned service kill failed')


def acquire_before_exec(listener, spec, launch, identity, deadline):
    require(time.monotonic() < deadline, 'before-exec acquisition deadline expired')
    listener.settimeout(deadline - time.monotonic())
    connection, _address = listener.accept()
    try:
        peer = before_exec.credentials(connection)
        require(peer == (identity['pid'], os.getuid(), os.getgid()), 'before-exec peer is not the exact service MainPID')
        request = before_exec.receive(connection, deadline)
        expected = {'phase': 'ready', 'spec_sha256': launch['before_exec_spec_sha256'],
                    'token': spec['token'], 'pid': identity['pid'],
                    'start_ticks': identity['start_ticks'], 'payload': launch['payload']}
        require(request == expected, 'before-exec request identity or payload differs')
        return connection, request
    except BaseException:
        connection.close()
        raise


def observe(out, binary, arguments, cpu_usec, wall_seconds, log_bytes=67108864):
    out = Path(out).resolve()
    binary = Path(binary).absolute()
    require(type(cpu_usec) is int and cpu_usec > 0 and type(wall_seconds) is int and wall_seconds > 0, 'invalid original CPU/wall allowance')
    require(type(log_bytes) is int and log_bytes > 0, 'invalid original stderr bound')
    require(out.is_relative_to(HERE) and not out.exists(), 'observer evidence must be new and under its owned directory')
    require(binary.is_file() and os.access(binary, os.X_OK), 'selected binary is not executable')
    check_inputs()
    out.mkdir(mode=0o700, parents=True)
    report = out / 'safehermit.report'
    payload = {'binary': str(binary), 'binary_sha256': digest(binary), 'arguments': list(arguments)}
    helper = Path('/usr/bin/python3')
    spec_path = out / 'before-exec.json'
    spec = {'payload': payload, 'token': secrets.token_hex(32), 'observer_pid': os.getpid(),
            'locale_environment': {key: os.environ.get(key) for key in ['LC_CTYPE', 'LC_ALL', 'LANG']},
            'ignore_sigpipe': True,
            'observer_start_ticks': proc_identity(os.getpid())['start_ticks'],
            'deadline_monotonic_ns': time.monotonic_ns() + int(STARTUP_SECONDS * 1e9)}
    write_new(spec_path, spec)
    helper_arguments = ['-B', str(HERE / 'before_exec.py'), str(spec_path), digest(spec_path)]
    launch = {'binary': str(helper), 'binary_sha256': digest(helper), 'arguments': helper_arguments,
              'payload': payload, 'before_exec_spec_sha256': digest(spec_path),
              'cpu_allowance_usec': cpu_usec, 'wall_seconds': wall_seconds, 'log_bytes': log_bytes,
              'safehermit_log_root': str(out / 'safehermit'), 'wrapper_sha256': digest(WRAPPER),
              'observer_sources': {name: digest(HERE / name) for name in ['observer.py', 'unit_reference.py', 'before_exec.py', 'source-inputs.json']},
              'cwd': os.getcwd(), 'epoch': time.time(), 'monotonic_ns': time.monotonic_ns()}
    argv = [str(WRAPPER), '--sh-deadline', str(wall_seconds), '--sh-max-log-bytes', str(log_bytes),
            '--sh-report', str(report), str(helper), *helper_arguments]
    environment = dict(os.environ, SAFEHERMIT_LOG_ROOT=launch['safehermit_log_root'])
    result = {'schema': 1, 'scope': 'Aggregate CPU observation of the actual safehermit service only',
              'launch': launch, 'wrapper_argv': argv, 'wrapper_exit_code': None, 'stop_reason': None,
              'observer_error': None, 'final_accounting': None, 'kill_commands': [], 'accounting_complete': False, 'comparison_eligible': False}
    process = reference = group = initial = listener = connection = directory_fd = None
    unit = None
    maximum_usage = 0
    sample_count = 0
    deadline = time.monotonic() + STARTUP_SECONDS
    try:
        evidence_guard(out)
        directory_fd = os.open(out, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        original_cwd_fd = os.open('.', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            os.fchdir(directory_fd)
            listener.bind('before-exec.sock')
        finally:
            os.fchdir(original_cwd_fd)
            os.close(original_cwd_fd)
        listener.listen(1)
        with (out / 'stdout').open('xb') as stdout, (out / 'stderr').open('xb') as stderr, (out / 'samples.jsonl').open('x') as samples:
            reference = UnitReference('libsystemd.so.0', None)
            result['loaded_systemd_library_before'] = loaded_systemd_library(reference)
            process = subprocess.Popen(argv, env=environment, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, start_new_session=True)
            launch['pid'] = process.pid
            launch['wrapper_identity'] = proc_identity(process.pid)
            write_new(out / 'launch.json', launch)
            while time.monotonic() < deadline:
                fields = report_fields(report)
                if 'env.forwarded' in fields:
                    unit = authenticate_report(fields, launch)
                    reference.bind(unit)
                    break
                require(process.poll() is None, 'wrapper failed before an authenticated startup report')
                evidence_guard(out)
                time.sleep(POLL_SECONDS)
            require(unit is not None, 'safehermit startup report deadline expired')
            while time.monotonic() < deadline:
                try:
                    reference.method('RefUnit', deadline)
                    reference.held = True
                    break
                except FileNotFoundError:
                    require(process.poll() is None, 'service ended before reference acquisition')
                    time.sleep(POLL_SECONDS)
            require(reference.held, 'service reference acquisition deadline expired')
            initial = unit_properties(reference, deadline)
            group, identity = authenticate_service(initial, unit, launch)
            result['authenticated_service'] = {'unit': unit, 'initial_properties': initial, 'main_process': identity,
                                               'cgroup_inode': group.stat().st_ino, 'cgroup_device': group.stat().st_dev}
            service_wall_deadline = initial['ExecMainStartTimestampMonotonic'] / 1e6 + wall_seconds
            release_deadline = min(deadline, service_wall_deadline)
            connection, request = acquire_before_exec(listener, spec, launch, identity, release_deadline)
            # This first real service sample is mandatory before authorizing the
            # payload. It includes every CPU instruction used by its helper.
            sample = read_usage(group)
            maximum_usage = sample['usage_usec']
            sample_count = 1
            previous_stamp = sample['monotonic_ns']
            samples.write(json.dumps(sample, sort_keys=True) + '\n'); samples.flush()
            if maximum_usage >= cpu_usec:
                result['stop_reason'] = 'aggregate_cpu_limit_before_payload'
                kill_owned_unit(unit, result['kill_commands'])
                raise Refusal('original aggregate CPU allowance exhausted before payload release')
            require(time.monotonic() < release_deadline, 'original wall/startup allowance exhausted before payload release')
            current_identity = proc_identity(identity['pid'])
            require(all(current_identity[key] == identity[key] for key in ['pid', 'start_ticks', 'parent_pid', 'cgroup']), 'waiting helper process identity changed')
            require(digest(binary) == payload['binary_sha256'], 'payload changed before authorization')
            require(digest(spec_path) == launch['before_exec_spec_sha256'], 'before-exec specification changed')
            require(launch['observer_sources'] == {name: digest(HERE / name) for name in launch['observer_sources']}, 'observer source changed before authorization')
            check_inputs()
            evidence_guard(out)
            response = dict(request, phase='exec')
            before_exec.send(connection, response, release_deadline)
            result['payload_authorization'] = {'request': request, 'response': response,
                                               'monotonic_ns': time.monotonic_ns(), 'first_cpu_sample': sample,
                                               'meaning': 'Authorization sent; payload start requires actual execution evidence.'}
            connection.close(); connection = None
            listener.close(); listener = None
            os.close(directory_fd); directory_fd = None
            # The cumulative counter includes startup before the first sample.
            deadline = launch['monotonic_ns'] / 1e9 + STARTUP_SECONDS + wall_seconds + FINAL_SECONDS
            while process.poll() is None:
                require(time.monotonic() < deadline, 'wrapper/service completion deadline expired')
                evidence_guard(out)
                if time.monotonic() >= service_wall_deadline:
                    result['stop_reason'] = 'independent_wall_limit'
                    kill_owned_unit(unit, result['kill_commands'])
                    break
                try:
                    require((group.stat().st_dev, group.stat().st_ino) == (result['authenticated_service']['cgroup_device'], result['authenticated_service']['cgroup_inode']), 'authenticated cgroup was replaced')
                    sample = read_usage(group, maximum_usage)
                    require(sample['monotonic_ns'] > previous_stamp, 'CPU observation timestamps regressed')
                    previous_stamp = sample['monotonic_ns']
                    maximum_usage = sample['usage_usec']
                    sample_count += 1
                    samples.write(json.dumps(sample, sort_keys=True) + '\n'); samples.flush()
                    if maximum_usage >= cpu_usec:
                        result['stop_reason'] = 'aggregate_cpu_limit'
                        result['cpu_limit_observation'] = sample
                        kill_owned_unit(unit, result['kill_commands'])
                        break
                except FileNotFoundError:
                    require(not group.exists(), 'CPU accounting disappeared while service cgroup remains')
                    # A held systemd reference supplies final accounting after removal.
                time.sleep(POLL_SECONDS)
            result['wrapper_exit_code'] = process.wait(timeout=FINAL_SECONDS)
            result['final_report'] = report_fields(report)
            require(authenticate_report(result['final_report'], launch) == unit, 'final wrapper report identity changed')
            final_values = unit_properties(reference, time.monotonic() + FINAL_SECONDS, final=True)
            result['final_accounting'] = final_accounting(final_values, initial, unit, group, maximum_usage)
            require(sample_count > 0, 'no live service CPU sample before completion')
            if result['final_accounting']['cpu_usage_nsec'] >= cpu_usec * 1000 and result['stop_reason'] is None:
                result['stop_reason'] = 'aggregate_cpu_limit_observed_at_completion'
            validate_terminal_report(result['final_report'], result['wrapper_exit_code'], result['final_accounting'], result['stop_reason'])
            require(digest(binary) == payload['binary_sha256'], 'selected payload changed during observation')
            require(digest(helper) == launch['binary_sha256'], 'selected helper interpreter changed during observation')
            check_inputs()
            require(launch['observer_sources'] == {name: digest(HERE / name) for name in launch['observer_sources']}, 'observer source changed during capture')
            require(int(result['final_report']['bytes_written']) <= log_bytes, 'stderr byte count exceeds applied limit')
            meta_path = Path(result['final_report']['log_file'] + '.meta')
            meta = dict(line.split('=', 1) for line in meta_path.read_text().splitlines())
            require(meta.get('cap') == str(log_bytes) and meta.get('truncated') == result['final_report']['truncated']
                    and meta.get('bytes_written') == result['final_report']['bytes_written'], 'retained stderr receipt disagrees with report')
            result['stderr_receipt_sha256'] = digest(meta_path)
            result['loaded_systemd_library_after'] = loaded_systemd_library(reference)
            require(result['loaded_systemd_library_after'] == result['loaded_systemd_library_before'], 'loaded systemd library identity changed during observation')
            result['accounting_complete'] = True
            result['comparison_eligible'] = (result['wrapper_exit_code'] == 0 and result['stop_reason'] is None
                                             and result['final_report']['truncated'] == 'false'
                                             and result['final_report']['unit_result'] == 'success')
    except BaseException as error:
        result['observer_error'] = type(error).__name__ + ': ' + str(error)
    finally:
        # Closing the private protocol refuses any waiting helper even when the
        # wrapper reported no unit. No authorization was sent on those paths.
        for channel in [connection, listener]:
            if channel is not None:
                channel.close()
        if directory_fd is not None:
            os.close(directory_fd)
        if process is not None and process.poll() is None:
            if unit is None:
                # Stop only our fresh wrapper session before it can submit a late
                # service. The bound wrapper emits its complete report before
                # systemd-run; a submitted service therefore already has a report.
                try:
                    os.killpg(process.pid, signal.SIGSTOP)
                    fields = report_fields(report)
                    if 'env.forwarded' in fields:
                        unit = authenticate_report(fields, launch)
                except Exception as error:
                    result['cleanup_identity_error'] = repr(error)
                finally:
                    os.killpg(process.pid, signal.SIGKILL)
                    result['wrapper_forced_cleanup'] = True
            if unit is not None:
                try:
                    kill_owned_unit(unit, result['kill_commands'])
                except Exception as error:
                    result['cleanup_error'] = repr(error)
            try:
                result['wrapper_exit_code'] = process.wait(timeout=FINAL_SECONDS)
            except subprocess.TimeoutExpired:
                # The direct wrapper's fresh session is ours; never target another shell/service.
                os.killpg(process.pid, signal.SIGKILL)
                result['wrapper_exit_code'] = process.wait(timeout=2)
                result['wrapper_forced_cleanup'] = True
        if process is not None and result['wrapper_exit_code'] is None:
            result['wrapper_exit_code'] = process.poll()
        if reference is not None and reference.held and initial is not None and group is not None and result['final_accounting'] is None:
            try:
                values = unit_properties(reference, time.monotonic() + FINAL_SECONDS, final=True)
                result['refusal_final_accounting'] = final_accounting(values, initial, unit, group, maximum_usage)
            except Exception as error:
                result['refusal_final_accounting_error'] = repr(error)
        if reference is not None:
            try:
                result['loaded_systemd_library_final_readback'] = loaded_systemd_library(reference)
                require(result['loaded_systemd_library_final_readback'] == result.get('loaded_systemd_library_before'), 'loaded systemd library changed at cleanup')
            except Exception as error:
                result['library_readback_error'] = repr(error)
                result['comparison_eligible'] = False
            try:
                reference.close()
            except Exception as error:
                result['reference_cleanup_error'] = repr(error)
                result['comparison_eligible'] = False
            result['systemd_calls'] = reference.calls
        result['sample_count'] = sample_count
        result['maximum_sampled_usage_usec'] = maximum_usage if sample_count else None
        result['completed_monotonic_ns'] = time.monotonic_ns()
        result['elapsed_seconds'] = (result['completed_monotonic_ns'] - launch['monotonic_ns']) / 1e9
        if report.exists():
            result['safehermit_report_sha256'] = digest(report)
        write_new(out / 'result.json', result)
    return result


def harden_observer():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_AS, (512 * 1024**2,) * 2)
    require(ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0, 'could not disable observer dumpability')
    os.umask(0o077)


def main():
    harden_observer()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True)
    parser.add_argument('--cpu-usec', type=int, required=True)
    parser.add_argument('--wall-seconds', type=int, required=True)
    parser.add_argument('--log-bytes', type=int, default=67108864)
    parser.add_argument('binary')
    parser.add_argument('arguments', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    observer_bound = STARTUP_SECONDS + args.wall_seconds + 2 * FINAL_SECONDS + 5
    require(observer_bound > 0, 'invalid observer bound')
    resource.setrlimit(resource.RLIMIT_CPU, (observer_bound, observer_bound))
    def expired(_number, _frame):
        raise Refusal('independent observer wall bound expired')
    signal.signal(signal.SIGALRM, expired)
    signal.alarm(observer_bound)
    result = observe(args.out, args.binary, args.arguments, args.cpu_usec, args.wall_seconds, args.log_bytes)
    print(json.dumps({key: result[key] for key in ['wrapper_exit_code', 'stop_reason', 'observer_error', 'accounting_complete', 'comparison_eligible', 'elapsed_seconds']}))
    return 0 if result['comparison_eligible'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
