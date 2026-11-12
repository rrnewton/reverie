from pathlib import Path
import ast
import hashlib
import json
import runpy
import sys

root = Path.cwd()
area = root / 'ignored/prejoin-failure-implementation-20260917'
out = area / 'qualification-execution-v3'
finalize = sys.argv[1:] == ['--bind-actual-artifacts']
assert sys.argv[1:] in [[], ['--bind-actual-artifacts']]


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


build = area / 'qualification-build-v4'
original = json.loads((build / 'plan.json').read_text())
native = json.loads((area / 'cargo-v13/plan.json').read_text())['selected_tests']
vm_tests = sorted(set(original['expected_tests']['lib']) - set(native))
static_tests = original['expected_tests']['static-elf']
assert len(vm_tests) == 4 and len(static_tests) == 22
helper = Path(original['helpers']['path'])
admit = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-hermit-20260917/ignored/prejoin-failure-implementation-20260917/hardware-qualification-helper-v1/admit.py')
assert digest(admit) == '9a33e8682509361f00c627e30004f9139a4de79167fb0f35f41ce727bf2996d3'
inputs = list(original['inputs'])
inputs.append(bind(area / 'cargo-v10/plan.json'))
inputs.append(bind(area / 'cargo-v13/plan.json'))
inputs.append(bind(area / 'qualification-build-v3/run-1/list-lib-inventory.json'))
inputs.append(bind(area / 'qualification-build-v3/run-1/list-static-elf-inventory.json'))
for path in [admit, build / 'build.py', build / 'plan.json', '/usr/bin/timeout']:
    inputs.append(bind(path))
run_root = out / 'run-1'
observer_root = Path(original['observer_root']).parent / 'reverie-qualification-execution-v3'
environment = dict(original['environment_fixed'], TMPDIR=str(run_root / 'tmp'), REVERIE_REQUIRE_KVM='1')
artifacts = {name: dict(path='<actual-' + name + '-executable>',
                      sha256='<actual-' + name + '-sha256>', bytes=None, mode=None)
             for name in ['lib', 'static-elf']}
inventories = {name: dict(path=str(build / 'run-1' / ('list-' + name + '-inventory.json')),
                         count=count) for name, count in [('lib', 449), ('static-elf', 288)]}
if finalize:
    assert out.is_dir() and not (out / 'plan.json').exists()
    summary = json.loads((build / 'run-1/summary.json').read_text())
    assert summary['status'] == 'passed'
    artifacts = json.loads((build / 'run-1/compiled-executables.json').read_text())
    assert set(artifacts) == {'lib', 'static-elf'}
    for name in artifacts:
        actual = summary['inventories'][name]
        previous = json.loads((area / 'qualification-build-v3/run-1' /
                              ('list-' + name + '-inventory.json')).read_text())
        if name == 'lib':
            additions = set(native) - set(json.loads((area / 'cargo-v10/plan.json').read_text())['selected_tests'])
            assert len(additions) == 1 and not (additions & set(previous['names']))
            expected = sorted(previous['names'] + list(additions))
        else:
            expected = previous['names']
        assert actual['names'] == expected and actual['count'] == inventories[name]['count']
        inputs.extend([bind(artifacts[name]['path']), bind(inventories[name]['path'])])
    for name in ['summary.json', 'compiled-executables.json', 'launch.json',
                 'compile-readback.json', 'list-lib-readback.json', 'list-static-elf-readback.json']:
        inputs.append(bind(build / 'run-1' / name))
else:
    out.mkdir()
plan = {key: original[key] for key in [
    'schema', 'source_root', 'source_base', 'source_head', 'source_binding', 'source_manifest',
    'observer', 'observer_sha256', 'helpers', 'environment_keys', 'service_memory_max_bytes',
    'service_swap_max_bytes', 'optional_cargo_configs', 'locked_dependency_file']}
plan.update(inputs=inputs, environment_fixed=environment, run_root=str(run_root),
            observer_root=str(observer_root), tmpdir=environment['TMPDIR'], artifacts=artifacts,
            inventories=inventories, selected_tests={'lib': vm_tests, 'static-elf': static_tests},
            admission_helper=str(admit), execution=['/usr/bin/python3', '-B', str(out / 'launch.py')],
            scope='Four exact library VM controls and all 22 unchanged static_elf lifecycle methods. The exec-worker method retains all ten modes. Real KVM admission occurs inside each actual observed service; no skip is qualification. This is Reverie execution evidence, not Hermit/Detcore guest or canonical parity evidence.')
stages = []
for kind, tests in [('lib', vm_tests), ('static-elf', static_tests)]:
    for index, test in enumerate(tests, 1):
        name = ('vm' if kind == 'lib' else 'static-elf') + '-' + str(index).zfill(2)
        artifact = artifacts[kind]
        admission = str(run_root / 'admissions' / (name + '.json'))
        payload = ['/usr/bin/python3', '-B', str(admit), '--record', admission,
                   '--executable', artifact['path'], '--sha256', artifact['sha256'], '--',
                   '--exact', '--nocapture', '--test-threads', '1', test]
        dest = str(observer_root / name)
        argv = ['/usr/bin/python3', '-B', original['observer'], '--out', dest,
                '--cpu-usec', '30000000', '--wall-seconds', '60', '--log-bytes', '1048576'] + payload
        stages.append(dict(name=name, artifact=kind, test=test, payload=payload, argv=argv,
                           out=dest, cwd=str(root), cpu_usec=30000000, wall_seconds=60,
                           stderr_limit_bytes=1048576, reader_limit_bytes=1048576,
                           admission_record=admission,
                           expected_summaries=2 if test.startswith('terminal_fork::') else 1,
                           environment_overrides={'TMPDIR': str(run_root / 'tmp' / name)}))
plan['stages'] = stages
assert len(stages) == 26 and all(step['argv'][11:] == step['payload'] for step in stages)
assert not run_root.exists() and not observer_root.exists()
functions = runpy.run_path(str(helper), run_name='reviewed_helpers_only')
functions['check_inputs'](plan)
if finalize:
    for artifact in artifacts.values():
        functions['check_executable'](artifact)
    path = out / 'plan.json'
    with path.open('x') as target:
        json.dump(plan, target, indent=2)
        target.write('\n')
    template = (area / 'qualification-execution-caller-v1.py.in').read_text()
    code = template.replace('<bound-plan-sha256>', digest(path))
    ast.parse(code)
    with (out / 'launch.py').open('x') as target:
        target.write(code)
    print(json.dumps({'plan': bind(path), 'caller': bind(out / 'launch.py')}, indent=2))
else:
    with (out / 'plan.pending-artifacts.json').open('x') as target:
        json.dump(plan, target, indent=2)
        target.write('\n')
    print(json.dumps({'plan': bind(out / 'plan.pending-artifacts.json'),
                      'caller_template': bind(area / 'qualification-execution-caller-v1.py.in'),
                      'scope': plan['scope']}, indent=2))
