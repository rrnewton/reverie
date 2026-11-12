from pathlib import Path
import datetime
import difflib
import hashlib
import json
import os
import re
import stat
import subprocess
import sys

p = Path(__file__).resolve().parent
old = p.parent / 'hermit-3047-review-continuation'
repo = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916')
if len(sys.argv) != 2 or not re.fullmatch('[0-9a-f]{40}', sys.argv[1]):
    raise SystemExit('Pass the actual committed final head; this prepares evidence only.')
head = sys.argv[1]
base = '158a89f6217b25db9540237f9c1e256cdbaf785c'
prior = 'a9d3b1faa4502963e33bcb3e7e8a213a4e1a3bb0'
prior_base = 'b03fd6c16a438060f0013d58948643d8f3d81ab5'
composed = '4cfbe917fa1131d928a69aea14e28089c9de1d87'
env = {**os.environ, 'GIT_NO_LAZY_FETCH': '1', 'GIT_OPTIONAL_LOCKS': '0'}


def git(*args, repository=repo):
    return subprocess.check_output(['git', *args], cwd=repository, env=env)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write(name, data, mode=0o644):
    q = p / name
    q.parent.mkdir(parents=True, exist_ok=True)
    with q.open('xb') as f:
        f.write(data)
    q.chmod(mode)
    return q


def dump(name, value):
    return write(name, (json.dumps(value, indent=2) + '\n').encode())


def row(q):
    data = q.read_bytes()
    return {'path': str(q), 'bytes': len(data), 'sha256': sha(data),
            'mode': oct(stat.S_IMODE(q.stat().st_mode))}


def entries(rev):
    found = {}
    for raw in git('ls-tree', '-r', '-z', rev).split(b'\0'):
        if not raw:
            continue
        prefix, path = raw.split(b'\t', 1)
        mode, kind, oid = prefix.decode().split()
        found[path.decode()] = {'mode': mode, 'type': kind, 'oid': oid}
    return found


assert git('rev-parse', head + '^').decode().strip() == composed
tree = git('rev-parse', head + '^{tree}').decode().strip()
assert git('diff', '--name-only', composed, head).decode().splitlines() == ['hermit-cli/tests/cli.rs']
before_cli = git('show', composed + ':hermit-cli/tests/cli.rs')
after_cli = git('show', head + ':hermit-cli/tests/cli.rs')
needle = b'        let output = hermit_command(&args)\n            .arg(guest)\n            .arg(mode)\n'
assert before_cli.count(needle) == 1
replacement = needle + b'            .current_dir(native_dir.path())\n'
assert after_cli == before_cli.replace(needle, replacement), 'F1 delta is not the exact expected single insertion'

prior_entries, main_entries, old_base_entries = entries(prior), entries(base), entries(prior_base)
incoming_paths = sorted(k for k in main_entries.keys() | old_base_entries.keys()
                        if main_entries.get(k) != old_base_entries.get(k))
assert incoming_paths == ['scripts/validate.rs']
expected = dict(prior_entries)
expected['scripts/validate.rs'] = main_entries['scripts/validate.rs']
assert entries(composed) == expected
actual = entries(head)
expected['hermit-cli/tests/cli.rs'] = actual['hermit-cli/tests/cli.rs']
assert actual == expected and len(actual) == 1726
assert git('show', head + ':scripts/validate.rs') == git('show', base + ':scripts/validate.rs')

patch = git('diff', '--no-ext-diff', '--binary', base, head)
write('complete-hermit.patch', patch)
write('fixture-cwd.patch', git('diff', '--no-ext-diff', '--binary', composed, head))
write('follow-up.diff', git('diff', '--no-ext-diff', '--binary', prior, head))
assert sha(git('diff', '--no-ext-diff', '--binary', prior_base, base)) == 'c7a10f5264cc71e6f4bd89165910de573e6e7aff85d5572631ab28d684778783'
assert sha(git('diff', '--no-ext-diff', '--binary', base, composed)) == '96502f9880df67b8423b1ed111c65f1f08feb0eede6c30657db25f281f92083a'
write('base-cli.rs', before_cli)
write('base-validate.rs', git('show', prior + ':scripts/validate.rs'), 0o755)

