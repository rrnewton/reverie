#!/usr/bin/python3
"""Freeze source/caller inputs by reading exact paths; no payload or lease admission."""
import ast
import hashlib
import json
import os
import stat
import subprocess
import sys
from pathlib import Path

W = Path(__file__).resolve().parent
Q = W / 'qualification-v1'
L = W.parent.parent
C = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/kvm-clock-origin-correction-source-v1')
PARENT = Path('/home/newton/work/dev-hermit')
H = PARENT / 'worktrees/slots/kvm-replay-prerequisites-20260918'


def require(value, message):
    if not value:
        raise RuntimeError(message)


def load(path):
    return json.loads(Path(path).read_text())


def rec(path):
    path = Path(path)
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode), 'not a regular file: ' + str(path))
    value = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(1024**2):
            value.update(block)
    after = path.lstat()
    require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_mode) ==
            (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_mode),
            'file changed during authentication: ' + str(path))
    return dict(path=str(path), bytes=after.st_size, mode=stat.S_IMODE(after.st_mode),
                sha256=value.hexdigest())


def write(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())


files = {}
links = {}


def add_file(path, expected=None):
    row = rec(path)
    if expected:
        for key in ('path', 'bytes', 'mode', 'sha256'):
            if key in expected:
                require(row[key] == expected[key], 'held input changed: ' + str(path))
    require(row['path'] not in files or files[row['path']] == row, 'conflicting file record')
    files[row['path']] = row
    return row


def add_link(path, expected=None):
    path = Path(path)
    require(path.is_symlink(), 'alias absent: ' + str(path))
    row = dict(path=str(path), target=os.readlink(path))
    if expected:
        require(row == expected, 'alias changed: ' + str(path))
    require(row['path'] not in links or links[row['path']] == row, 'conflicting alias')
    links[row['path']] = row


def referenced_records(value):
    if isinstance(value, dict):
        if {'path', 'bytes', 'sha256'} <= value.keys():
            yield value
        else:
            for item in value.values():
                yield from referenced_records(item)
    elif isinstance(value, list):
        for item in value:
            yield from referenced_records(item)


def add_tree(root):
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            add_link(path)
        elif path.is_file():
            add_file(path)


require(sys.flags.optimize == 0, 'explicit unoptimized freezer required')
for name in ['TARGET.json', 'READBACK.json']:
    require(not (W/name).exists(), 'preserve existing source freeze')
for name in ['CALLER-TARGET.json', 'INPUTS.json', 'READBACK.json']:
    require(not (Q/name).exists(), 'preserve existing caller freeze')

# Verify actual source bytes and modes against the complete authored manifest.
manifest = load(W/'SOURCE-MANIFEST.json')
for row in manifest:
    path = W/'source'/row['path']
    if row['mode'] == '120000':
        add_link(path)
        require(hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest() == row['sha256'],
                'source link changed')
    else:
        actual = add_file(path)
        require(actual['sha256'] == row['sha256'] and bool(actual['mode'] & 0o111) ==
                (row['mode'] == '100755'), 'authored source drift')
source_names = {row['path'] for row in manifest}
actual_names = {str(path.relative_to(W/'source')) for path in (W/'source').rglob('*')
                if path.is_symlink() or path.is_file()}
require(source_names == actual_names, 'source manifest not complete')

gitlinks = [row for row in load(W/'BASE-MANIFEST.json')['entries'] if row['mode'] == '160000']
for tree in ['base', 'before', 'source']:
    for row in gitlinks:
        path = W/tree/row['path']
        require(path.is_dir() and not path.is_symlink() and not list(path.iterdir()),
                'recorded unexpanded gitlink changed')
for tree in ['base', 'before', 'author-format-before', 'author-format-before-2', 'author-format-before-3']:
    add_tree(W/tree)

syntax = []
for path in sorted(W.glob('*.py')) + sorted(Q.glob('*.py')) + sorted((Q/'observer').glob('*.py')):
    ast.parse(path.read_text(), filename=str(path))
    syntax.append(rec(path))

