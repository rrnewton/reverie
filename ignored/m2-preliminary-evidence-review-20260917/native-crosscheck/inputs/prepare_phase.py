#!/usr/bin/python3
"""Resolve one frozen phase into a fresh concrete plan; never launches it."""
import argparse
import copy
import re
from pathlib import Path
from common import HERE, REPO, check_file, check_source, file_record, json_read, owned, require, write_new

TEMPLATES = {
 'integration': (HERE / 'sequence-template.json', 'e328de2997d1a5c929464cbc02eb983c31be46d337dcc66a45cbb0957ff89d0b', 'phase_order'),
 'guest': (REPO / 'ignored/prejoin-main-guest-plan-v1/guest-sequence-template.json', 'e5ecbe441fd16a672205da977500834e7aa38d31ac28edba4c1323068a900cd1', 'phases'),
}
KVM_PHASES = {'initialized-vm', 'original-kvm-cli', 'original-pthread', 'pthread-canonical-kvm'}


def reader_for(name):
    if name in ('native-m2-detcore', 'native-m2-hermit-reports', 'native-m2-hermit-comparators'):
        return 'native'
    if name in ('compile-native', 'compile-hardware', 'manifest-controls-compile'):
        return 'compile'
    if name.startswith(('list-', 'hardware-list-')) or name == 'manifest-controls-list':
        return 'list'
    if name in ('native-detcore', 'native-hermit', 'initialized-vm', 'manifest-controls'):
        return 'native'
    if name.startswith('inventory-') or name.endswith('-inventory'):
        return 'inventory'
    if name in ('original-kvm-cli', 'original-pthread'):
        return 'nextest'
    if name.endswith('-typed-results'):
        return 'typed-nextest'
    if name.startswith('pthread-canonical-'):
        return 'typed-canonical' if name.endswith('-typed-read') else 'canonical'
    if name.startswith('metadata-'):
        return 'metadata'
    if name == 'hardware-binaries-metadata':
        return 'binaries'
    if name == 'normal-cpu-wrapper-build':
        return 'cpu-wrapper'
    if name in ('normal-nextest-config', 'official-generator-write') or name.endswith('-binary-map'):
        return 'output-files'
    require(name in ('pin-offline', 'format', 'official-generator-check', 'workspace-clippy', 'final-format'), 'unknown plain phase')
    return 'plain'


def resolve(value, aliases, out):
    if isinstance(value, list):
        result = []
        for item in value:
            replacement = resolve(item, aliases, out)
            result.extend(replacement if isinstance(replacement, list) else [replacement])
        return result
    if isinstance(value, dict):
        return {key: resolve(item, aliases, out) for key, item in value.items()}
    if not isinstance(value, str):
        return value
    if value == 'OWNED_OBSERVER_STEP_OUT' or value.startswith('OWNED_OBSERVER_STEP_OUT/'):
        return str(out) + value[len('OWNED_OBSERVER_STEP_OUT'):]
    if value in aliases:
        replacement = aliases[value]
        require(isinstance(replacement, (str, list)) and replacement, 'empty/unresolved alias: ' + value)
        words=replacement if isinstance(replacement,list) else [replacement]
        require(all(isinstance(word,str) and not word.startswith(('ACTUAL_','OWNED_FRESH_','OWNED_GENERATED_','OWNED_OBSERVER_STEP_OUT')) for word in words), 'alias still contains a preparation marker: '+value)
        return replacement
    require(not value.startswith(('ACTUAL_', 'OWNED_FRESH_', 'OWNED_GENERATED_')), 'unresolved alias: ' + value)
    return value


