#!/usr/bin/python3
"""Observe one compiler-discovery/compile/native case. No action on import."""
import hashlib
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import time
import cache_lease
from common import bounded_process, check_file, file_record, json_read, read, require, terminal, write_new

HERE = Path(__file__).resolve().parent
OWNER = HERE.parent


def dependencies(raw):
    text = raw.decode('utf-8')
    require(text.startswith('probe:'), 'dependency target differs')
    names = shlex.split(text[len('probe:'):].replace('\\\n', ' '))
    require(names and all(Path(x).is_absolute() for x in names), 'nonabsolute compiler dependency')
    require(len(names) == len(set(names)), 'duplicate compiler dependency')
    source = OWNER / 'source/probe.c'
    require(str(source) in names, 'dependency list omits actual probe source')
    allowed = (Path('/usr/include'), Path('/usr/lib/gcc/x86_64-redhat-linux/11/include'),
               Path('/usr/lib/gcc/x86_64-redhat-linux/11/include-fixed'))
    for name in names:
        p = Path(name)
        require(p == source or any(p.is_relative_to(root) for root in allowed),
                'dependency outside actual declared source/system include roots: ' + name)
    return sorted(names)


def check(plan, before=False):
    require(plan['owner'] == str(OWNER) and plan['cwd'] == str(OWNER), 'wrong private owner/cwd')
    for r in plan['inputs']:
        check_file(r)
    for r in plan['input_symlinks']:
        p = Path(r['path'])
        require(p.is_symlink() and os.readlink(p) == r['target']
                and stat.S_IMODE(p.lstat().st_mode) == r['mode'], 'input symlink changed')
    for name in plan['absent_inputs']:
        p = Path(name)
        require(not p.exists() and not p.is_symlink(), 'unbound compiler specs appeared')
    target = Path(plan['target']['path'])
    require(target == HERE / 'target' and not target.is_symlink(), 'wrong private target')
    st = target.stat()
    require(stat.S_ISDIR(st.st_mode) and stat.S_IMODE(st.st_mode) == 0o700
            and [st.st_dev, st.st_ino, st.st_uid] == plan['target']['identity'], 'private target identity changed')
    if before:
        for name in plan['must_be_absent_before']:
            p = Path(name)
            require(not p.exists() and not p.is_symlink(), 'compile would overwrite an earlier output')


def inspect(plan, raw, stderr, code):
    if plan['kind'] == 'dependencies':
        require(code == 0 and not stderr, 'dependency query failed or emitted diagnostics')
        return dict(dependencies=dependencies(raw))
    if plan['kind'] == 'compile':
        require(code == 0 and not stderr, 'native compiler failed or emitted diagnostics')
        actual = dependencies(read(plan['dependency_output']))
        require(actual == plan['expected_dependencies'], 'actual compile header membership changed')
        # Linker -t names every selected input, including generated temporary .o.
        lines = raw.decode('utf-8').splitlines()
        require(lines, 'linker input trace missing')
        known = set(plan['allowed_link_inputs'])
        generated = []
        for line in lines:
            # GNU ld may suffix an archive input with '(member)'. The archive is bound.
            spelling = line.split('(', 1)[0]
            p = Path(spelling)
            require(p.is_absolute(), 'unexpected compiler/linker stdout: ' + line)
            if p.is_relative_to(HERE / 'tmp') and p.suffix == '.o':
                generated.append(line)
            else:
                require(str(p.resolve()) in known, 'unbound linker input: ' + line)
        require(generated, 'linker trace omits generated probe object')
        binary = Path(plan['binary_output'])
        record = file_record(binary)
        require(os.access(binary, os.X_OK), 'compiled probe is not executable')
        header = read(binary, 16 * 1024**2)[:20]
        require(header[:6] == b'\x7fELF\x02\x01' and int.from_bytes(header[18:20], 'little') == 62,
                'compiled probe is not x86-64 ELF')
        return dict(binary=record, actual_dependencies=actual, linker_inputs=lines,
                    generated_objects=generated, compiler_diagnostics=0)
    require(plan['kind'] == 'probe', 'unknown native phase')
    mode, instruction, prefix = plan['probe_args']
    if code == 77:
        require(instruction == 'rdtscp' and raw == b''
                and stderr == b'UNEXECUTED: native hardware does not advertise RDTSCP\n',
                'exit 77 does not have exact unavailable-RDTSCP evidence')
        return dict(outcome='unexecuted', native_success=False, availability='RDTSCP unavailable')
    require(code == 0 and not stderr, 'native probe failed or emitted stderr')
    lines = raw.decode('ascii').splitlines()
    require(len(lines) == 3 and lines[2] == 'PASS: native architectural properties', 'native completion marker/line count differs')
    pattern = (r'mode=' + re.escape(mode) + r' instruction=' + re.escape(instruction)
               + r' prefix=' + re.escape(prefix) + r' signal=(-?\d+) code=(-?\d+) trap=(-?\d+) '
               r'rip_delta=(-?\d+) fault_delta=(-?\d+) rf_stage=(-?\d+)')
    match = re.fullmatch(pattern, lines[0])
    require(match is not None, 'native result does not identify exact selected case')
    require(re.fullmatch(r'registers rax=[0-9a-f]{16} rdx=[0-9a-f]{16} rcx=[0-9a-f]{16} '
                         r'flags=[0-9a-f]{16} rsp_before=[0-9a-f]{16} rsp_after=[0-9a-f]{16}', lines[1]),
            'native architectural register diagnostics missing')
    return dict(outcome='passed', native_success=True, selector=plan['name'],
                diagnostic_lines=lines[:2], predicate_authority='unchanged exact probe source predicates plus raw exit zero')


