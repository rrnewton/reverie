#!/usr/bin/env python3
"""Run exactly once only after root approves RUN_PLAN.md. No product build."""
import hashlib
import json
import math
import os
from pathlib import Path
import resource
import selectors
import signal
import subprocess
import sys
import time

SOURCE = Path(__file__).resolve().parent
EVIDENCE = Path('/home/newton/work/dev-hermit/ignored/liteinst-01a0a13c-review/queue-drain-20260916/liteinst-continuation-native-experiment')
CASES = ['getpid-open', 'getpid-denied', 'revoke-stack']
MODES = ['native', 'intercepted', 'intercepted-omit-xstate']
MASK64 = (1 << 64) - 1


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for b in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def save(path, value):
    with path.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def check(condition, message):
    if not condition:
        raise ValueError(message)


def analyze_state(data, row):
    """Check exact per-process bindings before the narrowly named substitutions."""
    check((data['rseq_initial_size'] == 0 and data['rseq_unregistered'] == data['rseq_registered_after'] == 0) or
          (0 < data['rseq_initial_size'] <= 32 and data['rseq_area'] != 0 and data['rseq_unregistered'] == data['rseq_registered_after'] == 1),
          'same actual rseq isolation and restoration in native and intercepted modes')
    f = data['fields']
    g = f[24:43]
    r = data['regions']
    check(data['pid'] == row['pid'] == data['owner_tid'], 'actual child PID/TID binding')
    check(len(f) == 64 and len(g) == 19, 'full general-register inventory')
    check(f[44] == 1 and f[43] == f[17], 'actual guest continuation/PKRU observation')
    check(f[17] == (3 if data['case_kind'] == 1 else 0), 'requested guest rights')
    check(r['control'][0] == g[7], 'R15 must be the exact control mapping')
    check(g[15] == f[15] == (r['guest'][1] - 64) & ~15, 'RSP must be the exact original guest SP')
    check(f[16] == (r['observer'][1] - 64) & ~15, 'separate observer stack pointer')
    check(len(set(tuple(v) for v in r.values())) == 6, 'six distinct owned mappings')
    regions = sorted(r.values())
    check(all(regions[i][1] <= regions[i + 1][0] for i in range(5)), 'no overlapping owned mappings')
    check(data['final_mask'] == 1 << 11, 'actual completion mask blocks SIGUSR2 only')
    check(data['final_alt_sp'] == r['alt1'][0] and data['final_alt_size'] == 262144 and data['final_alt_flags'] == 0,
          'actual completion alternate stack metadata')
    check(data['stack_mapping_none'] == int(data['case_kind'] == 2), 'actual original stack mapping effect')
    args = f[9:15]
    expected = [args[4], args[5], args[3], 0xed7,
                0x1212121212121212, 0x1313131313131313, 0x1414141414141414,
                r['control'][0], args[0], args[1], 0xb0b0b0b0b0b0b0b0,
                0xb1b1b1b1b1b1b1b1, args[2],
                0 if data['case_kind'] == 2 else data['pid'],
                data['ips']['guest_resume'], f[15], data['ips']['guest_resume'], 0xed7, g[18]]
    check(g == expected, 'all meaningful GPRs/flags, including native SYSCALL RCX/R11 clobbers')
    check(g[18] & 0xffff == 0x33 and g[18] >> 48 == 0x2b, 'actual user CS/SS selectors')
    if data['case_kind'] == 2:
        check(f[8] == 10 and args[:3] == [r['guest'][0], 262144, 0], 'actual mprotect arguments')
    else:
        check(f[8] == 39, 'getpid syscall identity')
        check(args == [0x1010101010101010 + i for i in range(6)], 'unchanged ignored getpid arguments')
    n = list(g)
    n[7] = '<exact control.lo>'
    n[15] = '<exact guest stack pointer>'
    if data['case_kind'] == 2:
        n[8] = '<exact guest.lo>'
    else:
        n[13] = '<actual child pid>'
    # No range-based or general address normalization. RIP/RCX/FS/GS remain exact.
    if data['native']:
        check(data['handler_entries'] == data['restorer_requests'] == data['callback_observed'] == 0,
              'native has no signal mediation')
    else:
        check(data['handler_entries'] == data['restorer_requests'] == 2, 'two actual handler entries and two restorer requests')
        check(data['entry_accepted'] == data['completion_accepted'] == data['callback_observed'] == data['callback_finished'] == 1,
              'both accepted entry sites and observed callback')
        check(data['real_effects'] == data['completion_metadata_preserved'] == 1 and data['phase'] == 4,
              'one real physical effect and completed frame copy')
        check(data['post_pkru'] == f[17] and data['callback_pkru'] == 0 and data['callback_flags'] & 0x400 == 0,
              'callback rights/DF and actual post-effect rights')
        check(r['callback'][0] <= data['callback_sp'] < r['callback'][1], 'ordinary callback uses owned stack')
        entry, completion = data['entry'], data['completion']
        check(entry['mask'] == 1 << 9 and completion['mask'] == 1 << 11, 'completion metadata differs from entry')
        for frame, name in [(entry, 'alt0'), (completion, 'alt1')]:
            lo, hi = r[name]
            check(lo <= frame['address'] and frame['address'] + 440 <= hi, 'genuine frame within exact alternate stack')
            check(lo <= frame['handler_sp'] < hi, 'handler uses actual alternate stack')
            check(lo <= frame['fp_address'] and frame['fp_address'] + frame['extended'] <= hi, 'whole actual FP frame bound')
            check(frame['alt_sp'] == lo and frame['alt_size'] == hi - lo, 'actual kernel alternate stack descriptor')
        check(entry['address'] != completion['address'] and entry['fp_address'] != completion['fp_address'], 'fresh second frame')
        check(entry['gregs'][15] == f[15] and entry['gregs'][16] == data['ips']['guest_resume'] and entry['gregs'][17] & 0x400,
              'saved original guest RSP/RIP/DF')
    raw = bytes.fromhex(data['observed_xsave_hex'])
    check(len(raw) == data['cpuid_xsave_size'], 'actual XSAVE output width')
    present = int.from_bytes(raw[512:520], 'little')
    check(int.from_bytes(raw[520:528], 'little') == 0 and raw[528:576] == bytes(48), 'standard XSAVE header')
    known = 3 | 4 | 0xe0 | (1 << 9)
    check(present & ~known == 0, 'unseeded noninitial XSTATE is outside this fixture; do not silently omit it')
    check(present & ~f[7] == 0, 'observed active XSTATE within XCR0')
    state = {}
    # x87 padding and empty register payloads are not architectural values.
    if present & 1:
        top = (int.from_bytes(raw[2:4], 'little') >> 11) & 7
        tags = raw[4]
        occupied = {str(i): raw[32 + i * 16:42 + i * 16].hex()
                    for i in range(8) if tags & (1 << ((top + i) & 7))}
        state['x87'] = {'control': raw[:2].hex(), 'status': raw[2:4].hex(), 'tags': tags,
                        'opcode': raw[6:8].hex(), 'ip': raw[8:16].hex(), 'data_pointer': raw[16:24].hex(),
                        'occupied_registers': occupied}
    else:
        state['x87'] = {'control': '7f03', 'status': '0000', 'tags': 0,
                        'opcode': '0000', 'ip': '00' * 8, 'data_pointer': '00' * 8, 'occupied_registers': {}}
    state['mxcsr'] = raw[24:28].hex()
    state['xmm0_15'] = (raw[160:416] if present & 2 else bytes(256)).hex()
    for index, name, size in [(2, 'ymm_upper0_15', 256), (5, 'opmask0_7', 64),
                               (6, 'zmm_upper0_15', 512), (7, 'zmm16_31', 1024), (9, 'pkru', 8)]:
        if not f[7] & (1 << index):
            continue
        offset, actual_size = data['components'][index]
        check(actual_size == size and 576 <= offset and offset + size <= len(raw), 'CPUID component layout')
        payload = raw[offset:offset + size] if present & (1 << index) else bytes(size)
        state[name] = payload[:4].hex() if index == 9 else payload.hex()
    check(int.from_bytes(bytes.fromhex(state['pkru']), 'little') == f[43], 'XSAVE PKRU equals independently read live PKRU')
    if not data['omit_xstate']:
        check(state['mxcsr'] == (0x3f80).to_bytes(4, 'little').hex(), 'seeded MXCSR survives')
        check(present & data['seeded_features'] == data['seeded_features'] or
              (f[17] == 0 and present & (data['seeded_features'] & ~(1 << 9)) == data['seeded_features'] & ~(1 << 9)),
              'seeded extended components actually active; init PKRU may omit its bit')
    return {'gregs': n, 'xstate': state, 'active_xstate_bv': present,
            'seeded_features': data['seeded_features'],
            'observed_successful_signal_returns': 0 if data['native'] else 2,
            'return_observation_basis': 'normal callback entry after request 1; guest observation after request 2; no independent kernel-stop census'}


