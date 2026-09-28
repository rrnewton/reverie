#!/usr/bin/env python3
"""Freeze read-only source bindings; writes only this owned artifact directory."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

D = Path(__file__).resolve().parent
R = D.parents[1]
BASE = '000c15a1161ea2d58749431b5ddaaa97f7aa37d5'
P = Path('/home/newton/work/dev-hermit')
SUPPORT = P / 'worktrees/slots/kvm-parent-reader-support-20260916/ignored'
H = P / 'worktrees/slots/kvm-replay-prerequisites-20260918/ignored/timer-integration-review-v27'
env = dict(os.environ, GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')

def git(*args):
    proc = subprocess.run(['git', '-C', str(R), *args], env=env,
                          capture_output=True, timeout=30, check=True)
    return proc.stdout

def digest(data):
    return hashlib.sha256(data).hexdigest()

def record(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': digest(data)}

def put(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        assert path.read_bytes() == data, f'refuse overwrite: {path}'
    else:
        path.write_bytes(data)

def jput(path, data):
    put(path, (json.dumps(data, indent=2) + '\n').encode())

assert git('rev-parse', 'HEAD').decode().strip() == BASE
assert not git('diff', '--no-ext-diff', '--binary', 'HEAD')
tree = git('rev-parse', BASE + '^{tree}').decode().strip()
sources = []
reverie_files = [
    'reverie/src/tool.rs', 'reverie/src/guest.rs', 'reverie/src/signal.rs',
    'reverie/src/signal_observation.rs',
    'reverie-kvm/src/elf.rs', 'reverie-kvm/src/executor.rs',
    'reverie-kvm/src/runtime.rs', 'reverie-kvm/src/signal.rs',
    'reverie-kvm/src/vm.rs', 'reverie-kvm/src/error.rs',
]
for name in reverie_files:
    data = git('show', BASE + ':' + name)
    assert (R / name).read_bytes() == data, name
    target = D / 'source/reverie' / name
    put(target, data)
    sources.append({'repository': 'reverie', 'revision': BASE, 'relative_path': name,
                    'git_blob': git('rev-parse', BASE + ':' + name).decode().strip(),
                    'original': record(R / name), 'snapshot': record(target)})

copy = json.loads((H / 'SOURCE-COPY.json').read_text())
assert digest((H / 'SOURCE-COPY.json').read_bytes()) == '6d4fa360e94c1ee2d7d82f51c0621630ed507b47bee969e74394ca0724a249bc'
assert digest((H / 'PACKET.json').read_bytes()) == '2c7bd85c4933593d3587735f548f32d64bd694b936510d02908f4e009ede3385'
entries = {x['path']: x for x in copy['entries']}
hermit_files = [
    'detcore/src/scheduler.rs', 'detcore/src/scheduler/parked.rs',
    'detcore/src/scheduler/timed_waiters.rs', 'detcore/src/tool_global.rs',
    'detcore/src/tool_global/parked.rs', 'detcore/src/lib.rs',
]
for name in hermit_files:
    original = H / 'source' / name
    expected = entries[name]['snapshot']
    data = original.read_bytes()
    assert digest(data) == expected['sha256'] and len(data) == expected['bytes'], name
    target = D / 'source/hermit' / name
    put(target, data)
    sources.append({'repository': 'hermit', 'context': 'frozen authored v27',
                    'head': copy['head'], 'main': copy['main'], 'relative_path': name,
                    'original': record(original), 'snapshot': record(target)})

refs = [
    P / 'AGENTS.md', P / '.skills/code-review/SKILL.md',
    P / '.skills/deterministic-scheduling-review/SKILL.md',
    P / '.skills/supplemental_docs/coordinator-codex.md',
    H / 'PACKET.json', H / 'SOURCE-COPY.json',
    H.parent / 'timer-validation-v27/ACTUAL-REVERIE.json',
    SUPPORT / 'm2-chaosrand-choice-research-20260918/GROUNDING.json',
    SUPPORT / 'kvm-setitimer-signal-research-20260918/PRIMARY_REFERENCE_ADDENDUM_V1.md',
    SUPPORT / 'kvm-setitimer-signal-research-20260918/PRIMARY_REFERENCE_INPUTS_V1.json',
    SUPPORT / 'kvm-setitimer-signal-research-20260918/PRIMARY_REFERENCE_READBACK_V1.json',
    R / 'ignored/scalar-read-positioned-fd-parity-v1/READBACK.json',
]
reference_records = []
for i, path in enumerate(refs):
    target = D / 'references' / f'{i:02d}-{path.name}'
    put(target, path.read_bytes())
    reference_records.append({'original': record(path), 'snapshot': record(target)})

tracked = git('ls-tree', '-r', '--name-only', BASE).decode().splitlines()
applicable = []
for f in reverie_files:
    for parent in [Path('.'), *reversed(Path(f).parents[:-1])]:
        for name in ['AGENTS.md', 'CLAUDE.md']:
            p = str(parent / name)
            if p in tracked:
                applicable.append(p)
jput(D / 'INPUTS.json', {
    'scope': 'Source-only author design, no implementation or execution',
    'reverie': {'revision': BASE, 'tree': tree, 'tracked_diff_empty': True,
                'applicable_tracked_product_instructions': sorted(set(applicable))},
    'hermit': {'context': 'frozen v27 authored source', 'head': copy['head'],
               'main': copy['main'], 'packet': record(H / 'PACKET.json')},
    'sources': sources, 'references': reference_records,
    'read_coverage': {
        'reverie': 'Focused production paths described by REPORT.md; complete source copies bound, not a claim all lines were read.',
        'hermit': 'Relevant daemon/timed/exit/control paths and focused detcore/src symbol searches; not a complete v27 scheduler review.',
        'prior_grounding': 'Own earlier ordered full paper/v8 scheduler/vision/roadmap grounding retained as history only, not attributed to v27.',
        'primary_references': 'Own prior frozen primary-reference research reused; no new fetch or native observation.',
    },
    'nonclaims': ['No runtime result', 'No independent approval', 'No API implemented',
                  'No new executable declarations', 'No source/index/ref/cache changes'],
})

assert git('rev-parse', 'HEAD').decode().strip() == BASE
assert not git('diff', '--no-ext-diff', '--binary', 'HEAD')
files = [p for p in D.rglob('*') if p.is_file() and p.name != 'READBACK.json']
jput(D / 'READBACK.json', {
    'scope': 'Frozen author proposal and exact read-only input copies',
    'head_before_and_after': BASE, 'tree': tree,
    'tracked_diff_empty_before_and_after': True,
    'records': [record(p) for p in sorted(files)],
    'all_source_copies_authenticated': True,
    'runtime_executions': 0, 'new_executable_declarations': 0,
})
for name in ['REPORT.md', 'API.md', 'INPUTS.json', 'READBACK.json']:
    print(json.dumps(record(D / name)))
