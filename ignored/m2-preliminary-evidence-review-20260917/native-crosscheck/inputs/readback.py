"""Read complete bounded phase outputs; preserve identity and diagnostic evidence."""
import hashlib
import os
from pathlib import Path
import re
import shutil
from common import MIB, check_file, file_record, json_read, owned, read, require, write_new


def native_list(stdout):
    names = []
    for line in stdout.decode().splitlines():
        if not line:
            continue
        if line.endswith(': test'):
            names.append(line[:-6])
        else:
            require(re.fullmatch(r'\d+ tests?, 0 benchmarks?', line), 'unexpected libtest inventory output: ' + line)
    require(len(names) == len(set(names)), 'duplicate actual native identities')
    return sorted(names)


def native_result(stdout, names, population):
    text = stdout.decode()
    passed = re.findall(r'^test (\S+) \.\.\. ok$', text, re.M)
    recaps = re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', text, re.M)
    require(sorted(passed) == sorted(names) and len(passed) == len(set(passed)), 'actual named native outcomes differ')
    require(recaps == [(str(len(names)), '0', '0', '0', str(population-len(names)))], 'native counts or actual filtered population differ')
    return dict(passed_names=sorted(passed), passed=len(passed), failed=0, ignored=0, filtered=population-len(names))


def detcore_expected_panics(stdout, stderr, names, population):
    """Keep the four existing source-bound expected-panic successes exact."""
    import json
    from common import REPO
    rows = [
        ('tool_global::robust_exit_clock_tests::robust_exit_clock_ack_still_refuses_a_backwards_owner_sample', 'detcore/src/tool_global.rs', 'Attempted to update tid 17 time'),
        ('tool_global::tests::backend_failure_tests::ordinary_unknown_started_owner_is_not_acknowledged_as_unstarted', 'detcore/src/tool_global/tests/backend_failure_tests.rs', 'a started thread must have a scheduler registration'),
        ('tool_global::tests::backend_failure_tests::unstarted_cleanup_rejects_a_different_payload_mm', 'detcore/src/tool_global/tests/backend_failure_tests.rs', 'deregistration must retain its MmId'),
        ('tool_global::tests::backend_failure_tests::unstarted_cleanup_rejects_another_senders_identity', 'detcore/src/tool_global/tests/backend_failure_tests.rs', 'deregistration must belong to its sender'),
    ]
    expected = {}
    for name, source, message in rows:
        pattern = r'#\[should_panic\(expected = ' + re.escape(json.dumps(message)) + r'\)\]\s+async fn ' + re.escape(name.split('::')[-1]) + r'\(\)'
        require(len(re.findall(pattern, read(REPO/source).decode())) == 1, 'exact expected-panic source annotation absent')
        expected[name] = message
    require(set(expected).issubset(names) and len(names) == 54 and len(set(names)) == 54, 'Detcore cohort changed')
    sections = re.findall(r"thread '([^']+)' \(\d+\) panicked at ([\s\S]*?)(?=\nthread '|\Z)", stderr.decode())
    caught_name = 'tool_global::tests::backend_failure_tests::backend_failure_cleanup_retains_panicked_scheduler_and_recording_error'
    require(caught_name in names, 'new caught scheduler panic control absent')
    caught_source = read(REPO/'detcore/src/tool_global/tests/backend_failure_tests.rs').decode()
    require('panic!("scheduler cleanup control");' in caught_source and 'assert!(cleanup.scheduler.unwrap_err().is_panic());' in caught_source, 'caught panic premise/result changed')
    messages = {**expected, caught_name: 'scheduler cleanup control'}
    require(len(sections) == 5 and set(n for n,_ in sections) == set(messages), 'unexpected or missing panic output')
    for name, body in sections:
        require(messages[name] in body, 'expected panic message missing')

    def inspect_output(raw):
        text = raw.decode()
        actual = re.findall(r'^test (\S+)( - should panic)? \.\.\. (ok|FAILED|ignored)$', text, re.M)
        require(len(actual) == len(names) and sorted(n for n,_,_ in actual) == sorted(names), 'exact named outcomes differ')
        for name, suffix, status in actual:
            require(status == 'ok', 'nonpassing native outcome')
            require(suffix == (' - should panic' if name in expected else ''), 'unexpected or missing expected-panic label')
        for name in expected:
            text = text.replace('test '+name+' - should panic ... ok', 'test '+name+' ... ok')
        return native_result(text.encode(), names, population)

    result = inspect_output(stdout)
    mutations = [stdout.replace(b' - should panic ... ok', b' ... ok', 1), stdout.replace(b' ... ok', b' - should panic ... ok', 1), stdout.replace(b'54 passed;', b'53 passed;', 1), stdout.replace(b' ... ok', b' ... FAILED', 1)]
    controls = []
    for index, changed in enumerate(mutations):
        try:
            inspect_output(changed)
        except RuntimeError as error:
            controls.append(dict(index=index, refused=True, reason=str(error)))
        else:
            raise RuntimeError('negative reader control admitted')
    result.update(expected_panic_names=sorted(expected), negative_reader_controls=controls)
    return result


