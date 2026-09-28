"""Source snapshot, executable retention, and report generation; no payload launches."""

import argparse
import json
import os
from pathlib import Path
import shutil

from support import OUT, ROOT, TARGET
from support import identity, load_bound, load_source, local_path
from support import source_identity, validate_plan, write_json


def snapshot(args, plan):
    destination = local_path(args.name)
    before = source_identity(plan)
    if args.after_format:
        phase = local_path(args.after_format)
        result = json.loads((phase / 'result.json').read_text())
        command = json.loads((phase / 'command.json').read_text())
        if (not result['accepted'] or result['source_after'] != before or
                command['plan_binding']['sha256'] != args.plan_sha256 or
                args.after_format != 'format-1'):
            raise ValueError('formatted source does not match the accepted phase')
    destination.mkdir()
    for name in plan['source_paths']:
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, path)
        if identity(path) != before[name]:
            raise ValueError('source changed during snapshot: ' + name)
    if source_identity(plan) != before:
        raise ValueError('source changed while snapshot was prepared')
    manifest = local_path(args.name + '.json')
    write_json(manifest, {
        'source': before, 'snapshot_directory': args.name,
        'plan_sha256': args.plan_sha256, 'after_format': args.after_format,
        'dispatched_base': plan['dispatch_base'],
        'scope': 'Explicitly named source/lock files; not a full transitive input closure'})
    print(json.dumps({'manifest': str(manifest), **identity(manifest)}))


def retain(args, plan):
    load_source(args.source, args.source_sha256, plan, args.plan_sha256)
    phase = OUT / 'compile-1'
    result = json.loads((phase / 'result.json').read_text())
    command = json.loads((phase / 'command.json').read_text())
    if (not result['accepted'] or command['source_binding']['sha256'] != args.source_sha256 or
            command['plan_binding']['sha256'] != args.plan_sha256):
        raise ValueError('compile was not accepted for this source and plan')
    artifacts = []
    for line in (phase / 'stdout').read_text().splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get('reason') == 'compiler-artifact' and
                item.get('target', {}).get('name') == 'reverie_kvm' and
                item.get('target', {}).get('kind') == ['lib'] and
                item.get('profile', {}).get('test') is True and item.get('executable') and
                item.get('manifest_path') == str(ROOT / 'reverie-kvm/Cargo.toml')):
            artifacts.append(item)
    if len(artifacts) != 1:
        raise ValueError('expected exactly one reverie-kvm library test executable')
    artifact = artifacts[0]
    if 'native-test-support' not in artifact['features']:
        raise ValueError('compiled artifact lacks admitted feature')
    built = Path(artifact['executable']).resolve()
    if not built.is_relative_to(TARGET):
        raise ValueError('compiled artifact lies outside the admitted target directory')
    before = identity(built)
    with built.open('rb') as stream:
        if stream.read(4) != b'\x7fELF':
            raise ValueError('compiler artifact is not an ELF')
    retained = OUT / 'retained-lib-1'
    with retained.open('xb') as output, built.open('rb') as source:
        shutil.copyfileobj(source, output)
    os.chmod(retained, 0o555)
    if identity(retained) != before or identity(built) != before:
        raise ValueError('executable identity changed during retention')
    binding = {
        'path': str(retained), 'built_path': str(built), 'executable': before,
        'mode': '0555', 'artifact': artifact, 'source_sha256': args.source_sha256,
        'plan_sha256': args.plan_sha256,
        'compile_records': {name: identity(phase / name)
                            for name in ['command.json', 'RAW-RESULT.json', 'result.json', 'stdout', 'stderr']}}
    write_json(OUT / 'EXECUTABLE-1.json', binding)
    print(json.dumps({'binding': 'EXECUTABLE-1.json', **identity(OUT / 'EXECUTABLE-1.json')}))


