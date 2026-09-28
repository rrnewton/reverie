"""Authenticate source-only composition and freeze a review target, without executing product code."""
from pathlib import Path
import hashlib, json, os, re, subprocess

D = Path(__file__).resolve().parent
R = D.parents[1]
paths = ['reverie-kvm/src/elf.rs', 'reverie-kvm/src/executor.rs',
         'reverie-kvm/src/process_signal_publication.rs']

def record(p):
    b = p.read_bytes()
    return dict(path=str(p), bytes=len(b), sha256=hashlib.sha256(b).hexdigest())

def put(name, value):
    p = D/name
    assert not p.exists(), p
    p.write_text(json.dumps(value, indent=2)+'\n')

def reconstruct(patch, oldroot):
    lines = patch.read_text().splitlines(keepends=True)
    i = 0; results = {}
    while i < len(lines):
        assert lines[i].startswith('--- ')
        old = lines[i][4:].strip(); i += 1
        assert lines[i].startswith('+++ b/')
        rel = lines[i][6:].strip(); i += 1
        previous = [] if old == '/dev/null' else (oldroot/rel).read_text().splitlines(keepends=True)
        out = []; cursor = 0
        while i < len(lines) and lines[i].startswith('@@ '):
            m = re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@', lines[i])
            assert m, lines[i]
            start = int(m[1]); oc = int(m[2] or '1'); nc = int(m[4] or '1'); i += 1
            pos = start if oc == 0 else start-1
            assert pos >= cursor
            out += previous[cursor:pos]; cursor = pos; a = b = 0
            while i < len(lines) and not lines[i].startswith(('@@ ', '--- ')):
                line = lines[i]; i += 1
                assert line[0] in ' +-'
                if line[0] in ' -':
                    assert cursor < len(previous) and previous[cursor] == line[1:], (rel,cursor,line)
                    cursor += 1; a += 1
                if line[0] in ' +': out.append(line[1:]); b += 1
            assert (a,b) == (oc,nc), (rel,a,b,oc,nc)
        out += previous[cursor:]
        content = ''.join(out).encode()
        assert content == (D/'source'/rel).read_bytes(), rel
        results[rel] = hashlib.sha256(content).hexdigest()
    return results

proof = {}
for patch, predecessor in [('SOURCE.patch', 'base'),
    ('PUBLISHER-TO-COMPOSED.patch', 'before/publisher'),
    ('FD-TO-COMPOSED.patch', 'before/fd'),
    ('V1-TO-V2.patch', 'before/composition-v1')]:
    proof[patch] = dict(patch=record(D/patch), after=reconstruct(D/patch,D/predecessor))
    assert set(proof[patch]['after']) == set(paths)
put('RECONSTRUCTION.json', dict(patches=proof, exact_after_bytes=True,
    no_git_apply_or_index_mutation=True))

initial = json.loads((D/'INPUTS-INITIAL.json').read_text())
for row in initial['publisher'] + initial['fd'] + initial['composition_v1'] + initial['reviews'] + [initial['lock']]:
    assert record(Path(row['path'])) == row, row['path']
manifest = json.loads((D/'SOURCE-MANIFEST.json').read_text())
for row in manifest:
    p = D/'source'/row['relative']
    if row['kind'] == 'file':
        assert record(p) == {k:row[k] for k in ['path','bytes','sha256']}
        assert bool(p.stat().st_mode & 0o111) == (row['mode'] == '100755')
    elif row['kind'] == 'symlink':
        assert p.is_symlink() and os.readlink(p) == row['target']
    else:
        assert p.is_dir() and not list(p.iterdir())
for rel in paths: assert (D/'after'/rel).read_bytes() == (D/'source'/rel).read_bytes()
for row in json.loads((D/'SOURCE-FORMAT-RETAINED.json').read_text())['records']:
    assert record(Path(row['retained']['path'])) == row['retained']
    assert row['retained']['sha256'] == row['historical_path_record']['sha256']