def prepare(context_path, group, phase_name, out):
    context = json_read(context_path)
    require(context.get('status') == 'final-bound', 'context is preparation only')
    require(context.get('repository') == str(REPO), 'different product repository')
    sha = context.get('qualified_forward_reverie_sha')
    require(isinstance(sha, str) and re.fullmatch('[0-9a-f]{40}', sha), 'forward revision unresolved')
    require(sha != '30fee360d6359e38a429f6bf19d30cdfac9c1d0d', 'old pin is not the qualified successor')
    check_file(context['forward_revision_evidence'])
    for item in context['caller_files']:
        check_file(item)
    require({Path(item['path']).name for item in context['caller_files']} ==
            {'common.py', 'prepare_phase.py', 'run_phase.py', 'admit.py', 'readback.py'}, 'incomplete caller binding')
    check_source(context)
    template_path, expected, key = TEMPLATES[group]
    template_record = file_record(template_path)
    require(template_record['sha256'] == expected, 'frozen template changed')
    template = json_read(template_path)
    matches = [p for p in template[key] if p['name'] == phase_name]
    require(len(matches) == 1, 'phase name does not uniquely select the frozen template')
    original = matches[0]
    require('argv_template' in original, 'source transitions are explicit and never performed by this caller')
    out = owned(Path(out), HERE / 'observer')
    require(not out.exists(), 'output already exists; retain the first attempt')
    phase = copy.deepcopy(original)
    aliases = context['aliases']
    argv = resolve(phase['argv_template'], aliases, out)
    adaptations = []
    if phase_name == 'initialized-vm':
        # The previous helper only accepts libtest. The same owned service admission
        # now validates the full concrete payload vector for libtest and Nextest.
        require(argv[:2] == ['/usr/bin/python3', '-B'] and '--executable' in argv, 'old initialized-VM template changed')
        executable = argv[argv.index('--executable') + 1]
        separator = argv.index('--')
        argv = [executable, *argv[separator + 1:]]
        adaptations.append('Replace only the old admission wrapper with the common owned admission path; libtest arguments unchanged.')
    if phase_name == 'pin-offline':
        # The current checker queries main before its --offline branch. Preserve
        # its exact checker arguments and provide its normal proxy transport.
        require(argv == [str(REPO / 'ci/run-reverie-pin-check.sh'), '--offline'], 'pin-check template changed')
        argv = ['/usr/bin/with-proxy', *argv]
        adaptations.append('Provide normal proxy transport for the current checker, whose --offline branch follows query_main.')
    if phase_name == 'official-generator-write':
        require(argv[-2] == '--write', 'generator output template changed')
        argv[-1] = str(out / 'validate.json')
        adaptations.append('Place new generated candidate below the current observer output; preserve the frozen old packet.')
    require(argv and Path(argv[0]).is_absolute(), 'payload must name an absolute actual program')
    phase['argv'] = argv
    phase.pop('argv_template')
    for key2 in ('aggregate_cpu_usec', 'wall_seconds', 'memory_bytes', 'swap_bytes', 'lethal_stderr_bytes', 'postread_limit_per_stream_bytes'):
        require(key2 in phase and type(phase[key2]) is int, 'missing phase bound: ' + key2)
    require(phase['memory_bytes'] == 16 * 1024**3 and phase['swap_bytes'] == 0, 'changed memory/swap bound')
    environment = resolve(context['environment'], aliases, out)
    environment.update(resolve(original.get('environment', {}), aliases, out))
    require(all(isinstance(key,str) and isinstance(value,str) for key,value in environment.items()), 'complete environment is unresolved')
    require(environment['CARGO_TARGET_DIR'] == context['target_cache']['path'], 'target differs from owned cache')
    require(environment['CARGO_NET_OFFLINE'] == 'true' and environment['CARGO_BUILD_JOBS'] == '2', 'build policy changed')
    selected = context['phase_bindings'].get(phase_name)
    require(isinstance(selected, dict), 'phase-specific result/artifact bindings unresolved')
    require(selected.get('ready') is True, 'phase-specific bindings not final')
    require(selected['reader'] == reader_for(phase_name), 'reader cannot weaken this phase')
    if 'selected_names' in original:
        require(selected['selected_names'] == original['selected_names'], 'original selected names changed')
    if phase_name in ('native-detcore', 'native-hermit'):
        require(selected['selected_names'] == original['argv_template'][2:-2], 'native control cohort changed')
    if phase_name == 'initialized-vm':
        require(selected['selected_names'] == [original['argv_template'][-1]], 'initialized VM method changed')
    for record in selected.get('inputs', []):
        check_file(record)
    if phase_name in ('original-kvm-cli', 'original-pthread'):
        require((selected['package'],selected['binary']) == ('hermit','cli' if phase_name=='original-kvm-cli' else 'kvm_harder'), 'original suite identity changed')
        require(selected['test_embedded_hermit'], 'actual compile-time Hermit program binding missing')
    # Dependencies are exact retained results, including source-transition records.
    phase_index=template[key].index(original)
    predecessor=template[key][phase_index-1]['name'] if phase_index else ('final-format' if group=='guest' else None)
    seen_predecessor=predecessor is None
    for dependency in selected['dependencies']:
        check_file(dependency['record'])
        value = json_read(dependency['record']['path'])
        if value.get('phase') == predecessor:
            seen_predecessor=True
        if dependency.get('allow_terminal_failure_for_typed_writer'):
            require(phase_name in ('original-kvm-cli-typed-results', 'original-pthread-typed-results')
                    and value.get('terminal_authenticated') is True, 'only the production result writer follows a failed test')
        else:
            require(value.get('accepted') is True, 'an earlier phase did not pass')
    require(seen_predecessor, 'immediate prior phase/source transition has no retained result')
    return dict(schema=1, group=group, phase_name=phase_name, context=file_record(context_path),
                frozen_template=template_record, phase=phase, environment=environment,
                output=str(out), admission=phase_name in KVM_PHASES,
                phase_bindings=selected, adaptations=adaptations,
                validation_scope='Actual selected phase only; inventory is not execution, native is not guest, per-backend repeat is not cross-backend INFO parity.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('context'); parser.add_argument('group', choices=sorted(TEMPLATES))
    parser.add_argument('phase'); parser.add_argument('out'); parser.add_argument('plan')
    args = parser.parse_args()
    plan = prepare(Path(args.context), args.group, args.phase, Path(args.out))
    write_new(owned(Path(args.plan)), plan)


if __name__ == '__main__':
    main()