def payload(plan, sha):
    fd = cache_lease.join(plan['lease'], sha)
    try:
        check(plan, before=True)
        for key, value in plan['environment'].items():
            require(os.environ.get(key) == value, 'payload environment differs: ' + key)
        dangerous = ('LD_', 'GCC_', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'OBJC_INCLUDE_PATH',
                     'COMPILER_PATH', 'LIBRARY_PATH', 'CPATH', 'CFLAGS', 'CPPFLAGS', 'LDFLAGS')
        require(not [k for k in os.environ if k not in plan['environment'] and k.startswith(dangerous)],
                'undeclared compiler/loader override')
        group = read('/proc/self/cgroup', 4096).decode()
        require('/safehermit-' in group and group.strip().endswith('.service'), 'payload outside observed service')
        out = Path(plan['output'])
        write_new(out / 'payload.json', dict(argv=plan['argv'], cwd=plan['cwd'], environment=dict(os.environ),
                                             cgroup=group, pid=os.getpid(), plan_sha256=sha))
        started = time.monotonic()
        child = subprocess.Popen(plan['argv'], cwd=plan['cwd'], stdin=subprocess.DEVNULL)
        code = child.wait()  # Unchanged observer owns aggregate CPU/memory/descendants.
        write_new(out / 'payload-exit.json', dict(returncode=code, reaped=True,
                                                  elapsed_seconds=time.monotonic() - started))
        check(plan)
        cache_lease.check_held(fd, plan['lease'], sha)
        return code if code >= 0 else 128 - code
    finally:
        os.close(fd)


def launch(plan, sha):
    fd = cache_lease.claim(plan['lease'], sha)
    control = Path(plan['control'])
    control.mkdir(mode=0o700)
    result = dict(plan_sha256=sha, accepted=False, observer_attempted=False,
                  terminal_authenticated=False, raw_status=None, outcome='failed')
    try:
        check(plan, before=True)
        limits = plan['limits']
        argv = ['/usr/bin/python3', '-B', str(HERE / 'observer/observer.py'), '--out', plan['output'],
                '--cpu-usec', str(limits['aggregate_cpu_usec']), '--wall-seconds', str(limits['wall_seconds']),
                '--log-bytes', str(limits['lethal_stderr_bytes']), '/usr/bin/python3', '-B',
                str(HERE / 'native_phase.py'), 'payload', plan['path'], sha]
        write_new(control / 'dispatch.json', dict(argv=argv, environment=plan['environment'], cwd=plan['cwd']))
        result['observer_attempted'] = True
        outer = bounded_process(argv, OWNER, plan['environment'], control / 'observer',
                                limits['wall_seconds'] + 50, 1024**2, observer=True)
        result['transport'] = outer
        observed = json_read(Path(plan['output']) / 'result.json')
        result['observer_result'] = file_record(Path(plan['output']) / 'result.json')
        result['raw_status'] = observed.get('wrapper_exit_code')
        stdout = read(Path(plan['output']) / 'stdout', 16 * 1024**2)
        stderr = read(Path(plan['output']) / 'stderr', 16 * 1024**2)
        result['stdout'] = file_record(Path(plan['output']) / 'stdout')
        result['stderr'] = file_record(Path(plan['output']) / 'stderr')
        try:
            result['readback'] = inspect(plan, stdout, stderr, result['raw_status'])
        except Exception as error:
            result['readback_error'] = repr(error)
        code = terminal(observed, limits, outer, control / 'service-post', plan['environment'], result)
        receipt = json_read(Path(plan['output']) / 'payload-exit.json')
        result['payload_exit'] = receipt
        require(receipt['reaped'] and code == (receipt['returncode'] if receipt['returncode'] >= 0 else 128 - receipt['returncode']),
                'payload status differs')
        require('readback_error' not in result, 'native readback failed')
        if code == 77:
            require(plan['kind'] == 'probe' and result['readback']['outcome'] == 'unexecuted', 'unqualified unavailable outcome')
            result['outcome'] = 'unexecuted'  # accepted deliberately remains false.
        else:
            require(code == 0 and outer['returncode'] == 0, 'actual payload failed')
            result.update(accepted=True, outcome='passed')
    except BaseException as error:
        result['error'] = repr(error)
    finally:
        try:
            check(plan)
            cache_lease.check_held(fd, plan['lease'], sha)
            result['inputs_unchanged'] = True
        except BaseException as error:
            result.update(inputs_unchanged=False, input_error=repr(error), accepted=False, outcome='failed')
        write_new(control / 'result.json', result)
        cache_lease.complete(fd, plan['lease'], sha, control / 'result.json')
        os.close(fd)
    print(__import__('json').dumps(result), flush=True)
    return 0 if result['accepted'] else 77 if result['outcome'] == 'unexecuted' else 1


if __name__ == '__main__':
    require(len(sys.argv) == 4 and sys.argv[1] in ('payload', 'launch'), 'expected launch|payload PLAN SHA')
    mode, path, sha = sys.argv[1:]
    require(hashlib.sha256(read(path)).hexdigest() == sha, 'plan changed')
    plan = json_read(path)
    require(plan['path'] == path, 'plan path differs')
    raise SystemExit(payload(plan, sha) if mode == 'payload' else launch(plan, sha))