live = json.loads((D/'LIVE-CONTINUITY.json').read_text())['after']
for row in live['live']+[live['index']]: assert record(Path(row['path'])) == row
for key, args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current'])]:
    c = subprocess.run(['/usr/bin/git','-C',str(R),*args],capture_output=True,
        env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'},timeout=30)
    assert c.returncode == 0 and c.stdout.decode().strip() == live[key], (key,c)

markers = {
    paths[0]: ['struct FileRetirement(', 'struct StagedFile {', 'fn destroy(',
               'fn try_clone_for_fork_locked(', 'fn inherit_process_state_locked('],
    paths[1]: ['impl FileTableState {', 'fn try_from_elf(', 'fn install(',
               'fn execute_accept(', 'fn fork_child(', 'fn thread_child_with_signal_observation(',
               'fn release_files_on_exit(', 'fn replace_after_exec(', 'impl Drop for ElfExecutor',
               'impl SyscallExecutor for ElfExecutor', 'fn insert_file_with_flags(',
               'fn duplicate_fd(', 'fn insert_file_pair(', 'fn install_received_rights(',
               'fn close(state:', 'type RetirementObservations =',
               'fn descriptor_retirement_close_and_dup_release_both_guards(',
               'fn descriptor_retirement_exec_and_exit_release_both_guards(',
               'fn descriptor_retirement_install_error_releases_both_guards(',
               'fn descriptor_retirement_accept_cleanup_releases_both_guards('],
    paths[2]: ['fn publish(', 'fn inactive_publication_refuses_poisoned_file_table_without_effects(']
}
source_map = {}
for rel, names in markers.items():
    lines = (D/'source'/rel).read_text().splitlines()
    source_map[rel] = {name:[i for i,line in enumerate(lines,1) if name in line] for name in names}
    assert all(len(rows)==1 for rows in source_map[rel].values()), source_map[rel]
put('SOURCE-MAP.json',source_map)

records = []
# This tree is owned evidence; no recursive enumeration of a product or foreign cache.
for p in sorted(D.rglob('*')):
    if p.is_symlink(): continue
    if p.is_file(): records.append(record(p))
assert len({r['path'] for r in records}) == len(records)
for row in records: assert record(Path(row['path'])) == row
put('READBACK.json', dict(records=records, record_count=len(records),
    manifest_entries=len(manifest), source_manifest=record(D/'SOURCE-MANIFEST.json'),
    predecessors=record(D/'INPUTS-INITIAL.json'), exact_patch_reconstruction=record(D/'RECONSTRUCTION.json'),
    no_product_execution=True, source_preparation_formatter_only=True,
    live_unchanged=record(D/'LIVE-CONTINUITY.json'), no_cache_or_lease_access=True))
put('TARGET.json', dict(base=initial['base'], status='SOURCE-ONLY CORRECTED COMPOSITION; review and qualification pending',
    product_paths=paths, source_patch=record(D/'SOURCE.patch'),
    publisher_delta=record(D/'PUBLISHER-TO-COMPOSED.patch'), fd_delta=record(D/'FD-TO-COMPOSED.patch'),
    predecessor_delta=record(D/'V1-TO-V2.patch'), report=record(D/'REPORT.md'),
    ownership=record(D/'OWNERSHIP.md'), plan=record(D/'PLAN.md'),
    source_inputs=record(D/'SOURCE_INPUTS.json'), caller_inputs=record(D/'CALLER_INPUTS.json'),
    caller_delta=record(D/'CALLER-V1-TO-V2.patch'), readback=record(D/'READBACK.json'),
    original_selectors=73, additive_selectors=5, proposed_unique_selectors=78,
    lib_selectors=69, static_selectors=9, actual_inventory=None, product_execution_performed=False,
    protocol='Retain both old trigger classifications; root applies conservative triggers 2+3 workflow. No new-syscall tags.'))
print(json.dumps({n:record(D/n)for n in ['TARGET.json','REPORT.md','READBACK.json','SOURCE.patch','V1-TO-V2.patch','PLAN.md']},indent=2))