def inventory(path, baseline=None, selected_names=None):
    value = json_read(path)
    suites = value['rust-suites']
    identities, executable_bindings = [], []
    for binary_id, suite in suites.items():
        require(binary_id == suite['binary-id'], 'suite key disagrees with binary identity')
        require(suite['status'] == 'listed', 'suite was not actually listed')
        executable_bindings.append(file_record(suite['binary-path']))
        for name, case in suite['testcases'].items():
            identities.append(dict(binary_id=binary_id, test_name=name, ignored=case['ignored'], filter_match=case['filter-match']))
    keys = [(row['binary_id'], row['test_name']) for row in identities]
    require(len(keys) == len(set(keys)), 'duplicate Nextest identities')
    require(value['test-count'] == len(identities), 'actual Nextest population disagrees with named inventory')
    selected = [row for row in identities if row['filter_match']['status'] == 'matches']
    require(all(row['ignored'] is False for row in selected), 'selected ignored tests cannot be credited')
    if baseline is not None:
        check_file(baseline)
        old = json_read(baseline['path'])
        current = {(row['binary_id'], row['test_name']): row for row in identities}
        require(all(current.get((row['binary_id'], row['test_name'])) == row for row in old), 'baseline name/ignored/filter status changed')
    if selected_names is not None:
        require(sorted(row['test_name'] for row in selected) == sorted(selected_names), 'actual selected names differ from original cohort')
    return dict(identities=sorted(identities, key=lambda row:(row['binary_id'],row['test_name'])),
                selected_count=len(selected), selected=selected, executable_bindings=executable_bindings,
                enumeration_only=True)


def compile_artifacts(stdout, config, context, out):
    events = [__import__('json').loads(line) for line in stdout.splitlines() if line]
    diagnostics = [event for event in events if event.get('reason') == 'compiler-message']
    write_new(out / 'compiler-messages.json', diagnostics)
    emitted = [event for event in events if event.get('reason') == 'compiler-artifact' and event.get('executable')]
    write_new(out / 'compiler-executables.json', emitted)
    require(not diagnostics, 'compiler diagnostics retained; no clean compilation claim')
    require([row.get('success') for row in events if row.get('reason') == 'build-finished'] == [True], 'compiler did not emit exactly one successful completion')
    target = Path(context['target_cache']['path']).resolve(strict=True)
    copied = {}
    for selector in config['artifact_selectors']:
        matches = [event for event in emitted if event.get('manifest_path') == selector['manifest']
                   and event['target']['name'] == selector['target'] and event['target']['kind'] == selector['kind']
                   and event['profile']['test'] == selector['test']]
        require(len(matches) == 1, 'actual compiler artifact missing/ambiguous: ' + selector['id'])
        original = Path(matches[0]['executable'])
        require(original.is_absolute() and original.resolve(strict=True) == original and original.is_relative_to(target), 'emitted executable escaped owned target or is symlinked')
        before = file_record(original)
        require(os.access(original, os.X_OK), 'emitted artifact is not executable')
        destination = out / ('retained-' + selector['id'])
        require(not destination.exists(), 'retain previous executable')
        with original.open('rb') as source, destination.open('xb') as output:
            shutil.copyfileobj(source, output, 1024**2)
            output.flush(); os.fsync(output.fileno())
        os.chmod(destination, before['mode'])
        retained = file_record(destination)
        require(retained['sha256'] == before['sha256'] and retained['bytes'] == before['bytes'] and retained['mode'] == before['mode'], 'retained ELF differs')
        require((destination.stat().st_dev,destination.stat().st_ino) != (original.stat().st_dev,original.stat().st_ino), 'retained ELF is not a separate inode')
        check_file(before)
        copied[selector['id']] = dict(emitted=before, retained=retained, compiler_event=matches[0])
    return dict(artifacts=copied, compiler_message_count=0, actual_executable_events=len(emitted))