def compare(native, candidate):
    diffs = []
    if native['gregs'] != candidate['gregs']:
        diffs.append('gregs')
    for k in sorted(set(native['xstate']) | set(candidate['xstate'])):
        if native['xstate'].get(k) != candidate['xstate'].get(k):
            diffs.append(k)
    return {'native_equivalence': not diffs, 'differences': diffs}


def main():
    check(len(sys.argv) == 2 and sys.argv[1] == 'run-01', 'only the reviewed single run directory is permitted')
    root = EVIDENCE / sys.argv[1]
    root.mkdir(exist_ok=False)
    (root / 'commands').mkdir()
    (root / 'build').mkdir()
    (root / 'tmp').mkdir()
    inputs = json.loads((EVIDENCE / 'PLAN_INPUTS.json').read_text())
    for item in inputs['experiment_sources']:
        p = SOURCE / item['name']
        check(digest(p) == item['sha256'] and p.stat().st_size == item['bytes'], 'approved source binding changed: ' + str(p))
    input_source = root / 'source'
    input_source.mkdir()
    for item in inputs['experiment_sources']:
        source_bytes = (SOURCE / item['name']).read_bytes()
        check(hashlib.sha256(source_bytes).hexdigest() == item['sha256'], 'source copy binding')
        (input_source / item['name']).write_bytes(source_bytes)
    cgroup_line = next(x[3:] for x in Path('/proc/self/cgroup').read_text().splitlines() if x.startswith('0::'))
    cgroup = Path('/sys/fs/cgroup') / cgroup_line.lstrip('/')
    limits = {n: (cgroup / n).read_text().strip() for n in ['cpu.max', 'memory.max', 'memory.swap.max', 'pids.max']}
    save(root / 'limits.json', {'path': str(cgroup), 'effective_files': limits})
    check(limits == {'cpu.max': '400000 100000', 'memory.max': '8589934592', 'memory.swap.max': '0', 'pids.max': '1024'}, 'owned scope exact limits')
    env = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'LANG': 'C', 'TMPDIR': str(root / 'tmp'), 'HOME': os.environ['HOME']}
    rows = []

    def run(name, argv, timeout, output_limit, file_limit):
        prefix = root / 'commands' / name
        def bounds():
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
            resource.setrlimit(resource.RLIMIT_FSIZE, (file_limit, file_limit))
            resource.setrlimit(resource.RLIMIT_AS, (8589934592, 8589934592))
            cpu = math.ceil(timeout)
            resource.setrlimit(resource.RLIMIT_CPU, (cpu, cpu + 1))
        start = time.monotonic()
        usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)
        child = subprocess.Popen(list(map(str, argv)), cwd=root / 'build', env=env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 preexec_fn=bounds, start_new_session=True)
        mux = selectors.DefaultSelector()
        outputs, counts = {}, {}
        for label, pipe in [('stdout', child.stdout), ('stderr', child.stderr)]:
            mux.register(pipe, selectors.EVENT_READ, label)
            outputs[label] = Path(str(prefix) + '.' + label).open('xb')
            counts[label] = 0
        reason = None
        try:
            while mux.get_map() or child.poll() is None:
                if reason is None and time.monotonic() - start > timeout:
                    reason = 'timeout'
                if reason:
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                for key, _ in mux.select(.025):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        mux.unregister(key.fileobj)
                        continue
                    label = key.data
                    outputs[label].write(data[:max(0, output_limit - counts[label])])
                    counts[label] += len(data)
                    if counts[label] > output_limit:
                        reason = 'output_limit'
            status = child.wait()
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
            for f in outputs.values():
                f.close()
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        row = {'name': name, 'argv': list(map(str, argv)), 'cwd': str(root / 'build'), 'environment': env,
               'pid': child.pid, 'returncode': status, 'terminating_signal': -status if status < 0 else None,
               'elapsed_seconds': time.monotonic() - start, 'cpu_user_seconds': after.ru_utime - usage_before.ru_utime,
               'cpu_system_seconds': after.ru_stime - usage_before.ru_stime, 'timeout_seconds': timeout,
               'output_limit_bytes_per_stream': output_limit, 'file_limit_bytes': file_limit,
               'limit_reason': reason, 'observed_output_bytes': counts}
        save(Path(str(prefix) + '.json'), row)
        rows.append(row)
        print(name, status, reason, flush=True)
        return row

    compiler = Path('/usr/bin/cc').resolve()
    save(root / 'compiler.json', {'requested': '/usr/bin/cc', 'resolved': str(compiler), 'sha256': digest(compiler)})
    elf = root / 'build' / 'continuation-fixture'
    build = run('build', ['/usr/bin/cc', '-std=gnu11', '-O2', '-g3', '-Wall', '-Wextra', '-Werror',
                         '-mgeneral-regs-only', '-fno-omit-frame-pointer', '-fno-stack-protector', '-fno-builtin',
                         '-fno-pie', '-no-pie', '-Wl,-z,now', '-Wl,-z,noexecstack', '-save-temps=obj', '-MD', '-v',
                         '-I', input_source, input_source / 'fixture.c', input_source / 'bridge.S', '-ldl', '-o', elf],
                180, 4 * 1024 * 1024, 512 * 1024 * 1024)
    if build['returncode'] != 0 or build['limit_reason']:
        save(root / 'RESULTS.json', {'build_pass': False, 'native_cases': 'unexecuted after failed build', 'commands': rows})
        return 1
    save(root / 'elf-binding.json', {'path': str(elf), 'bytes': elf.stat().st_size, 'sha256': digest(elf), 'sources': inputs['experiment_sources']})
    for name, argv in [('readelf', ['/usr/bin/readelf', '-aW', elf]),
                       ('disassembly', ['/usr/bin/objdump', '-drwC', '-Mintel', elf]),
                       ('symbols', ['/usr/bin/nm', '-n', elf])]:
        row = run(name, argv, 10, 4 * 1024 * 1024, 512 * 1024 * 1024)
        if row['returncode'] != 0 or row['limit_reason']:
            save(root / 'RESULTS.json', {'build_pass': True, 'artifact_inspection_pass': False, 'native_cases': 'unexecuted', 'commands': rows})
            return 1
    symbols = {}
    for line in (root / 'commands/symbols.stdout').read_text().splitlines():
        words = line.split()
        if len(words) == 3:
            symbols[words[2]] = int(words[0], 16)
    states, outcomes = {}, []
    for case in CASES:
        for mode in MODES:
            name = case + '--' + mode
            row = run(name, [elf, mode, case], 5, 1024 * 1024, 1024 * 1024)
            result = {'name': name, 'mode': mode, 'case': case, 'process': row, 'state_checked': False}
            try:
                check(row['returncode'] == 0 and row['limit_reason'] is None, 'actual process failed or bounded out')
                check((root / ('commands/' + name + '.stderr')).stat().st_size == 0, 'unexpected actual stderr')
                data = json.loads((root / ('commands/' + name + '.stdout')).read_text())
                for symbol, address in data['ips'].items():
                    check(symbols[symbol] == address, 'actual ELF symbol binding: ' + symbol)
                check(data['case_kind'] == CASES.index(case) and data['native'] == (mode == 'native') and
                      data['omit_xstate'] == (mode == 'intercepted-omit-xstate'), 'exact mode and case binding')
                state = analyze_state(data, row)
                states[(case, mode)] = state
                result.update(state_checked=True, architectural_state=state)
                # Retain actual observations and both genuine frame images as separate binary artifacts.
                blob_dir = root / name
                blob_dir.mkdir()
                for label, content in [('observed.xsave', data['observed_xsave_hex']),
                                       ('entry.fp', data['entry']['fp_hex']), ('completion.fp', data['completion']['fp_hex'])]:
                    (blob_dir / label).write_bytes(bytes.fromhex(content))
            except (ValueError, KeyError, IndexError, TypeError) as error:
                result['error'] = str(error)
            outcomes.append(result)
    comparisons = []
    for case in CASES:
        for mode in MODES[1:]:
            item = {'case': case, 'mode': mode, 'native_equivalence': False, 'comparison_available': False}
            if (case, 'native') in states and (case, mode) in states:
                item.update(compare(states[(case, 'native')], states[(case, mode)]), comparison_available=True)
                if mode == 'intercepted-omit-xstate':
                    required = {'x87', 'mxcsr', 'xmm0_15'}
                    for bit, name in [(2, 'ymm_upper0_15'), (5, 'opmask0_7'), (6, 'zmm_upper0_15'), (7, 'zmm16_31')]:
                        if states[(case, 'native')]['seeded_features'] & (1 << bit):
                            required.add(name)
                    item['required_clobber_differences'] = sorted(required)
                    item['deliberate_omission_detected'] = (not item['native_equivalence'] and
                                                          'gregs' not in item['differences'] and
                                                          'pkru' not in item['differences'] and
                                                          required <= set(item['differences']))
            comparisons.append(item)
    rejection_controls = []
    for case, fatal in [('wrong-phase', 141), ('spoof-signal', 130)]:
        name = case + '--intercepted'
        row = run(name, [elf, 'intercepted', case], 5, 1024 * 1024, 1024 * 1024)
        item = {'case': case, 'process': row, 'expected_rejection_observed': False}
        try:
            data = json.loads((root / ('commands/' + name + '.stdout')).read_text())
            item['actual_record'] = data
            item['expected_rejection_observed'] = (row['returncode'] == 90 and row['limit_reason'] is None and
                (root / ('commands/' + name + '.stderr')).stat().st_size == 0 and
                data == {'fatal': fatal, 'handler_entries': 1, 'restorer_requests': 0, 'callback_observed': 0, 'guest_observed': 0, 'phase': 0})
        except (ValueError, KeyError, TypeError):
            pass
        rejection_controls.append(item)
    positive_pass = all(x['state_checked'] for x in outcomes if x['mode'] != 'intercepted-omit-xstate') and all(
        x['comparison_available'] and x['native_equivalence'] for x in comparisons if x['mode'] == 'intercepted')
    clobbers_detected = all(x.get('deliberate_omission_detected', False) for x in comparisons if x['mode'] == 'intercepted-omit-xstate')
    result = {'build_pass': True, 'artifact_inspection_pass': True, 'native_cases': outcomes,
              'comparisons': comparisons, 'provenance_rejection_controls': rejection_controls,
              'positive_native_equivalence_pass': positive_pass, 'negative_xstate_omissions_detected': clobbers_detected,
              'all_requested_checks_pass': positive_pass and clobbers_detected and all(x['expected_rejection_observed'] for x in rejection_controls),
              'physical_counts_scope': 'in-process handler entries, restorer requests, and observed callback/guest continuations; no independent kernel-stop census',
              'commands': rows}
    save(root / 'RESULTS.json', result)
    check(digest(elf) == json.loads((root / 'elf-binding.json').read_text())['sha256'], 'ELF unchanged throughout all children')
    for item in inputs['experiment_sources']:
        check(digest(input_source / item['name']) == item['sha256'], 'compiled source snapshot unchanged')
    # This manifest freezes the actual outputs, compiler intermediates, source/ELF binding and every failed outcome.
    files = []
    for p in sorted(root.rglob('*')):
        if p.is_file():
            files.append({'path': str(p.relative_to(root)), 'bytes': p.stat().st_size, 'sha256': digest(p)})
    save(root / 'MANIFEST.json', files)
    for item in files:
        p = root / item['path']
        check(digest(p) == item['sha256'] and p.stat().st_size == item['bytes'], 'evidence readback')
    return 0 if result['all_requested_checks_pass'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
