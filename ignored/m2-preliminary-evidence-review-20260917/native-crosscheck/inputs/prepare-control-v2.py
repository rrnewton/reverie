#!/usr/bin/python3
"""Resolve a source-bound native cohort through the unchanged safety checks."""
from pathlib import Path
import sys

from common import check_file, check_source, file_record, json_read, require, write_new
from prepare_phase import prepare
from refresh_systemd import refresh_systemd

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
PREVIOUS = REPO / 'ignored/m2-callers-v1'


def main():
    cohort_record = file_record(HERE / 'COHORTS.json')
    cohort = json_read(cohort_record['path'])
    name = sys.argv[1]
    require(name in cohort['selected'], 'unexpected cohort')
    context = json_read(cohort['context']['path'])
    compiled = json_read(cohort['compile']['path'])
    config = cohort['selected'][name]
    template = json_read(HERE / 'sequence-template.json')
    phases = [row['name'] for row in template['phase_order']]
    dependencies = [{'record': cohort['compile']},
                    *[{'record': record} for record in cohort['all_inventory_records']]]
    if phases.index(name):
        previous = file_record(HERE / 'controls-run-1' / phases[phases.index(name) - 1] / 'result.json')
        require(json_read(previous['path'])['accepted'], 'earlier native cohort failed')
        dependencies.append({'record': previous})
    artifact = compiled['readback']['artifacts'][config['artifact_key']]['retained']
    check_file(artifact, executable=True)
    executables = {row['path']: row for row in context['executables']}
    executables[artifact['path']] = artifact
    context['executables'] = list(executables.values())
    context['aliases']['ACTUAL_RETAINED_NATIVE_ELF:' + config['artifact_key']] = artifact['path']
    context['control_root'] = str(HERE / 'controls-run-1')
    context['caller_files'] = [file_record(HERE / filename) for filename in
                              ['common.py', 'prepare_phase.py', 'run_phase.py', 'admit.py', 'readback.py']]
    context['observer'] = file_record(HERE / 'observer/observer.py')
    loader_record = file_record(PREVIOUS / 'native-loader-inputs.json')
    loader = json_read(loader_record['path'])
    context['inputs'] += [cohort_record, cohort['context'], cohort['compile'],
                          *cohort['all_inventory_records'], loader_record, loader['ldd'], *loader['inputs'],
                          file_record(HERE / 'sequence-template.json'), file_record(HERE / 'prepare-package-v2.py'),
                          file_record(Path(__file__)), file_record(HERE / 'launch-controls-v2.sh'),
                          *[file_record(HERE / 'observer' / filename) for filename in
                            ['observer.py', 'before_exec.py', 'unit_reference.py', 'source-inputs.json']]]
    for row in loader['symlinks']:
        if row not in context['input_symlinks']:
            context['input_symlinks'].append(row)
    context['phase_bindings'][name] = {
        'ready': True, 'reader': 'native', 'selected_names': config['names'],
        'inventory_record': config['inventory_record'], 'inputs': [],
        'runtime_executables': [artifact], 'dependencies': dependencies}
    refresh_systemd(context, HERE / (name + '-systemd'))
    check_source(context)
    context_path = HERE / (name + '-context.json')
    write_new(context_path, context)
    plan = prepare(context_path, 'integration', name, HERE / 'observer' / name)
    plan_path = HERE / (name + '-plan.json')
    write_new(plan_path, plan)
    print(file_record(plan_path))


if __name__ == '__main__':
    main()
