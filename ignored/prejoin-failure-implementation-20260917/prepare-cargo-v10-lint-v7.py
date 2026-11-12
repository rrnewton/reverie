from pathlib import Path
import ast
import difflib
import hashlib
import json
import runpy

root = Path.cwd()
area = root / 'ignored/prejoin-failure-implementation-20260917'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def bind(path):
    path = Path(path)
    info = path.stat()
    return dict(path=str(path), resolved_path=str(path.resolve(strict=True)),
                bytes=info.st_size, mode=info.st_mode & 0o7777, sha256=digest(path))


def prepare(old_name, new_name, previous_source):
    old = area / old_name
    new = area / new_name
    new.mkdir()
    plan = json.loads((old / 'plan.json').read_text())
    for row in plan['inputs']:
        old_path = row['path']
        current_path = old_path.replace(previous_source, 'source-v16-preparation')
        current = bind(current_path)
        if current_path == old_path:
            assert current == row, old_path
        row.clear()
        row.update(current)
    for key in ['source_binding', 'source_manifest']:
        plan[key] = plan[key].replace(previous_source, 'source-v16-preparation')
    for key in ['run_root', 'observer_root', 'tmpdir']:
        plan[key] = plan[key].replace(old_name, new_name)
    plan['environment_fixed']['TMPDIR'] = plan['environment_fixed']['TMPDIR'].replace(old_name, new_name)
    plan['execution'] = ['/usr/bin/python3', '-B', str(new / 'launch.py')]
    plan['reuse_owned_target_cache'] = True
    for stage in plan['stages']:
        stage['out'] = stage['out'].replace(old_name, new_name)
        stage['argv'] = [value.replace(old_name, new_name) for value in stage['argv']]
        assert stage['payload'] == stage['argv'][11:]
    for key in ['run_root', 'observer_root']:
        assert not Path(plan[key]).exists() and not Path(plan[key]).is_symlink()
    # This is normal reuse of the already owned Cargo cache. Earlier v13/v14
    # qualification executables were copied and authenticated before reuse.
    target = Path(plan['target_dir'])
    assert target.is_dir() and not target.is_symlink()
    assert target.resolve(strict=True).is_relative_to(root / 'target')
    return old, new, plan


old, new, plan = prepare('cargo-v9', 'cargo-v10', 'source-v12-preparation')
old_selected = list(plan['selected_tests'])
plan['selected_tests'] = json.loads((area / 'source-v16-preparation/selected-tests.json').read_text())['selected']
assert len(plan['selected_tests']) == len(set(plan['selected_tests'])) == 39
assert set(old_selected) <= set(plan['selected_tests'])
assert set(plan['selected_tests']) - set(old_selected) == {
    'failure::tests::completion_promotes_joined_worker_cause_without_duplicate_diagnostic',
    'failure::tests::completion_keeps_distinct_shared_cleanup_causes_and_first_worker_identity',
}
plan['required_count'] = 39
plan['new_test_count'] = 19
plan['status'] = 'Prepared source v16: 39 exact no-VM controls; execution requires root release.'
plan['preparation_changes'] = [
    'The measured duplicate EIO and original panic diagnostic are corrected structurally; all earlier failures remain retained.',
    'All 37 prior native selectors remain; two actual completion controls bring the prepared selection to 39.',
    'The owned target/prejoin-native-v9 cache is reused without copying or resetting it; v13/v14 qualification ELFs are separately retained.',
    'Fresh run and observer outputs; unchanged 600 CPU/900 wall compile, 5/15 list and 30/60 native limits, 16 GiB/zero swap/jobs 2.',
    'No manifests, lock, original assertions, VM/static cohort or Hermit code changes occur during this sequence.',
]
stage = plan['stages'][2]
stage['payload'] = ['<verified-compiled-test-executable>', '--exact'] + plan['selected_tests'] + ['--test-threads=1', '--nocapture']
stage['argv'] = stage['argv'][:11] + stage['payload']
for version in ['v13', 'v14']:
    directory = 'qualification-build-v1' if version == 'v13' else 'qualification-build-v2'
    # The actual files are included as inputs rather than trusting a prose claim
    # that prior cache artifacts remain recoverable.
    for name in ['lib', 'static-elf']:
        plan['inputs'].append(bind(area / directory / 'run-1' / ('retained-elf-' + version) / name))
(new / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
script = (old / 'launch.py').read_text().replace(digest(old / 'plan.json'), digest(new / 'plan.json'))
for before, after in [
    ('== 37', '== 39'), ("'count': 37", "'count': 39"),
    ("('37', '0', '0', '0')", "('39', '0', '0', '0')"),
    ('exactly 37 passing', 'exactly 39 passing'),
    ("'selected_count': 37", "'selected_count': 39"),
]:
    script = script.replace(before, after)
before = "    for path in [root, Path(plan['observer_root']), Path(plan['target_dir'])]:\n"
assert script.count(before) == 1
after = """    target = Path(plan['target_dir'])
    require(plan['reuse_owned_target_cache'] is True and target.is_dir() and not target.is_symlink(),
            'expected owned Cargo cache is missing or not a directory')
    require(target.stat().st_uid == os.getuid() and target.resolve(strict=True).is_relative_to(
            Path(plan['source_root']) / 'target'), 'Cargo cache is outside the owned target')
    for path in [root, Path(plan['observer_root'])]:
"""
script = script.replace(before, after)
ast.parse(script)
(new / 'launch.py').write_text(script)
(new / 'caller-changes.patch').write_text(''.join(difflib.unified_diff(
    (old / 'launch.py').read_text().splitlines(True), script.splitlines(True),
    fromfile=str(old / 'launch.py'), tofile=str(new / 'launch.py'))))
functions = runpy.run_path(str(new / 'launch.py'), run_name='preflight_only')
functions['check_inputs'](plan)
record = dict(plan=bind(new / 'plan.json'), caller=bind(new / 'launch.py'),
              observer=bind(plan['observer']), source=bind(plan['source_binding']),
              outputs={step['name']: step['out'] for step in plan['stages']},
              execution=plan['execution'], scope='Preparation only; no execution.')
(new / 'reservation.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))

old, new, plan = prepare('lint-v6', 'lint-v7', 'source-v14-preparation')
plan['scope'] = 'Read-only workspace format check and default-feature reverie-kvm all-targets Clippy on source v16. No native, VM or guest execution.'
(new / 'plan.json').write_text(json.dumps(plan, indent=2) + '\n')
script = (old / 'launch.py').read_text().replace(digest(old / 'plan.json'), digest(new / 'plan.json'))
ast.parse(script)
(new / 'launch.py').write_text(script)
functions = runpy.run_path(plan['helpers']['path'], run_name='preflight_only')
functions['check_inputs'](plan)
record = dict(plan=bind(new / 'plan.json'), caller=bind(new / 'launch.py'),
              source=bind(plan['source_binding']),
              outputs={step['name']: step['out'] for step in plan['stages']},
              execution=plan['execution'], scope='Preparation only; no execution.')
(new / 'reservation.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