def report(args, plan):
    source, _ = load_source(args.source, args.source_sha256, plan, args.plan_sha256)
    phases = {}
    for name in plan['phases']:
        phase = OUT / name
        result = json.loads((phase / 'result.json').read_text())
        command = json.loads((phase / 'command.json').read_text())
        if command['plan_binding']['sha256'] != args.plan_sha256:
            raise ValueError('phase plan mismatch: ' + name)
        if name != 'format-1' and command['source_binding']['sha256'] != args.source_sha256:
            raise ValueError('phase source mismatch: ' + name)
        if result.get('source_after') != source['source']:
            raise ValueError('phase final source mismatch: ' + name)
        for stream in ['stdout', 'stderr']:
            if identity(phase / stream) != result['stream_records'][stream]:
                raise ValueError('retained phase stream changed: ' + name)
        phases[name] = result
    selected = sum(group['expected_passed'] for group in plan['tests'])
    passed = sum(group['expected_passed'] for group in plan['tests']
                 if phases[group['phase']]['accepted'])
    complete = all(result['accepted'] for result in phases.values())
    final = OUT / 'final-1'
    final.mkdir()
    write_json(final / 'RESULTS.json', {
        'all_phases_accepted': complete, 'selected_declarations': selected,
        'passed_declarations_from_accepted_phases': passed, 'phases': phases,
        'source_sha256': args.source_sha256, 'plan_sha256': args.plan_sha256})
    rows = '\n'.join(
        f'| {name} | {result["raw_status"]} | {result["accepted"]} | {result["elapsed_seconds"]:.3f} |'
        for name, result in phases.items())
    text = f'''Private panic consumer composition evidence

All phases accepted: {complete}. Exact selected declarations: {selected}; declarations in accepted test phases: {passed}. Inventory is discovery, not execution. Every test phase requires its exact passed count, zero failed, and zero ignored. Per-phase raw statuses and refusals remain in RESULTS.json and the individual result.json files.

| Phase | Raw status | Accepted | Wall seconds |
| --- | --- | --- | --- |
{rows}

Source manifest SHA256 {args.source_sha256}; plan SHA256 {args.plan_sha256}. The manifest binds the explicitly listed source/lock records, not a full transitive input closure. Compiler artifacts, retained 0555 executable, inventory and each launch's executable/source/tool identities are separately recorded. Some selected controls execute KVM_RUN using existing minimal guest fixtures; the total ioctl count is not measured. Other controls use only KVM setup and host futures. No Hermit/Detcore guest or cross-backend comparison is included, and no parity or landing approval follows from these controls.

The build uses locked offline Cargo with native-test-support, and the normal library cargo check does not set cfg(test). Systemd bounds are CPU 200%, memory 12 GiB, swap zero, core zero, with 600 seconds for compile/check, 60 for each test group and 30 for format/inventory. Each stdout and stderr stream is capped at 16 MiB; overflow, timeout, unresolved terminal service state, identity change or wrong selection is a refusal. Missing CPU accounting from a collected transient unit is not reported as zero CPU usage.

This packet records selected callback, consuming-hook, concrete-owner and child-join controls. It does not certify actual host-spawn refusal, every unexpected setup/report-hook unwind, externally cancelled pending futures, recursively panicking final payload destructors, or the external caller's global/scheduler cleanup after a propagated root panic. Production activation of the earlier entry-owner preparation and the remaining determinism/parity work are separate. Native and external code review still apply before landing.
'''
    (final / 'REPORT.md').write_text(text)
    paths = [local_path(args.plan), local_path(args.source), OUT / 'EXECUTABLE-1.json',
             OUT / 'retained-lib-1', OUT / 'inventory-1/INVENTORY.json']
    paths += [OUT / name for name in ['support.py', 'runner.py', 'packet.py']]
    paths += [OUT / source['snapshot_directory'] / name for name in plan['source_paths']]
    for name in plan['phases']:
        paths += [OUT / name / leaf for leaf in
                  ['command.json', 'RAW-RESULT.json', 'result.json', 'stdout', 'stderr']]
    paths += [final / 'RESULTS.json', final / 'REPORT.md']
    records = [{'path': str(path.relative_to(OUT)), **identity(path)} for path in paths]
    write_json(final / 'TARGET.json', {'scope': 'private source and host controls', 'records': records})
    print(json.dumps({'all_phases_accepted': complete, 'target': 'final-1/TARGET.json',
                      **identity(final / 'TARGET.json')}))
    return 0 if complete else 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['snapshot', 'retain', 'report'])
    parser.add_argument('--plan', required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--name')
    parser.add_argument('--after-format')
    parser.add_argument('--source')
    parser.add_argument('--source-sha256')
    args = parser.parse_args()
    plan, _ = load_bound(args.plan, args.plan_sha256)
    validate_plan(plan)
    if args.action == 'snapshot':
        if not args.name or args.source or args.source_sha256:
            raise ValueError('snapshot requires a new --name, without --source')
        snapshot(args, plan)
    elif not args.source or not args.source_sha256:
        raise ValueError('retain/report requires a bound source manifest')
    elif args.action == 'retain':
        retain(args, plan)
    else:
        return report(args, plan)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
