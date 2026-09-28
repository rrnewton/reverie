"""One explicitly admitted bounded phase. Does not execute on import."""

import argparse
import json
import os
import re
import selectors
import subprocess
import sys
import time

from support import OUT, ROOT, STREAM_CAP, TARGET, TOOLCHAIN
from support import identity, load_bound, load_source, local_path
from support import selected_names, source_identity, validate_plan, write_json


def service_show(unit):
    argv = ['/usr/bin/systemctl', '--user', 'show', unit,
            '--property=LoadState,ActiveState,SubState,ControlGroup,MainPID,Result,ExecMainCode,ExecMainStatus,CPUUsageNSec']
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=10)
        fields = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
        return {'argv': argv, 'raw_status': result.returncode,
                'stdout': result.stdout, 'stderr': result.stderr, 'fields': fields}
    except subprocess.TimeoutExpired:
        return {'argv': argv, 'refusal': 'systemctl show timeout', 'fields': {}}


def stop_service(unit, controls):
    argv = ['/usr/bin/systemctl', '--user', 'stop', unit]
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=12)
        controls.append({'argv': argv, 'raw_status': result.returncode,
                         'stdout': result.stdout, 'stderr': result.stderr})
    except subprocess.TimeoutExpired:
        controls.append({'argv': argv, 'refusal': 'systemctl stop timeout'})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('phase')
    parser.add_argument('--plan', required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--source-sha256', required=True)
    args = parser.parse_args()
    plan, plan_binding = load_bound(args.plan, args.plan_sha256)
    validate_plan(plan)
    if args.phase not in plan['phases']:
        raise ValueError('phase not in exact plan')
    source, source_binding = load_source(
        args.source, args.source_sha256, plan, args.plan_sha256)
    group = next((g for g in plan['tests'] if g['phase'] == args.phase), None)
    retained = OUT / 'retained-lib-1'
    binding = None
    if args.phase == 'format-1':
        argv = [str(TOOLCHAIN / 'rustfmt'), '--edition', '2024', '--config',
                'skip_children=true'] + plan['format_paths']
        wall = 30
    elif args.phase == 'compile-1':
        argv = [str(TOOLCHAIN / 'cargo'), 'test', '--offline', '--locked', '-p',
                'reverie-kvm', '--lib', '--no-run', '--features',
                'native-test-support', '--message-format=json', '-j', '2']
        wall = 600
    elif args.phase == 'check-1':
        # Normal library compilation: cfg(test) is absent, with the same explicit feature.
        argv = [str(TOOLCHAIN / 'cargo'), 'check', '--offline', '--locked', '-p',
                'reverie-kvm', '--lib', '--features', 'native-test-support',
                '--message-format=json', '-j', '2']
        wall = 600
    elif args.phase == 'clippy-1':
        argv = [str(TOOLCHAIN / 'cargo'), 'clippy', '--offline', '--locked',
                '-p', 'reverie-kvm', '--all-targets', '--features',
                'native-test-support', '--', '-D', 'warnings']
        wall = 600
    elif args.phase == 'format-check-1':
        argv = [str(TOOLCHAIN / 'cargo'), 'fmt', '--all', '--', '--check']
        wall = 30
    elif args.phase == 'inventory-1' or group:
        binding = json.loads((OUT / 'EXECUTABLE-1.json').read_text())
        if (binding['source_sha256'] != args.source_sha256 or
                binding['plan_sha256'] != args.plan_sha256 or
                identity(retained) != binding['executable'] or
                retained.stat().st_mode & 0o777 != 0o555):
            raise ValueError('retained executable does not match the admitted source/plan')
        argv = [str(retained)]
        if group:
            inventory = json.loads((OUT / 'inventory-1' / 'INVENTORY.json').read_text())
            inventory_result = json.loads((OUT / 'inventory-1' / 'result.json').read_text())
            if (not inventory_result['accepted'] or
                    inventory['executable'] != binding['executable'] or
                    inventory['source_sha256'] != args.source_sha256 or
                    inventory['plan_sha256'] != args.plan_sha256):
                raise ValueError('inventory was not accepted for this executable/source/plan')
            selected_names(plan, inventory['names'])
            if group['exact']:
                argv.append('--exact')
            argv += [group['selector'], '--test-threads=1', '--nocapture']
            wall = 60
        else:
            argv.append('--list')
            wall = 30
    else:
        raise ValueError('unknown phase kind')

    phase_dir = local_path(args.phase)
    phase_dir.mkdir()
    env = {'HOME': '/home/newton', 'PATH': str(TOOLCHAIN) + ':/usr/bin:/bin',
           'CARGO_HOME': '/home/newton/.cargo', 'CARGO_TARGET_DIR': str(TARGET),
           'RUSTC': str(TOOLCHAIN / 'rustc'), 'RUSTDOC': str(TOOLCHAIN / 'rustdoc'),
           'CARGO_BUILD_JOBS': '2', 'CARGO_NET_OFFLINE': 'true',
           'CARGO_TERM_COLOR': 'never', 'LANG': 'C', 'LC_ALL': 'C', 'TZ': 'UTC',
           'RUSTUP_TOOLCHAIN': 'nightly-2026-07-29', 'RUSTUP_AUTO_INSTALL': '0',
           'REVERIE_REQUIRE_KVM': '1'}
    unit = 'kvm-entry-owner-' + args.phase + '-' + str(os.getpid())
    command = ['/usr/bin/systemd-run', '--user', '--wait', '--pipe', '--collect',
               '--unit', unit, '--property=CPUQuota=200%', '--property=MemoryMax=12G',
               '--property=MemorySwapMax=0', '--property=LimitCORE=0',
               '--property=RuntimeMaxSec=' + str(wall), '--property=TimeoutStopSec=10',
               '--property=KillMode=control-group', '--working-directory=' + str(ROOT),
               '/usr/bin/env', '-i'] + [k + '=' + v for k, v in env.items()] + argv
    before = source_identity(plan)
    if before != source['source']:
        raise ValueError('source changed after admission')
    executable_before = identity(argv[0])
    tools_before = {name: identity(TOOLCHAIN / name)
                    for name in ['cargo', 'rustc', 'rustdoc', 'rustfmt']}
    caller_before = {name: identity(OUT / name) for name in ['runner.py', 'support.py']}
    write_json(phase_dir / 'command.json', {
        'argv': argv, 'environment': env, 'service': command, 'unit': unit,
        'source': before, 'source_manifest': args.source, 'source_binding': source_binding,
        'plan': args.plan, 'plan_binding': plan_binding,
        'executable_path': argv[0], 'executable_before': executable_before,
        'tools_before': tools_before, 'caller_before': caller_before,
        'retained_binding': binding, 'wall_seconds': wall,
        'stream_cap_bytes': STREAM_CAP, 'dispatched_base': plan['dispatch_base']})
    started = time.monotonic()
    process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    selector = selectors.DefaultSelector()
    streams, counts = {}, {}
    for name, pipe in [('stdout', process.stdout), ('stderr', process.stderr)]:
        selector.register(pipe, selectors.EVENT_READ, name)
        streams[name] = (phase_dir / name).open('xb')
        counts[name] = 0
    refusals, controls = [], []
    observed = service_show(unit)
    hard_stopped = False
    while selector.get_map():
        for key, _ in selector.select(0.25):
            chunk = os.read(key.fileobj.fileno(), 65536)
            if not chunk:
                selector.unregister(key.fileobj)
                key.fileobj.close()
                continue
            name = key.data
            remaining = max(0, STREAM_CAP - counts[name])
            streams[name].write(chunk[:remaining])
            counts[name] += len(chunk)
            if counts[name] > STREAM_CAP and not refusals:
                refusals.append('stream output limit')
                stop_service(unit, controls)
        elapsed = time.monotonic() - started
        if elapsed > wall + 20 and not refusals:
            refusals.append('outer deadline')
            stop_service(unit, controls)
        if elapsed > wall + 45 and not hard_stopped:
            hard_stopped = True
            refusals.append('hard outer deadline')
            kill = ['/usr/bin/systemctl', '--user', 'kill', '--signal=SIGKILL', unit]
            try:
                result = subprocess.run(kill, capture_output=True, text=True, timeout=5)
                controls.append({'argv': kill, 'raw_status': result.returncode,
                                 'stdout': result.stdout, 'stderr': result.stderr})
            except subprocess.TimeoutExpired:
                controls.append({'argv': kill, 'refusal': 'systemctl kill timeout'})
            process.kill()
            for key in list(selector.get_map().values()):
                selector.unregister(key.fileobj)
                key.fileobj.close()
    for stream in streams.values():
        stream.close()
    try:
        raw_status = process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        refusals.append('wrapper did not terminate after streams closed')
        stop_service(unit, controls)
        process.kill()
        raw_status = process.wait(timeout=5)
    result = {'raw_status': raw_status, 'elapsed_seconds': time.monotonic() - started,
              'stream_bytes': counts, 'refusals': refusals, 'control_commands': controls,
              'observed_service': observed, 'accepted': False}
    # Retain the raw outcome before post-execution authentication or parsing can fail.
    write_json(phase_dir / 'RAW-RESULT.json', result)
    try:
        result['terminal_service'] = service_show(unit)
        fields = result['terminal_service']['fields']
        if not (fields.get('LoadState') == 'not-found' or
                (fields.get('ActiveState') in ['inactive', 'failed'] and
                 fields.get('MainPID') == '0')):
            refusals.append('service terminal state not established')
        cgroup = observed['fields'].get('ControlGroup')
        result['observed_cgroup'] = cgroup or None
        if cgroup:
            result['observed_cgroup_absent'] = not (
                ROOT.__class__('/sys/fs/cgroup') / cgroup.lstrip('/')).exists()
            if not result['observed_cgroup_absent']:
                refusals.append('observed service cgroup remains')
        result['source_after'] = source_identity(plan)
        changed = [name for name in before if before[name] != result['source_after'][name]]
        result['changed_source_paths'] = changed
        if args.phase == 'format-1':
            if set(changed) - set(plan['format_paths']):
                refusals.append('format changed a path outside the five admitted files')
        elif changed:
            refusals.append('source or lock identity changed')
        result['executable_after'] = identity(argv[0])
        result['tools_after'] = {name: identity(TOOLCHAIN / name) for name in tools_before}
        result['caller_after'] = {name: identity(OUT / name) for name in caller_before}
        if (executable_before != result['executable_after'] or
                tools_before != result['tools_after'] or caller_before != result['caller_after']):
            refusals.append('executable, toolchain, or caller identity changed')
        if identity(local_path(args.plan)) != plan_binding or identity(local_path(args.source)) != source_binding:
            refusals.append('plan or source manifest changed')
        stdout = (phase_dir / 'stdout').read_text(errors='replace')
        if group:
            summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', stdout)
            result['selection'] = {'expected': group, 'summaries': summaries}
            expected_summaries = group.get('expected_summaries', [[str(group['expected_passed']), '0', '0']])
            if summaries != [tuple(item) for item in expected_summaries]:
                refusals.append('selected test count/result mismatch')
        if args.phase == 'inventory-1':
            names = re.findall(r'^(.+): test$', stdout, flags=re.MULTILINE)
            summaries = re.findall(r'^(\d+) tests?, (\d+) benchmarks?$', stdout, flags=re.MULTILINE)
            if len(names) != len(set(names)) or summaries != [(str(len(names)), '0')]:
                raise ValueError('inventory summary and listed declarations differ')
            selections = selected_names(plan, names)
            write_json(phase_dir / 'INVENTORY.json', {
                'names': names, 'declaration_count': len(names), 'selections': selections,
                'executable': executable_before, 'source_sha256': args.source_sha256,
                'plan_sha256': args.plan_sha256, 'tests_executed': 0})
    except Exception as error:
        refusals.append('post-execution authentication failed: ' + repr(error))
    result['refusals'] = refusals
    result['accepted'] = raw_status == 0 and not refusals
    result['stream_records'] = {name: identity(phase_dir / name) for name in streams}
    write_json(phase_dir / 'result.json', result)
    print(json.dumps({'phase': args.phase, 'raw_status': raw_status,
                      'accepted': result['accepted'], 'elapsed_seconds': result['elapsed_seconds'],
                      'refusals': refusals}))
    return 0 if result['accepted'] else (raw_status if raw_status > 0 else 1)


if __name__ == '__main__':
    sys.exit(main())
