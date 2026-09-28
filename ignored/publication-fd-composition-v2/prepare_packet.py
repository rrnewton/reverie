"""Bind isolated source and propose a caller; never build/test or claim a lease."""
from pathlib import Path
import difflib, hashlib, json, os, shutil, subprocess

D = Path(__file__).resolve().parent
R = D.parents[1]
V1 = D.parent / 'publication-fd-composition-v1'
P = D.parent / 'process-publication-implementation-v1/frozen-v1'
F = D.parent / 'same-inode-ofd-repair-v2/final-v3'
S = R.parent / 'kvm-parent-reader-support-20260916'
H = R.parent / 'kvm-replay-prerequisites-20260918'
paths = ['reverie-kvm/src/elf.rs', 'reverie-kvm/src/executor.rs',
         'reverie-kvm/src/process_signal_publication.rs']

def record(p):
    b = p.read_bytes()
    return dict(path=str(p), bytes=len(b), sha256=hashlib.sha256(b).hexdigest())

def put(name, value):
    p = D / name
    assert not p.exists(), p
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(value, indent=2) + '\n')

def copy(old, new):
    assert not new.exists(), new
    new.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(old, new)

initial = json.loads((V1 / 'INPUTS-INITIAL.json').read_text())
for row in initial['publisher'] + initial['fd'] + [initial['lock']]:
    assert record(Path(row['path'])) == row, row['path']
initial['composition_v1'] = [record(V1 / name) for name in
    ['TARGET.json', 'READBACK.json', 'SOURCE.patch', 'REPORT.md', 'CONFLICTS.md',
     'CALLER_INPUTS.json', 'SOURCE-MANIFEST.json']]
reviews = [H/'ignored/process-publication-native-review-v1/01-source.final.txt',
           S/'ignored/kvm-private-publication-claude-review-v1-20260918/CLAUDE-REPORT.md']
assert record(reviews[0])['sha256'] == 'a262e7ae3582666500240f187df8f118bf3cd6c1fe8d3694340f8e68d9be9c42'
initial['reviews'] = [record(p) for p in reviews]
put('INPUTS-INITIAL.json', initial)

for rel in paths:
    copy(D/'source'/rel, D/'after'/rel)
for rel in paths[:2]:
    copy(V1/'base'/rel, D/'base'/rel)
predecessors = [('publisher', P/'source', 'PUBLISHER-TO-COMPOSED.patch'),
                ('fd', F/'after', 'FD-TO-COMPOSED.patch'),
                ('composition-v1', V1/'source', 'V1-TO-V2.patch'),
                ('base', D/'base', 'SOURCE.patch')]
for name, root, patch in predecessors:
    result = ''
    for rel in paths:
        old = root/rel
        a = old.read_text() if old.is_file() else ''
        if a and name != 'base':
            copy(old, D/'before'/name/rel)
        b = (D/'source'/rel).read_text()
        result += ''.join(difflib.unified_diff(
            a.splitlines(keepends=True), b.splitlines(keepends=True),
            fromfile='a/'+rel if a else '/dev/null', tofile='b/'+rel))
    (D/patch).write_text(result)

manifest = []
for old in json.loads((V1/'SOURCE-MANIFEST.json').read_text()):
    rel = old['relative']; p = D/'source'/rel
    row = dict(relative=rel, mode=old['mode'], kind=old['kind'],
               publisher_git_object=old.get('publisher_git_object'))
    if row['kind'] == 'file':
        row.update(record(p))
        assert bool(p.stat().st_mode & 0o111) == (row['mode'] == '100755')
        if rel not in paths:
            assert row['sha256'] == old['sha256'], rel
    elif row['kind'] == 'symlink':
        assert p.is_symlink() and os.readlink(p) == old['target']
        row.update(target=os.readlink(p), sha256=old['sha256'])
    else:
        assert p.is_dir() and not list(p.iterdir()), p
    manifest.append(row)
put('SOURCE-MANIFEST.json', manifest)
put('qualification-proposal/source-manifest.json', [
    dict(path=r['relative'], mode=r['mode'], **(
        {'git_object': r['publisher_git_object']} if r['kind'] == 'unexpanded_gitlink'
        else {'sha256': r['sha256']})) for r in manifest])
assert record(D/'source/Cargo.lock')['sha256'] == initial['lock']['sha256']

oldselection = json.loads((V1/'qualification-proposal/SELECTORS.json').read_text())
selection = json.loads(json.dumps(oldselection))
added = ['executor::tests::descriptor_retirement_close_and_dup_release_both_guards',
         'executor::tests::descriptor_retirement_exec_and_exit_release_both_guards',
         'executor::tests::descriptor_retirement_install_error_releases_both_guards',
         'executor::tests::descriptor_retirement_accept_cleanup_releases_both_guards',
         'executor::process_signal_publication::tests::inactive_publication_refuses_poisoned_file_table_without_effects']
for i, name in enumerate(added, 1):
    selection['groups'][f'retirement-{i:02}'] = dict(artifact='lib', names=[name])
seen = [(g['artifact'], name) for g in selection['groups'].values() for name in g['names']]
assert len(seen) == len(set(seen)) == 78
selection.update(proposed_unique_declarations=78, proposed_lib_declarations=69,
                 proposed_static_declarations=9, added_declarations=added)
