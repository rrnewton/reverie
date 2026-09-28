from pathlib import Path
import ast
import hashlib
import json
import runpy

reverie = Path.cwd()
area = reverie / 'ignored/prejoin-failure-implementation-20260917'
hermit = reverie.parent / 'kvm-prejoin-hermit-20260917'
hermit_area = hermit / 'ignored/prejoin-failure-implementation-20260917'


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def bind(path):
    path = Path(path)
    info = path.stat()
    return dict(path=str(path), resolved_path=str(path.resolve(strict=True)),
                bytes=info.st_size, mode=info.st_mode & 0o7777, sha256=digest(path))


old = area / 'qualification-build-v2'
new = area / 'qualification-build-v3'
new.mkdir()
plan = json.loads((old / 'plan.json').read_text())
for row in plan['inputs']:
    path = row['path'].replace('source-v14-preparation', 'source-v16-preparation')
    current = bind(path)
    if path == row['path']:
        assert current == row, path
    row.clear()
    row.update(current)
for key in ['source_binding', 'source_manifest']:
    plan[key] = plan[key].replace('source-v14-preparation', 'source-v16-preparation')
for key in ['run_root', 'observer_root', 'tmpdir']:
    plan[key] = plan[key].replace('qualification-build-v2', 'qualification-build-v3')
plan['environment_fixed']['TMPDIR'] = plan['environment_fixed']['TMPDIR'].replace('qualification-build-v2', 'qualification-build-v3')
plan['execution'] = ['/usr/bin/python3', '-B', str(new / 'build.py')]
prior_native = json.loads((area / 'cargo-v9/plan.json').read_text())['selected_tests']
current_native = json.loads((area / 'cargo-v10/plan.json').read_text())['selected_tests']
vm_tests = sorted(set(plan['expected_tests']['lib']) - set(prior_native))
assert len(vm_tests) == 4
plan['expected_tests']['lib'] = sorted(current_native + vm_tests)
assert len(set(plan['expected_tests']['lib'])) == 43
assert len(plan['expected_tests']['static-elf']) == 22
plan['scope'] = 'Compile the frozen source v16 library and unchanged static_elf target, then record two actual inventories; all 39 native, four VM and 22 static identities remain required. No test execution.'
for name in ['cargo-v10/RESULT.json', 'cargo-v10/REPORT.md', 'cargo-v10/run-1/retained-elf-v16/lib',
             'cargo-v10/run-1/retained-elf-v16/binding.json', 'lint-v7/REPORT.md', 'lint-v7/run-1/summary.json']:
    plan['inputs'].append(bind(area / name))
for stage in plan['stages']:
    stage['out'] = stage['out'].replace('qualification-build-v2', 'qualification-build-v3')
    stage['argv'] = [value.replace('qualification-build-v2', 'qualification-build-v3') for value in stage['argv']]
    assert stage['argv'][11:] == stage['payload']
for key in ['run_root', 'observer_root']:
    assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
(new / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
script = (old / 'build.py').read_text().replace(digest(old / 'plan.json'), digest(new / 'plan.json'))
assert script.count("len(plan['expected_tests']['lib']) == 41") == 1
script = script.replace("len(plan['expected_tests']['lib']) == 41", "len(plan['expected_tests']['lib']) == 43")
ast.parse(script)
(new / 'build.py').write_text(script)
functions = runpy.run_path(plan['helpers']['path'], run_name='preflight_only')
functions['check_inputs'](plan)
record = dict(plan=bind(new / 'plan.json'), caller=bind(new / 'build.py'),
              execution=plan['execution'], outputs={stage['name']: stage['out'] for stage in plan['stages']},
              scope='Preparation only; no execution. Same compile and two inventory commands/bounds.')
(new / 'reservation.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))

old = hermit_area / 'integration-v6'
new = hermit_area / 'integration-v7'
new.mkdir()
plan = json.loads((old / 'build-plan.json').read_text())


def update_path(path):
    if str(hermit) in path:
        return path.replace('/source-v3/', '/source-v9/')
    if str(reverie) in path:
        return path.replace('/source-v9-preparation/', '/source-v16-preparation/')
    return path


for row in plan['inputs']:
    path = update_path(row['path'])
    current = bind(path)
    if path == row['path']:
        assert current == row, path
    row.clear()
    row.update(current)
for repo in plan['repositories']:
    for key in ['source_binding', 'source_manifest']:
        repo[key] = update_path(repo[key])
plan['status'] = 'Prepared explicit Hermit source v9 and Reverie source v16 local integration; original 51 + 9 native selectors and all limits unchanged.'
plan['source_pairing'] = dict(
    hermit_binding=bind(hermit_area / 'source-v9/binding.json'),
    reverie_binding=bind(area / 'source-v16-preparation/binding.json'),
    historical_relation='The preserved Hermit v9 binding names Reverie v15 from preparation time. This plan explicitly binds v16; the sole v15-to-v16 change is the reviewed authoritative-context selection/control correction. The historical binding is not rewritten.')
for key in ['run_root', 'observer_root', 'tmpdir']:
    plan[key] = plan[key].replace('integration-v6', 'integration-v7')
plan['environment_fixed']['TMPDIR'] = plan['environment_fixed']['TMPDIR'].replace('integration-v6', 'integration-v7')
plan['execution'] = ['/usr/bin/python3', '-B', str(new / 'build.py')]
plan['target_cache_provenance'] = 'Normal reuse of owned target/prejoin-integration-v5; all prior actual records remain. Cache files are transient build outputs, not retained evidence. The actual compiler-emitted executables are individually bound and checked between stages.'
for stage in plan['stages']:
    stage['out'] = stage['out'].replace('integration-v6', 'integration-v7')
    stage['argv'] = [value.replace('integration-v6', 'integration-v7') for value in stage['argv']]
    assert stage['argv'][11:] == stage['payload']
for key in ['run_root', 'observer_root']:
    assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
(new / 'build-plan.json').write_text(json.dumps(plan, indent=2) + '\n')
script = (old / 'build.py').read_text().replace(digest(old / 'build-plan.json'), digest(new / 'build-plan.json'))
ast.parse(script)
(new / 'build.py').write_text(script)
functions = runpy.run_path(plan['helpers']['path'], run_name='preflight_only')
for repo in plan['repositories']:
    functions['check_inputs'](dict(repo, inputs=plan['inputs'], optional_cargo_configs=plan['optional_cargo_configs']))
record = dict(plan=bind(new / 'build-plan.json'), caller=bind(new / 'build.py'),
              execution=plan['execution'], outputs={stage['name']: stage['out'] for stage in plan['stages']},
              scope='Preparation only; no execution. Same compile, six actual inventories and 51 + 9 native methods.')
(new / 'reservation.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