def nextest_events(stdout, names, package, binary):
    events = [__import__('json').loads(line) for line in stdout.splitlines() if line]
    terminals = [event for event in events if event.get('type') == 'test' and event.get('event') in ('ok','failed','ignored')]
    actual = [event['name'] for event in terminals]
    expected = [package + '::' + binary + '$' + name for name in names]
    require(len(actual) == len(set(actual)), 'duplicate terminal Nextest identities or retries')
    # The production writer separately validates typed suite populations, statuses,
    # retry counts and the complete per-attempt CPU records.
    require(sorted(actual) == sorted(expected), 'missing/extra/ignored Nextest terminal identities')
    return dict(events=terminals, all_passed=all(event['event']=='ok' for event in terminals),
                selected_names=sorted(names), actual_named_count=len(terminals))


def canonical_report(path, template, logs):
    report = json_read(path)
    policy = template['strict_policy']; outcome = template['strict_outcome']
    require(report['comparison'] == policy, 'actual full canonical INFO policy differs')
    for key in ('verified','bitwise_parity','verdict','guest_exit_code','guest_signal'):
        require(report[key] == outcome[key], 'canonical result differs: ' + key)
    require(report.get('no_result_reason') is None and report.get('infrastructure_error') is None, 'typed refusal/error present')
    counts = report['compared_log_messages']
    require(all(type(counts[side]) is int and counts[side] > 0 for side in ('left','right')), 'comparison consumed no INFO records')
    output = dict(exit_code=0, signal=None, **{key:outcome[key] for key in ('stdout_bytes','stdout_sha256','stderr_bytes','stderr_sha256')})
    require(report['compared_outputs'] == {'left':output,'right':output}, 'actual guest bytes/status differ from unchanged pthread fixture')
    directory = Path(logs)
    require(directory.is_dir() and not directory.is_symlink(), 'retained INFO directory missing')
    files = list(directory.iterdir())
    require(len(files) == 2, 'expected the two actual retained INFO log files')
    records = []
    for path2 in sorted(files):
        info = file_record(path2,64*MIB)
        require(0 < info['bytes'] <= 64*MIB, 'retained INFO file exceeds the bounded complete read')
        read(path2, 64*MIB)
        records.append(info)
    return dict(report=report, retained_info_logs=records,
                scope='One backend and the unchanged pthread fixture, compared across its two actual runs; no cross-backend INFO comparison.')