manifest_old = json.loads((old / 'manifest.json').read_text())
source_rows = []
candidate_files = []
unchanged = []
specs = [(Path(r['origin']['repository']), r['origin']['path'], r['origin']['revision'], r['path'])
         for r in manifest_old['source_files']]
for extra in ['scripts/lib/rust_script_prelude.rs', 'scripts/lib/validate_runtime.rs', 'scripts/lib/validate_plan.rs']:
    specs.append((repo, extra, head, None))
for repository, relative, revision, old_copy in specs:
    product = 'hermit' if repository == repo else 'reverie'
    if product == 'hermit':
        revision = head
    entry = git('ls-tree', revision, '--', relative, repository=repository).decode().strip()
    prefix, actual_path = entry.split('\t', 1)
    mode, kind, blob = prefix.split()
    assert kind == 'blob' and relative == actual_path
    data = git('cat-file', 'blob', blob, repository=repository)
    q = write('source/' + product + '/' + relative, data, int(mode[-3:], 8))
    r = row(q)
    r['origin'] = {'repository': str(repository), 'revision': revision, 'path': relative,
                   'git_blob': blob, 'git_mode': mode}
    source_rows.append(r)
    candidate_files.append({**r, 'path': product + '/' + relative})
    if old_copy:
        unchanged.append({'path': product + '/' + relative, 'prior_copy': old_copy,
                          'same_bytes_and_mode': data == Path(old_copy).read_bytes()
                          and row(q)['mode'] == row(Path(old_copy))['mode']})
assert sorted(r['path'] for r in unchanged if not r['same_bytes_and_mode']) == ['hermit/hermit-cli/tests/cli.rs', 'hermit/scripts/validate.rs']

composition = {'base': base, 'prior_approved_head': prior, 'intermediate_head': composed,
               'head': head, 'tree': tree, 'whole_entries': len(actual),
               'whole_tree_equals_prior_plus_incoming_plus_exact_F1': True,
               'incoming_paths': incoming_paths, 'F1_only_path': 'hermit-cli/tests/cli.rs',
               'F1_exact_single_current_dir_insertion': True,
               'all_other_entries_modes_and_objects_unchanged': True,
               'prior_63_source_copy_comparisons': unchanged,
               'complete_authored_patch_bytes': len(patch), 'complete_authored_patch_sha256': sha(patch),
               'source_bindings': source_rows,
               'no_runtime_execution_or_new_validation_claim': True}
dump('composition-readback.json', composition)
candidate = {'repository': 'rrnewton/hermit', 'pull_request': 'https://github.com/rrnewton/hermit/pull/3047',
             'root': str(p / 'source'), 'base': base, 'head': head, 'tree': tree,
             'candidate_patch_sha256': sha(patch), 'files': candidate_files}
dump('candidate-binding.json', candidate)
template = (p / 'prompt.template.txt').read_text()
for key, value in {'__HEAD__': head, '__TREE__': tree, '__PATCH_BYTES__': str(len(patch)), '__PATCH_SHA__': sha(patch)}.items():
    template = template.replace(key, value)
assert not re.search('__\w+__', template)
write('prompt.txt', template.encode())

argument = '--execute-reviewed-' + head[:8]
launcher = (old / 'launch-review.py').read_text().replace('--execute-reviewed-a9d3b1fa', argument)
write('launch-review.py', launcher.encode())
write('launcher.diff', ''.join(difflib.unified_diff((old / 'launch-review.py').read_text().splitlines(True),
                                                 launcher.splitlines(True), fromfile='prior/launch-review.py',
                                                 tofile='follow-up/launch-review.py')).encode())