for row in load(Q/'HELPER-CONTINUITY.json')['runtime_helpers']:
    add_file(row['before']['path'], row['before'])
    add_file(row['after']['path'], row['after'])
    require(row['before']['sha256'] == row['after']['sha256'], 'runtime helper changed')

metadata = load(Q/'metadata-plan.json')
for row in metadata['inputs']:
    add_file(row['path'], row)
for row in metadata['input_symlinks']:
    add_link(row['path'], row)
for row in load(Q/'observer/source-inputs.json')['inputs']:
    path = Path(row['path'])
    if path.is_symlink():
        add_link(path)
        resolved = path.resolve(strict=True)
        add_file(resolved, {**row, 'path': str(resolved)})
    else:
        add_file(path, row)
for name in metadata['absent_inputs']:
    require(not Path(name).exists() and not Path(name).is_symlink(), 'Cargo override appeared')

# Authenticate existing immutable registry sources; fresh metadata still derives its own closure.
dependency_index = C/'corrected-component-continuation-v1/dependency-inputs.json'
dependency_aliases = C/'corrected-component-continuation-v1/dependency-symlinks.json'
add_file(dependency_index)
add_file(dependency_aliases)
dependencies = load(dependency_index)
for row in dependencies:
    require(Path(row['path']).is_relative_to('/home/newton/.cargo/registry/src'),
            'unexpected predecessor dependency root')
    add_file(row['path'], row)
for row in load(dependency_aliases):
    add_link(row['path'], row)
manifest_paths = [row['path'] for row in manifest
                  if row['path'].endswith('Cargo.toml') or row['path'] in ('rust-toolchain', 'rust-toolchain.toml')]
require(len(manifest_paths) == 28, 'manifest continuity count changed')
for name in manifest_paths + ['Cargo.lock']:
    require((W/'source'/name).read_bytes() == (C/'source'/name).read_bytes(),
            'dependency carrier changed: ' + name)

# Preserve concrete predecessor records, raw streams, retained ELFs and replay trace.
# Referenced plan/artifact JSON remains evidence; mutable paths named inside it are not read.
history = load(Q/'HISTORY.json')
for row in referenced_records(history):
    add_file(row['path'], row)
for key in ['observation_target', 'observation_result', 'original_failed_result',
            'baseline_failed_result', 'first_debugger_refusal']:
    for row in referenced_records(load(history[key]['path'])):
        require('/target/' not in row['path'] and not row['path'].endswith('/lane.lease'),
                'historical mutable path requires retained mapping')
        add_file(row['path'], row)
for name in ['TRACE-ANALYSIS.json', 'TRACE-TIMELINE.tsv', 'SOURCE-EXCERPTS.md', 'SOURCE-SUPPLEMENT.md']:
    add_file(C/'clock-boundary-observation-v2/analysis-v1'/name)

# Hold explicit review instructions without relying on mutable inherited unindexed reads.
policy = W/'policy'
policy.mkdir(mode=0o700)
policy_rows = []
for name, original in [
    ('AGENTS.md', PARENT/'AGENTS.md'),
    ('code-review.md', PARENT/'.skills/code-review/SKILL.md'),
    ('deterministic-scheduling-review.md', PARENT/'.skills/deterministic-scheduling-review/SKILL.md'),
    ('post-facto-review.md', H/'.llms/skills/post-facto-review/SKILL.md'),
]:
    original = original.resolve(strict=True)
    before = rec(original)
    destination = policy/name
    destination.open('xb').write(original.read_bytes())
    after = rec(destination)
    require(before['sha256'] == after['sha256'], 'policy copy differs')
    if name == 'post-facto-review.md':
        require(after['sha256'] == '88ceb86d661b468a0a32f99776ae480a5231e9c9695fd97a223a965950008567',
                'post-facto policy changed')
    policy_rows.append(dict(original=before, held=after))
    add_file(destination)
write(W/'POLICY-ORIGIN.json', policy_rows)