def inspect(plan, context, status):
    phase, config = plan['phase'], plan['phase_bindings']
    out = Path(plan['output']); name = phase['name']
    stdout = read(out/'stdout',phase['postread_limit_per_stream_bytes'])
    stderr = read(out/'stderr',phase['postread_limit_per_stream_bytes'])
    records = dict(stdout=file_record(out/'stdout'), stderr=file_record(out/'stderr'), raw_service_status=status)
    mode = config['reader']
    if mode == 'compile':
        records.update(compile_artifacts(stdout, config, context, out))
    elif mode == 'list':
        records['actual_names'] = native_list(stdout)
        records['enumeration_only'] = True
    elif mode == 'native':
        check_file(config['inventory_record'])
        population = json_read(config['inventory_record']['path'])['readback']['actual_names']
        names = config['selected_names']
        require(set(names).issubset(population), 'native selection absent from actual list')
        records.update(detcore_expected_panics(stdout,stderr,names,len(population)) if name == 'native-detcore' else native_result(stdout,names,len(population)))
    elif mode == 'inventory':
        records.update(inventory(out/'stdout',config.get('baseline'),config.get('selected_names')))
    elif mode == 'nextest':
        records.update(nextest_events(stdout,config['selected_names'],config['package'],config['binary']))
        require(records['all_passed'] and status == 0, 'original Nextest cohort failed; run its production typed writer next')
    elif mode == 'canonical':
        template = json_read(plan['frozen_template']['path'])
        records.update(canonical_report(out/'verification.json',template,out/'verify-logs'))
        # Hermit's verify controller emits its own banner; the producer's two
        # compared_outputs carry the precise expected guest stdout/stderr.
    elif mode == 'typed-canonical':
        check_file(config['canonical_phase_result'])
        canonical = json_read(config['canonical_phase_result']['path'])
        require(canonical['accepted'] is True, 'producer phase did not meet the full policy')
        records['producer_result'] = config['canonical_phase_result']
        records['current_producer_owned_reader_status'] = status
    elif mode == 'typed-nextest':
        check_file(config['test_phase_result'])
        previous = json_read(config['test_phase_result']['path'])
        records['original_raw_status'] = previous['raw_status']
        records['production_counts'] = json_read(out/'counts.json')
        records['production_cpu'] = json_read(out/'cpu.json')
        require(records['production_counts']['executed_tests'] == phase['expected_count'], 'production writer changed selected count')
        require(len(records['production_cpu']['attempts']) == phase['expected_count'], 'production attempts missing/extra; no retries authorized')
        expected_ids={'hermit::'+config['binary']+'$'+name for name in config['selected_names']}
        rows=records['production_counts']['results']
        require({row['id'] for row in rows}==expected_ids and len(rows)==len(expected_ids), 'production result identities differ')
        require(all(row['attempts']==1 for row in rows), 'unrequested retry in production results')
        attempts=records['production_cpu']['attempts']
        require({row['identity']['test'] for row in attempts}==set(config['selected_names']) and all(row['identity']['attempt']==1 for row in attempts), 'actual CPU attempt identities differ')
        # Writer exit 0 proves conversion, not original test success.
        require(previous['raw_status'] == 0 and previous['accepted'] is True and all(row['result']=='pass' for row in rows), 'original tests remain failed')
    elif mode == 'binaries':
        value=json_read(out/'stdout')
        binaries=value['rust-binaries']
        require(set(binaries)==set(config['expected_binary_ids']), 'actual hardware binary set differs')
        records['actual_binaries']={}
        for binary_id,item in binaries.items():
            require(item['binary-id']==binary_id, 'binary metadata identity mismatch')
            record=file_record(item['binary-path'])
            require(record==config['compiled_test_executables'][binary_id], 'metadata selected a different compiler-emitted test executable')
            records['actual_binaries'][binary_id]=record
    elif mode == 'metadata':
        value = json_read(out/'stdout')
        sources = [package['source'] for package in value['packages'] if package.get('source') and 'github.com/rrnewton/reverie' in package['source']]
        require(sources and all(source.endswith('#'+context['qualified_forward_reverie_sha']) for source in sources), 'resolved Reverie dependency differs from landed qualified source')
        records['actual_reverie_sources'] = sources
    elif mode == 'cpu-wrapper':
        lines = stdout.decode().splitlines()
        require(len(lines)==1 and Path(lines[0]).is_absolute(), 'actual production wrapper path missing/ambiguous')
        executable = Path(lines[0]); target = Path(context['target_cache']['path'])
        require(executable.resolve(strict=True)==executable and executable.is_relative_to(target), 'production wrapper escaped owned cache')
        records['actual_cpu_wrapper'] = file_record(executable)
    elif mode == 'output-files':
        records['generated_outputs'] = [file_record(out/relative) for relative in config['relative_outputs']]
    elif mode == 'plain':
        pass
    else:
        raise RuntimeError('no bounded reader for phase: '+name)
    require(status == 0, 'nonzero actual payload status retained')
    return records
