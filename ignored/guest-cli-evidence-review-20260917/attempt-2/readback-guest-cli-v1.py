"""Authenticate completed CLI evidence; preserve both original reader refusals."""
import copy
import json
from pathlib import Path
from common import HERE, REPO, MIB, check_file, check_source, file_record, json_read, read, require, terminal, write_new
from prepare_phase import prepare
from readback import inventory, nextest_events
from run_phase import scm_readback

D = HERE / 'guest-cli-readback-v1'
BINDING = HERE / 'guest-cli-readback-v1-binding.json'
CLI = 'original-kvm-cli'
TYPED = CLI + '-typed-results'
EXTRA = 'hermit::cli$run_dbt_verifies_fresh_physical_workdirs'


def selected_events(events, names, identities):
    expected = {'hermit::cli$' + name for name in names}
    require(len(expected) == len(names) == 24, 'original selected population differs')
    lookup = {row['binary_id'] + '$' + row['test_name']: row for row in identities}
    require(len(lookup) == len(identities), 'inventory identities duplicated')
    matched = {key for key, row in lookup.items() if row['filter_match']['status'] == 'matches'}
    require(matched == expected and all(lookup[key]['ignored'] is False for key in expected), 'selected inventory differs')
    terminals = [event for event in events if event.get('type') == 'test' and event.get('event') in ('ok', 'failed', 'ignored')]
    actual = [event['name'] for event in terminals]
    require(len(actual) == len(set(actual)), 'duplicate terminal identity or retry')
    ignored = []
    passed = []
    for event in terminals:
        key = event['name']
        if key in expected:
            require(event['event'] == 'ok', 'selected test failed or was ignored')
            passed.append(key)
        else:
            row = lookup.get(key)
            require(event['event'] == 'ignored' and row is not None and row['ignored'] is True
                    and row['filter_match']['status'] == 'mismatch', 'unexpected terminal is not declared ignored and unselected')
            ignored.append(key)
    require(set(passed) == expected and len(passed) == 24, 'selected terminal missing')
    require(ignored == [EXTRA], 'observed extra ignored identity changed')
    return dict(passed_names=sorted(passed), ignored_unselected=ignored, passed=24, failed=0, selected_ignored=0)


def reader_controls(events, names, identities):
    outcomes = []
    index = next(i for i, row in enumerate(events) if row.get('type') == 'test' and row.get('event') == 'ok')
    extra_index = next(i for i, row in enumerate(events) if row.get('name') == EXTRA and row.get('event') == 'ignored')
    variants = []
    for event in ('ignored', 'failed'):
        rows = copy.deepcopy(events); rows[index]['event'] = event; variants.append(('selected-' + event, rows, identities))
    rows = copy.deepcopy(events); del rows[index]; variants.append(('selected-missing', rows, identities))
    rows = copy.deepcopy(events); rows.append(rows[index]); variants.append(('selected-duplicate', rows, identities))
    rows = copy.deepcopy(events); rows[extra_index]['event'] = 'ok'; variants.append(('unselected-executed', rows, identities))
    rows = copy.deepcopy(events); rows[extra_index]['name'] = 'hermit::cli$unknown'; variants.append(('unknown-ignored', rows, identities))
    for field, value in [('ignored', False), ('filter_match', {'status': 'matches'})]:
        changed = copy.deepcopy(identities)
        next(row for row in changed if row['binary_id'] + '$' + row['test_name'] == EXTRA)[field] = value
        variants.append(('extra-inventory-' + field, events, changed))
    for label, rows, population in variants:
        try:
            selected_events(rows, names, population)
        except RuntimeError as error:
            outcomes.append(dict(control=label, refused=True, reason=str(error)))
        else:
            raise RuntimeError('negative reader control admitted: ' + label)
    return outcomes


def authenticate(name, expected_error):
    path = HERE / 'guest-controls-run-2' / name / 'result.json'
    original = json_read(path)
    require(original['accepted'] is False and original['raw_status'] == 0 and original['terminal_authenticated'] is True
            and original['final_source_inputs_unchanged'] is True and not original.get('source_error')
            and original['error'] == expected_error, 'original refusal differs')
    check_file(original['plan']); plan = json_read(original['plan']['path'])
    check_file(plan['context']); context = json_read(plan['context']['path'])
    target = D / name; target.mkdir(mode=0o700)
    fresh = HERE / 'observer' / ('readback-only-unused-' + name)
    require(not fresh.exists(), 'plan reconstruction path exists')
    derived = prepare(Path(plan['context']['path']), plan['group'], plan['phase_name'], fresh)
    def normalize(value):
        if isinstance(value, list): return [normalize(x) for x in value]
        if isinstance(value, dict): return {key: normalize(x) for key, x in value.items()}
        if isinstance(value, str) and value.startswith(str(fresh)): return plan['output'] + value[len(str(fresh)):]
        return value
    require(normalize(derived) == plan, 'plan differs beyond unused output spelling')
    check_source(context); scm_readback(context, target / 'scm-before', plan['environment'])
    control = path.parent
    require(json_read(control / 'before.json') == dict(source=context['source_manifest'], inputs=context['inputs'], executables=context['executables']), 'original source/input snapshot differs')
    for side in ('before', 'after'):
        require(json_read(control / ('scm-' + side + '-binding.json')) == context['scm'], 'original SCM snapshot differs')
    check_file(original['observer_result']); observed = json_read(original['observer_result']['path'])
    for stream in ('stdout', 'stderr'): check_file(original['transport'][stream])
    receipt = dict(accepted=False, phase=name, kind='read-only authentication of completed execution',
                   original_refusal=file_record(path), plan=original['plan'], source=context['source_manifest'], execution_rerun=False)
    raw = terminal(observed, plan['phase'], original['transport'], target / 'service-post', plan['environment'], receipt)
    require(raw == 0 and observed['comparison_eligible'] is True, 'original service failed/refused')
    payload = json_read(Path(plan['output']) / 'payload-exit.json')
    require(payload == original['payload_exit'] and payload['reaped'] and not payload['local_wait_timed_out'] and payload['returncode'] == 0, 'payload status differs')
    receipt.update(raw_status=raw, cpu_seconds=observed['final_accounting']['cpu_usage_nsec'] / 1e9,
                   observer_wall_seconds=observed['elapsed_seconds'], observer_result=original['observer_result'])
    return receipt, plan, context


