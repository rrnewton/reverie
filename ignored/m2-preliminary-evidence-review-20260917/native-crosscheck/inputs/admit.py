#!/usr/bin/python3
"""Validate the bound payload inside the already observed safehermit service."""
import argparse
import fcntl
import os
from pathlib import Path
import signal
import stat
import subprocess
import time
from common import check_file, digest, json_read, owned, read, require, signal_owned, write_new


def check_prerequisite(item):
    spelling = Path(item['spelling'])
    require(str(spelling.resolve(strict=True)) == item['resolved']['path'], 'guest prerequisite resolution changed')
    check_file(item['resolved'], executable=item['executable'])
    info = spelling.stat()
    require([info.st_dev, info.st_ino, info.st_uid] == item['identity'], 'guest prerequisite identity changed')


def embedded_path(test, program):
    check_file(test, executable=True); check_file(program, executable=True)
    needle=os.fsencode(program['path']); tail=b''; found=False
    with Path(test['path']).open('rb') as source:
        for block in iter(lambda:source.read(1024**2),b''):
            data=tail+block
            found=found or needle in data
            tail=data[-len(needle):]
    require(found, 'actual test ELF does not contain its bound CARGO_BIN_EXE_hermit path')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('plan'); parser.add_argument('sha256')
    args = parser.parse_args()
    require(digest(args.plan) == args.sha256, 'concrete phase plan changed')
    plan = json_read(args.plan)
    check_file(plan['context']); context = json_read(plan['context']['path'])
    out = owned(Path(plan['output']), directory=True)
    # This helper is reached only after the observer creates its exclusive output
    # and authenticates the actual service before allowing Python to exec it.
    cgroup = read('/proc/self/cgroup', 4096).decode()
    require(any(line.startswith('0::/') and '/safehermit-' in line and line.endswith('.service')
                for line in cgroup.splitlines()), 'payload is not in an observed safehermit service')
    phase = plan['phase']; argv = phase['argv']
    executable_records = {item['path']: item for item in context['executables']}
    require(argv[0] in executable_records, 'actual payload executable unbound')
    runtime = plan['phase_bindings']['runtime_executables']
    require(argv[0] in {item['path'] for item in runtime}, 'runtime payload identity missing')
    for item in runtime:
        require(item == executable_records.get(item['path']), 'runtime program not in full bound inputs')
        check_file(item, executable=True)
    for item in plan['phase_bindings'].get('inputs', []):
        check_file(item)
    # Environment values are the complete originally frozen map. safehermit adds
    # its own service bookkeeping; declared payload keys may not drift.
    actual_environment=dict(os.environ)
    for key, value in plan['environment'].items():
        require(actual_environment.get(key) == value, 'payload environment changed: ' + key)
    protected_prefixes=('CARGO_','RUST','HERMIT_','NEXTEST_','LD_','THIRD_PARTY_','PKG_CONFIG_')
    protected_names={'CC','CXX','CFLAGS','CPPFLAGS','CXXFLAGS','LDFLAGS'}
    unexpected={key:value for key,value in actual_environment.items() if key not in plan['environment'] and (key.startswith(protected_prefixes) or key in protected_names)}
    require(not unexpected, 'undeclared build/test/runtime environment override: '+','.join(sorted(unexpected)))
    for key in ('TMPDIR', 'XDG_STATE_HOME', 'HERMIT_NEXTEST_CPU_RECORD_DIR'):
        value = plan['environment'].get(key)
        if value and Path(value).is_relative_to(out):
            path = owned(Path(value), out)
            require(not path.exists(), 'retain prior output directory: ' + str(path))
            path.mkdir(mode=0o700)
    for pair in plan['phase_bindings'].get('test_embedded_hermit', []):
        embedded_path(pair['test'],pair['hermit'])
    prerequisites = context['guest_prerequisites'] if plan['admission'] else []
    for item in prerequisites:
        check_prerequisite(item)
    if plan['admission'] and phase['name'] in ('original-kvm-cli', 'original-pthread'):
        required = json_read(context['guest_prerequisites_template']['path'])
        spellings = {item['spelling'] for item in prerequisites}
        require(set(required['required_paths'] + required['host_input_files']).issubset(spellings), 'missing anti-skip prerequisite binding')
        require('/usr/bin/cc' in spellings, 'original fixture compiler unbound')
    device = None
    record = dict(argv=argv, cgroup=cgroup, supervisor_pid=os.getpid(), environment=plan['environment'],
                  actual_environment=actual_environment, phase_plan_sha256=args.sha256, executable_bindings=list(executable_records.values()),
                  prerequisites=prerequisites, kvm_required=plan['admission'])
    try:
        if plan['admission']:
            device = os.open('/dev/kvm', os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW)
            info = os.fstat(device)
            require(stat.S_ISCHR(info.st_mode), '/dev/kvm is not a character device')
            require(fcntl.ioctl(device, 0xAE00) == 12, 'real KVM API admission failed')
            record['kvm'] = dict(device=info.st_dev, inode=info.st_ino, rdev=info.st_rdev, api_version=12)
        write_new(out / 'admission.json', record)
        process = subprocess.Popen(argv, cwd=phase['cwd'], stdin=subprocess.DEVNULL)
        pidfd = os.pidfd_open(process.pid)
        started = time.monotonic()
        timed_out = False
        try:
            try:
                code = process.wait(timeout=phase['wall_seconds'])
            except subprocess.TimeoutExpired:
                timed_out = True
                signal_owned(process, pidfd, signal.SIGKILL)
                try:
                    code = process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    code = None
            write_new(out / 'payload-exit.json', dict(returncode=code, elapsed_seconds=time.monotonic()-started,
                      local_wait_timed_out=timed_out, reaped=code is not None))
        finally:
            os.close(pidfd)
        for item in runtime:
            check_file(item, executable=True)
        for item in prerequisites:
            check_prerequisite(item)
        if device is not None:
            current = os.stat('/dev/kvm', follow_symlinks=False)
            require((current.st_dev, current.st_ino, current.st_rdev) == (info.st_dev, info.st_ino, info.st_rdev), 'KVM device changed')
        require(not timed_out and code is not None, 'payload did not complete within its original allowance')
        # Preserve negative Python signal status in payload-exit.json. The service
        # exit convention is recorded separately; never turn a failure into 0.
        return code if code >= 0 else 128 - code
    finally:
        if device is not None:
            os.close(device)


if __name__ == '__main__':
    raise SystemExit(main())