state = {}
for key, args in [('head', ['rev-parse', 'HEAD']), ('branch', ['branch', '--show-current']),
                  ('index', ['ls-files', '--stage'])]:
    raw = subprocess.check_output(['/usr/bin/git', '--no-optional-locks', '-C', str(L), *args])
    state[key] = hashlib.sha256(raw).hexdigest() if key == 'index' else raw.decode().strip()
require(state == load(W/'OWNER-SCM.json')['state'], 'live SCM changed')
require(subprocess.check_output(['/usr/bin/git', '--no-optional-locks', '-C', str(L), 'status',
                                 '--porcelain=v1', '--untracked-files=no']).decode() ==
        load(W/'OWNER-SCM.json')['tracked_status'], 'live tracked status changed')

source_target = dict(
    scope='Authored unexecuted initial clock and physical injection-stop correction; no qualification or approval',
    base='3d4a401ed8959befad0b2f10db59093725f27432',
    base_tree='73024912837cbbeaa70b6d2173f5b596eebedcb3',
    landed_identical_base='b5e2ab49cd99e5d456fa0238b8cebd75958c529f',
    source_root=str(W/'source'), source_manifest=rec(W/'SOURCE-MANIFEST.json'),
    full_patch=rec(W/'SOURCE.patch'), successor_delta=rec(W/'INJECTION-CORRECTION.patch'),
    oracle_delta=rec(W/'ORACLE-CHANGES.patch'), report=rec(W/'REPORT.md'),
    continuity=rec(W/'CONTINUITY.json'), origin=rec(W/'SOURCE-ORIGIN.json'),
    publication_base=rec(W/'PUBLICATION-BASE.json'), preparation_limits=rec(W/'PREPARATION-LIMITS.json'),
    policy_origin=rec(W/'POLICY-ORIGIN.json'),
    old_evidence=rec(Q/'HISTORY.json'), author_formats=[rec(W/('AUTHOR-FORMAT'+suffix+'.json'))
                                                     for suffix in ['', '-2', '-3']],
    meaningful_new_declarations=7, meaningful_new_modes=8,
    no_product_execution=True, no_lease_admission=True, source_author_not_independent_reviewer=True)
write(W/'TARGET.json', source_target)

for path in sorted(W.iterdir()):
    if path.is_file():
        add_file(path)
for path in sorted(Q.rglob('*')):
    if path == Q/'lane.lease':
        continue
    if path.is_symlink():
        add_link(path)
    elif path.is_file():
        add_file(path)

phases = load(Q/'PHASES.json')['phases']
selectors = load(Q/'SELECTORS.json')
require(len(phases) == 45 and selectors['exact_declarations'] == 38 and
        selectors['library_declarations'] == 37 and selectors['integration_declarations'] == 1,
        'finite scope changed')
require([row['selected'][0] for row in phases if row['kind'] == 'test'] == selectors['ordered_names'],
        'phase/selection mismatch')
prior_selectors = [row['selected'][0] for row in load(C/'corrected-component-continuation-v1/PHASES.json')['phases']
                   if row['kind'] == 'test']
require(len(prior_selectors) == 9 and set(prior_selectors) <= set(selectors['ordered_names']),
        'original selection dropped')
absent = [Q/name for name in ['EXECUTION-RESULT.json', 'retained-binaries', 'dependency-inputs.json',
                              'dependency-symlinks.json', 'dependency-closure.json']]
for row in phases:
    absent += [Q/'controls'/row['name'], Q/'observer'/row['name']]
    if row['name'] != 'metadata':
        absent.append(Q/(row['name']+'-plan.json'))
for name in ['launch', 'controls', 'loader', 'cache', 'tmp', 'lease-state']:
    path = Q/name
    require(path.is_dir() and not path.is_symlink() and not list(path.iterdir()), 'output directory not empty')
for path in absent:
    require(not path.exists() and not path.is_symlink(), 'future output exists: ' + str(path))
