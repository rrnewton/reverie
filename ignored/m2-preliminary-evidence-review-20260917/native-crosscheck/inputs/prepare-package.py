#!/usr/bin/python3
"""Freeze native comparator cohorts from the actual six M2 ELF inventories."""
import copy
import hashlib
from pathlib import Path

from common import check_file, file_record, json_read, read, require, write_new

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
PREVIOUS = REPO / 'ignored/m2-callers-v1'
COHORTS = [
    ('native-m2-detcore', 'detcore', ['logdiff::']),
    ('native-m2-hermit-reports', 'hermit', ['canonical_verdict::', 'logdiff_report::']),
    ('native-m2-hermit-comparators', 'hermit-bin',
     ['verify::', 'run::', 'record_start::', 'logdiff::', 'backends::', 'analyze::phases::']),
]


def main():
    context_record = file_record(PREVIOUS / 'compile-native-context.json')
    context = json_read(context_record['path'])
    compiled_record = file_record(PREVIOUS / 'controls-run-1/compile-native/result.json')
    compiled = json_read(compiled_record['path'])
    require(compiled['accepted'] and compiled['final_source_inputs_unchanged'], 'compile not accepted')
    inventory_records, inventories = [], {}
    for key in ['detcore', 'syscaller', 'hermit', 'hermit-bin', 'hermit-dap', 'verification-report']:
        record = file_record(PREVIOUS / 'controls-run-1' / ('list-' + key) / 'result.json')
        result = json_read(record['path'])
        require(result['accepted'] and result['final_source_inputs_unchanged']
                and result['source'] == context['source_manifest'], 'actual list not accepted: ' + key)
        inventory_records.append(record)
        inventories[key] = result['readback']['actual_names']
        check_file(compiled['readback']['artifacts'][key]['retained'], executable=True)
    old_template = json_read(REPO / 'ignored/prejoin-main-integration-plan-v2/sequence-template.json')
    native = next(row for row in old_template['phase_order'] if row['name'] == 'native-hermit')
    phases, selected = [], {}
    for name, artifact_key, prefixes in COHORTS:
        population = inventories[artifact_key]
        names = sorted(test for test in population if any(test.startswith(prefix) for prefix in prefixes))
        require(names and len(names) == len(set(names)), 'empty or duplicate native cohort')
        require(all(any(test.startswith(prefix) for test in names) for prefix in prefixes),
                'an intended module is absent from the actual list')
        phase = copy.deepcopy(native)
        phase.update(name=name, selected_names=names, expected_selected=names,
                     argv_template=['ACTUAL_RETAINED_NATIVE_ELF:' + artifact_key, '--exact', *names,
                                    '--nocapture', '--test-threads=1'],
                     expected='Every exact selected native identity must pass once; actual complete ELF inventory supplies filtered count.')
        phases.append(phase)
        selected[name] = {'artifact_key': artifact_key, 'prefixes': prefixes, 'names': names,
                          'inventory_record': next(row for row in inventory_records
                                                   if Path(row['path']).parent.name == 'list-' + artifact_key)}
    require('logdiff::test::default_comparison_preserves_numeric_values' in selected['native-m2-detcore']['names'],
            'numeric-value control absent')
    require('canonical_verdict::tests::canonical_match_rejects_unequal_counts_without_rewriting_history'
            in selected['native-m2-hermit-reports']['names'], 'equal-count/history control absent')
    template = {'scope': 'Exact actual native modules for the original M2 comparator/report/default-policy increment',
                'phase_order': phases, 'source': context['source_manifest']}
    template_path = HERE / 'sequence-template.json'
    write_new(template_path, template)
    prepare_raw = read(PREVIOUS / 'prepare_phase.py').decode()
    old_line = " 'integration': (REPO / 'ignored/prejoin-main-integration-plan-v2/sequence-template.json', '63926a74389e1939e3fbaaf841d4214dc0951c3eaf4577d34fa59d04de83f379', 'phase_order'),"
    new_line = " 'integration': (HERE / 'sequence-template.json', '" + file_record(template_path)['sha256'] + "', 'phase_order'),"
    require(prepare_raw.count(old_line) == 1, 'old template source anchor changed')
    prepare_raw = prepare_raw.replace(old_line, new_line)
    anchor = 'def reader_for(name):\n'
    require(prepare_raw.count(anchor) == 1, 'reader source anchor changed')
    prepare_raw = prepare_raw.replace(anchor, anchor +
        "    if name in ('native-m2-detcore', 'native-m2-hermit-reports', 'native-m2-hermit-comparators'):\n        return 'native'\n")
    compile(prepare_raw, str(HERE / 'prepare_phase.py'), 'exec')
    with (HERE / 'prepare_phase.py').open('x') as output:
        output.write(prepare_raw)
    import difflib
    patch = ''.join(difflib.unified_diff(read(PREVIOUS / 'prepare_phase.py').decode().splitlines(True),
                                       prepare_raw.splitlines(True), fromfile='C1/prepare_phase.py',
                                       tofile='M2-controls/prepare_phase.py'))
    with (HERE / 'prepare-phase.patch').open('x') as output:
        output.write(patch)
    write_new(HERE / 'COHORTS.json', {
        'source': context['source_manifest'], 'context': context_record, 'compile': compiled_record,
        'all_inventory_records': inventory_records, 'selected': selected,
        'scope': 'Native method execution only. No VM, actual traced guest, shell-consumer or complete recurring-node claim.'})
    print({name: len(row['names']) for name, row in selected.items()})


if __name__ == '__main__':
    main()