selection['inputs'].append(record(V1/'qualification-proposal/SELECTORS.json'))
put('qualification-proposal/SELECTORS.json', selection)
put('qualification-proposal/SETUP.json', dict(owner_slot=str(R), source_root=str(D/'source'),
    base=initial['base'], source_files=[dict(relative=p, file=record(D/'after'/p)) for p in paths],
    purpose='Isolated inactive publisher + FD entry repair + destruction after both guards; unexecuted'))
caller = (V1/'qualification-proposal/prepare.py').read_text()
caller = caller.replace('Inactive publisher and descriptor-entry composition; unchanged unit and VM controls.',
                        'Inactive publisher and descriptor-entry composition; additive retirement/poison and unchanged unit/VM controls.')
(D/'qualification-proposal/prepare.py').write_text(caller)
for old, patch in [(V1/'qualification-proposal/prepare.py', 'CALLER-V1-TO-V2.patch'),
                   (D.parent/'process-publication-implementation-v1/qualification-v2/prepare.py', 'CALLER-CHANGE.patch')]:
    (D/patch).write_text(''.join(difflib.unified_diff(old.read_text().splitlines(keepends=True),
        caller.splitlines(keepends=True), fromfile=str(old), tofile='qualification-proposal/prepare.py')))
oldcaller = json.loads((V1/'CALLER_INPUTS.json').read_text())
for group in oldcaller['original_closure']:
    for key in ['publisher', 'fd']:
        if group.get(key): assert record(Path(group[key]['path'])) == group[key]
put('CALLER_INPUTS.json', dict(original_closure=oldcaller['original_closure'],
    observer_source_inputs=oldcaller['observer_source_inputs'],
    predecessor=record(V1/'CALLER_INPUTS.json'),
    proposed=[record(D/'qualification-proposal'/n) for n in
              ['prepare.py', 'SETUP.json', 'SELECTORS.json', 'source-manifest.json']],
    execution_performed=False, not_a_deployed_caller=True))

proof = {}
for rel, startmark, endmark in [
    (paths[1], '    type RetirementObservations =',
     '    #[test]\n    fn shared_file_table_reopen_same_inode_replaces_description()'),
    (paths[2], '    #[test]\n    fn inactive_publication_refuses_poisoned_file_table_without_effects',
     '    #[test]\n    fn inactive_publication_alarm_preserves_masks_dispositions_and_coalesces')]:
    a = (V1/'source'/rel).read_text(); b = (D/'source'/rel).read_text()
    start = b.index(startmark); end = b.index(endmark, start)
    block = b[start:end]; stripped = b[:start]+b[end:]
    oldtests = a.split('#[cfg(test)]\nmod tests {')[1]
    newtests = stripped.split('#[cfg(test)]\nmod tests {')[1]
    assert newtests == oldtests, rel
    name = Path(rel).stem+'-NEW-TESTS.rs'
    (D/name).write_text(block)
    proof[rel] = dict(old_test_bodies_byte_identical=True, old_test_module_bytes=len(oldtests.encode()),
        old_test_module_sha256=hashlib.sha256(oldtests.encode()).hexdigest(), new_bodies=record(D/name))
for rel in ['reverie-kvm/tests/static_elf.rs', 'reverie-kvm/tests/parked_signals/mod.rs']:
    # Full manifest continuity, rather than an assumed fixture path, is authoritative.
    if (D/'source'/rel).is_file():
        assert (D/'source'/rel).read_bytes() == (V1/'source'/rel).read_bytes()
put('TEST-SOURCE-CONTINUITY.json', dict(proof=proof, unchanged_other_manifest_entries=True,
    original_selected_declarations=73, additional_declarations=5, total_proposed_declarations=78,
    actual_inventory=None, no_assertion_or_budget_relaxation=True))
put('SOURCE_INPUTS.json', dict(base=initial['base'], predecessor_inputs=record(D/'INPUTS-INITIAL.json'),
    publisher_target=record(P/'TARGET.json'), fd_final_inputs=record(F/'SOURCE_INPUTS.json'),
    composition_v1=record(V1/'TARGET.json'), source_manifest=record(D/'SOURCE-MANIFEST.json'),
    after=[dict(relative=p, file=record(D/'after'/p)) for p in paths],
    actual_ignored_lock=record(D/'source/Cargo.lock'), production_execution_performed=False))

def git(*args):
    r = subprocess.run(['/usr/bin/git','-C',str(R),*args], capture_output=True,
                       env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'},timeout=30)
    assert r.returncode == 0, (args,r.returncode,r.stderr)
    return r.stdout
before = json.loads((V1/'LIVE-BEFORE.json').read_text())
after = dict(head=git('rev-parse','HEAD').decode().strip(), branch=git('branch','--show-current').decode().strip(),
    index=record(Path(before['index']['path'])), live=[record(R/p) for p in paths])
assert after == before, (before,after)
put('LIVE-CONTINUITY.json', dict(predecessor=record(V1/'LIVE-BEFORE.json'), before=before, after=after,
    identical=True, scope='Read-only HEAD/branch/whole-index/three live publisher files'))
print(json.dumps(dict(manifest=len(manifest), selectors=len(seen), source=record(D/'SOURCE.patch'),
                     delta=record(D/'V1-TO-V2.patch')),indent=2))