target = Path(metadata['target']['path'])
tstat = target.lstat()
require(stat.S_ISDIR(tstat.st_mode) and stat.S_IMODE(tstat.st_mode) == 0o700 and
        [tstat.st_dev, tstat.st_ino, tstat.st_uid] == metadata['target']['identity'] and not list(target.iterdir()),
        'fresh private target changed')
lease = Path(metadata['lease']['path'])
lstat = lease.lstat()
require(stat.S_ISREG(lstat.st_mode) and stat.S_IMODE(lstat.st_mode) == 0o600 and lstat.st_size == 0 and
        [lstat.st_dev, lstat.st_ino, lstat.st_uid] == metadata['lease']['identity'], 'private lease not pristine')

index = dict(files=sorted(files.values(), key=lambda row: row['path']),
             symlinks=sorted(links.values(), key=lambda row: row['path']),
             must_be_absent_before_execution=[str(path) for path in absent],
             mutable_private_target_and_lease_excluded=True,
             prior_mutable_build_paths_metadata_only=True,
             historical_evidence_closure='Direct records in named final targets/results and exact analysis files; not a claim to re-audit every prior full index',
             dependency_prebinding='Exact prior registry closure authenticated; actual fresh metadata must derive and bind its closure before compile')
write(Q/'INPUTS.json', index)
caller_target = dict(scope='Prepared once-only 45-phase / 38-declaration fresh ptrace qualification; no execution',
                     source_target=rec(W/'TARGET.json'), inputs=rec(Q/'INPUTS.json'),
                     metadata_plan=rec(Q/'metadata-plan.json'), phases=rec(Q/'PHASES.json'),
                     selectors=rec(Q/'SELECTORS.json'), setup=rec(Q/'SETUP.json'),
                     helper_delta=rec(Q/'CALLER-DELTA.patch'), helper_continuity=rec(Q/'HELPER-CONTINUITY.json'),
                     execution=rec(Q/'EXECUTION.md'), readme=rec(Q/'README.md'),
                     command=['/usr/bin/python3', '-B', 'execute.py', 'run'],
                     required_outer_environment={'PYTHONOPTIMIZE': '0'}, cwd=str(Q),
                     source_and_bounds_unchanged_since_preparation=True, no_retry=True)
write(Q/'CALLER-TARGET.json', caller_target)
readback = dict(source_target=rec(W/'TARGET.json'), caller_target=rec(Q/'CALLER-TARGET.json'),
                inputs=rec(Q/'INPUTS.json'), file_count=len(files), symlink_count=len(links),
                source_manifest_entries=len(manifest), historical_registry_files=len(dependencies),
                recorded_unexpanded_gitlinks=gitlinks, dependency_manifest_continuity=28,
                runtime_helpers_unchanged=7, planned_phases=45, exact_declarations=38,
                library_declarations=37, integration_declarations=1, new_declarations=7, new_modes=8,
                original_nine_retained=True, python_syntax_parsed=syntax,
                private_target=metadata['target'], pristine_private_lease=metadata['lease'],
                live_scm_unchanged=state, no_execution=True, no_lease_admission=True,
                prior_unresolved_lease_not_opened=True,
                display_limits=['One combined preparation display truncated; prepare.py and retained/dependency helpers reread completely.',
                                'One verbose input listing truncated; freezer authenticates exact complete records directly.'])
write(Q/'READBACK.json', readback)
write(W/'READBACK.json', dict(source_target=rec(W/'TARGET.json'), caller_readback=rec(Q/'READBACK.json'),
                             source_patch=rec(W/'SOURCE.patch'), successor_delta=rec(W/'INJECTION-CORRECTION.patch'),
                             report=rec(W/'REPORT.md'), no_execution=True))
print(json.dumps(dict(source_target=rec(W/'TARGET.json'), caller_target=rec(Q/'CALLER-TARGET.json'),
                      caller_inputs=rec(Q/'INPUTS.json'), caller_readback=rec(Q/'READBACK.json'),
                      source_readback=rec(W/'READBACK.json'), file_count=len(files), symlinks=len(links),
                      planned_phases=45, declarations=38)))