def main():
    binding = json_read(BINDING)
    for record in binding['inputs']: check_file(record)
    D.mkdir(mode=0o700)
    report = dict(accepted=False, caller=file_record(Path(__file__).resolve()), binding=file_record(BINDING), product_rerun=False)
    try:
        cli, plan, context = authenticate(CLI, "RuntimeError('missing/extra/ignored Nextest terminal identities')")
        out = Path(plan['output'])
        inv_result = json_read(HERE / 'guest-controls-run-1/original-kvm-cli-inventory/result.json')
        require(inv_result['accepted'], 'actual inventory was not accepted')
        check_file(inv_result['readback']['stdout'])
        names = plan['phase_bindings']['selected_names']
        inv = inventory(Path(inv_result['readback']['stdout']['path']), selected_names=names)
        require(inv['identities'] == inv_result['readback']['identities'], 'actual inventory identities differ')
        events = [json.loads(line) for line in read(out / 'stdout').splitlines() if line]
        evidence = selected_events(events, names, inv['identities'])
        source = read(REPO / 'hermit-cli/tests/cli.rs').decode()
        require('#[ignore = "requires the pinned-root isolation validation node and its /test marker"]\nfn run_dbt_verifies_fresh_physical_workdirs()' in source, 'ignored source annotation differs')
        try: nextest_events(read(out / 'stdout'), names, 'hermit', 'cli')
        except RuntimeError as error: require(str(error) == 'missing/extra/ignored Nextest terminal identities', 'original reader refusal changed')
        else: raise RuntimeError('original reader unexpectedly accepted')
        evidence['negative_reader_controls'] = reader_controls(events, names, inv['identities'])
        admission = json_read(out / 'admission.json')
        require(admission['kvm_required'] is True and admission['kvm']['api_version'] == 12, 'actual KVM admission absent')
        evidence['admission'] = file_record(out / 'admission.json')
        evidence['stdout'] = file_record(out / 'stdout'); evidence['stderr'] = file_record(out / 'stderr')
        evidence['warnings'] = [line for line in read(out / 'stderr').decode().splitlines() if line.startswith('warning:')]
        require(len(evidence['warnings']) == 30 and len(set(evidence['warnings'])) == 1, 'retained warning population differs')
        cli['readback'] = evidence
        typed, writer_plan, writer_context = authenticate(TYPED, "RuntimeError('original tests remain failed')")
        typed_out = Path(writer_plan['output'])
        counts = json_read(typed_out / 'counts.json'); cpu = json_read(typed_out / 'cpu.json')
        expected = {'hermit::cli$' + name for name in names}
        require(counts['schema'] == 2 and counts['executed_tests'] == 24 and counts['filtered_tests'] == 89, 'typed suite counts differ')
        rows = counts['results']; require(len(rows) == 24 and {r['id'] for r in rows} == expected and all(r['attempts'] == 1 and r['result'] == 'pass' for r in rows), 'typed terminal identity/status/retry differs')
        attempts = cpu['attempts']; require(cpu['schema'] == 3 and len(attempts) == 24, 'typed CPU population differs')
        require({r['identity']['test'] for r in attempts} == set(names), 'CPU selected identity differs')
        paths = list((out / 'attempts').iterdir()); require(len(paths) == 24, 'raw CPU attempt count differs')
        for row in attempts:
            require(row['identity'] == dict(package='hermit', binary='hermit::cli', test=row['identity']['test'], attempt=1)
                    and row['run_id'] == cpu['run_id'] and row['completion'] == dict(kind='exit', code=0), 'CPU attempt identity/status differs')
            require(row['cpu_source'] == 'wait4-subtree' and row['cpu_usage_usec'] >= 0, 'CPU provenance differs')
            path = out / 'attempts' / (row['key'] + '.json')
            require(path in paths and json_read(path) == row, 'typed CPU differs from original raw attempt')
        typed['readback'] = dict(production_counts=counts, production_cpu=cpu, original_raw_status=0,
                                stdout=file_record(typed_out / 'stdout'), stderr=file_record(typed_out / 'stderr'))
        for receipt, actual_plan, actual_context in [(cli, plan, context), (typed, writer_plan, writer_context)]:
            check_source(actual_context); scm_readback(actual_context, D / receipt['phase'] / 'scm-after', actual_plan['environment'])
            receipt.update(accepted=True, final_source_inputs_unchanged=True)
            write_new(D / receipt['phase'] / 'result.json', receipt)
        for record in binding['inputs']: check_file(record)
        report.update(accepted=True, cli=file_record(D / CLI / 'result.json'), typed=file_record(D / TYPED / 'result.json'), original_refusals_preserved=True)
    except BaseException as error:
        report['error'] = repr(error)
    finally: write_new(D / 'RESULT.json', report)
    print(file_record(D / 'RESULT.json'))
    return 0 if report['accepted'] else 1


if __name__ == '__main__': raise SystemExit(main())