old_plan = json.loads((old / 'execution-plan.json').read_text())
plan = {k: old_plan[k] for k in ['default_launcher_refuses_without_execution_argument', 'argument_is_not_authorization',
                               'review_command', 'wall_seconds', 'kill_after_seconds', 'per_output_file_bytes',
                               'no_test_or_guest_execution', 'no_source_or_ref_or_pr_operations', 'tools_current_metadata', 'read_tools_only']}
plan.update(state='prepared_only_not_released_or_executed',
            proposed_released_argv=['/usr/bin/python3', '-B', str(p / 'launch-review.py'), argument],
            cwd=str(p / 'source/hermit'), hermit_base=base, hermit_head=head, hermit_tree=tree,
            candidate_binding_sha256=sha((p / 'candidate-binding.json').read_bytes()),
            outputs=[str(p / name) for name in ['launch.json', 'stdout.jsonl', 'stderr.log', 'exit.json']],
            source_scope='Incoming scripts/validate.rs plus exact test-only F1 insertion; complete final authored patch and relevant unchanged source provided.',
            prior_literal_source_approval={'head': prior, 'report': str(p / 'prior-a9-REVIEW.md'),
                                          'sha256': sha((p / 'prior-a9-REVIEW.md').read_bytes())},
            actual_evidence_cutoff='aa7 native19/fmt/Clippy634/build and2descriptor+3complete-mode passes retained at aa7; official method/empty-stderr obligation pending; Reverie426/1 unchanged.',
            raw_private_transcript_not_input=True, no_rebuild_or_relist_or_retest=True)
dump('execution-plan.json', plan)
write('plan.diff', ''.join(difflib.unified_diff((old / 'execution-plan.json').read_text().splitlines(True),
                                             (p / 'execution-plan.json').read_text().splitlines(True),
                                             fromfile='prior/execution-plan.json', tofile='follow-up/execution-plan.json')).encode())
write('prompt.diff', ''.join(difflib.unified_diff((old / 'prompt.txt').read_text().splitlines(True),
                                               template.splitlines(True), fromfile='prior/prompt.txt', tofile='follow-up/prompt.txt')).encode())

inherited = json.loads((old / 'input-binding.json').read_text())
for r in inherited:
    assert row(Path(r['path'])) == r, 'Inherited input changed: ' + r['path']
manifest = {'prepared_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'source_files': source_rows, 'inherited_input_count': len(inherited),
            'source_identity': {'base': base, 'head': head, 'tree': tree},
            'composition': composition, 'prior_review': row(p / 'prior-a9-REVIEW.md'),
            'preparation_only': True}
dump('manifest.json', manifest)
bound = {r['path']: r for r in inherited}
for q in sorted(p.rglob('*')):
    if q.is_file() and q.name not in ['input-binding.json', 'PREPARATION-READBACK.json', 'STATUS.md']:
        bound[str(q)] = row(q)
for tool in plan['tools_current_metadata']:
    r = row(Path(tool['path']))
    assert all(r[k] == tool[k] for k in ['path', 'bytes', 'sha256', 'mode'])
    bound[r['path']] = r
dump('input-binding.json', list(bound.values()))
for r in bound.values():
    assert row(Path(r['path'])) == r
summary = {'prepared_only': True, 'no_launcher_execution': True, 'head': head, 'tree': tree,
           'input_count': len(bound), 'source_copy_count': len(source_rows),
           'files': [row(p / name) for name in ['prompt.txt', 'launch-review.py', 'execution-plan.json',
                                              'input-binding.json', 'candidate-binding.json', 'manifest.json',
                                              'complete-hermit.patch', 'fixture-cwd.patch', 'follow-up.diff',
                                              'launcher.diff', 'plan.diff', 'prompt.diff', 'composition-readback.json']],
           'proposed_released_argv': plan['proposed_released_argv']}
dump('PREPARATION-READBACK.json', summary)
print(json.dumps(summary, indent=2))
