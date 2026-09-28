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


old = area / 'qualification-build-v4'
new = area / 'qualification-build-v5'
new.mkdir()
plan = json.loads((old / 'plan.json').read_text())
for row in plan['inputs']:
    path = row['path'].replace('source-v19-preparation', 'source-v22-preparation')
    current = bind(path)
    if path == row['path']:
        assert current == row, path
    row.clear()
    row.update(current)
for key in ['source_binding', 'source_manifest']:
    plan[key] = plan[key].replace('source-v19-preparation', 'source-v22-preparation')
for key in ['run_root', 'observer_root', 'tmpdir']:
    plan[key] = plan[key].replace('qualification-build-v4', 'qualification-build-v5')
plan['environment_fixed']['TMPDIR'] = plan['environment_fixed']['TMPDIR'].replace('qualification-build-v4', 'qualification-build-v5')
plan['execution'] = ['/usr/bin/python3', '-B', str(new / 'build.py')]
prior_native = json.loads((area / 'cargo-v13/plan.json').read_text())['selected_tests']
current_native = json.loads((area / 'cargo-v16/plan.json').read_text())['selected_tests']
vm_tests = sorted(set(plan['expected_tests']['lib']) - set(prior_native))
assert len(vm_tests) == 4
plan['expected_tests']['lib'] = sorted(current_native + vm_tests)
assert len(set(plan['expected_tests']['lib'])) == 48
assert len(plan['expected_tests']['static-elf']) == 22
plan['scope'] = 'Compile the frozen source v22 library and unchanged static_elf target, then record two actual inventories; all 44 native, four VM and 22 static identities remain required. No test execution.'
for name in ['cargo-v16/RESULT.json', 'cargo-v16/REPORT.md', 'cargo-v16/run-1/retained-elf-v22/lib',
             'cargo-v16/run-1/retained-elf-v22/binding.json', 'lint-v13/REPORT.md', 'lint-v13/run-1/summary.json']:
    plan['inputs'].append(bind(area / name))
for stage in plan['stages']:
    stage['out'] = stage['out'].replace('qualification-build-v4', 'qualification-build-v5')
    stage['argv'] = [value.replace('qualification-build-v4', 'qualification-build-v5') for value in stage['argv']]
    assert stage['argv'][11:] == stage['payload']
for key in ['run_root', 'observer_root']:
    assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
(new / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
script = (old / 'build.py').read_text().replace(digest(old / 'plan.json'), digest(new / 'plan.json'))
assert script.count("len(plan['expected_tests']['lib']) == 44") == 1
script = script.replace("len(plan['expected_tests']['lib']) == 44", "len(plan['expected_tests']['lib']) == 48")
ast.parse(script)
(new / 'build.py').write_text(script)
functions = runpy.run_path(plan['helpers']['path'], run_name='preflight_only')
functions['check_inputs'](plan)
record = dict(plan=bind(new / 'plan.json'), caller=bind(new / 'build.py'),
              execution=plan['execution'], outputs={stage['name']: stage['out'] for stage in plan['stages']},
              scope='Preparation only; no execution. Same compile and two inventory commands/bounds.')
(new / 'reservation.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
