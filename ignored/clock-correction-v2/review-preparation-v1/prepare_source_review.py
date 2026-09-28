"""Prepare immutable review material only. This script never launches a reviewer."""
from pathlib import Path
import difflib
import hashlib
import json
import os
import stat
import subprocess
import sys

P = Path(__file__).resolve().parent
W = P.parent
L = W.parent.parent
H = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918')
S = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916')
ROOT = Path('/home/newton/work/dev-hermit')
F = L / 'ignored/parity-composition-v1/review-preparation-v1/final-v1'
D = H / 'ignored/kvm-clock-design-review-preparation-v1'
C = H / 'ignored/kvm-clock-origin-correction-source-v1'
FIX = W / 'v2-fix'
SOURCE = FIX / 'source'
BASE = 'b5e2ab49cd99e5d456fa0238b8cebd75958c529f'
HBASE = '97c39b308efd854693038f24962484182d932f27'
SOURCE_TARGET_SHA = 'c01fe71e9f784013be5c657d20e9578467f9b463efe0fb70365e9eefceda34eb'
PATCH_SHA = '50580ff2751dad2e24a2fc86aa5ca7274b13ef0ee6761514ea383a8e360e410e'
SID = {'claude': '5d98403d-b8e9-42bf-8e2f-da7e7b70809c', 'native': '01a0b38e-9e3c-7883-9816-65199c048935'}

def require(ok, reason):
    if not ok:
        raise RuntimeError(reason)

def rec(path):
    path = Path(path)
    require(path.is_file() and not path.is_symlink(), f'not a regular file: {path}')
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return dict(path=str(path), bytes=path.stat().st_size, sha256=h.hexdigest(), mode=stat.S_IMODE(path.stat().st_mode))

def put(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x') as f:
        f.write(value if isinstance(value, str) else json.dumps(value, indent=2) + '\n')

def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True).stdout

require(sys.flags.optimize == 0, 'run with PYTHONOPTIMIZE=0')
require(rec(FIX / 'TARGET.json')['sha256'] == SOURCE_TARGET_SHA, 'source target drift')
require(rec(FIX / 'SOURCE.patch')['sha256'] == PATCH_SHA, 'patch drift')
require(git(L, 'rev-parse', BASE + '^{tree}').strip() == b'73024912837cbbeaa70b6d2173f5b596eebedcb3', 'base tree drift')

# Bind every actual source entry without reading or mutating the live index.
source_files, aliases, empty_gitlinks = [], [], []
manifest = json.loads((FIX / 'SOURCE-MANIFEST.json').read_text())
for row in manifest:
    path = SOURCE / row['path']
    if row['mode'] == '120000':
        require(path.is_symlink(), str(path))
        target = os.readlink(path)
        require(hashlib.sha256(target.encode()).hexdigest() == row['sha256'], str(path))
        aliases.append(dict(path=str(path), target=target))
    elif row['mode'] in ('100644', '100755'):
        actual = rec(path)
        require(actual['sha256'] == row['sha256'], str(path))
        require(actual['mode'] == int(row['mode'][-3:], 8), str(path))
        source_files.append(actual)
    else:
        require(path.is_dir() and not any(path.iterdir()), f'unexpected manifest kind: {row}')
        empty_gitlinks.append(row)

changed = ['reverie-ptrace/src/' + name for name in ('clock_origin_tests.rs', 'injection_stop_tests.rs', 'perf.rs', 'task.rs', 'timer.rs', 'tracer.rs')]
base_paths = {x.split(b'\t', 1)[1].decode(): x.split(b'\t', 1)[0].decode() for x in git(L, 'ls-tree', '-r', '-z', BASE).split(b'\0') if x}
base_rows = []
for rel in changed:
    if rel not in base_paths:
        base_rows.append(dict(path=rel, present=False))
        continue
    path = P / 'common/base' / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('xb') as f:
        f.write(git(L, 'show', BASE + ':' + rel))
    base_rows.append(dict(path=rel, present=True, file=rec(path), git_entry=base_paths[rel]))

hermit_rows = []
for rel in ['AGENTS.md', 'detcore/src/lib.rs', 'detcore/src/time.rs']:
    path = P / 'common/hermit-97c' / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('xb') as f:
        f.write(git(H, 'show', HBASE + ':' + rel))
    hermit_rows.append(dict(file=rec(path), revision=HBASE, path=rel, blob=git(H, 'rev-parse', HBASE + ':' + rel).decode().strip()))

helpers = [('claude', 'launch-review.py'), ('claude', 'audit-final.py'), ('native', 'launch-guard.py'), ('native', 'run_stage.py'), ('native', 'audit_source.py'), ('native', 'audit_split_reads.py')]
helper_records = []
for lane, name in helpers:
    original = F / lane / name
    content = original.read_text()
    revised = content
    if name == 'audit-final.py':
        old = 'Actual independent linear CPUID and capture composition source review. No whole-backend parity or clock-repair approval inferred.'
        new = 'Actual independent initial-command clock and syscall-injection source review. No whole-backend parity or signal-design approval inferred.'
        require(content.count(old) == 1, 'audit wording anchor')
        revised = content.replace(old, new)
    destination = P / lane / name
    put(destination, revised)
    delta = ''.join(difflib.unified_diff(content.splitlines(True), revised.splitlines(True), fromfile=str(original), tofile=str(destination)))
    if delta:
        put(P / lane / (name + '.delta.patch'), delta)
    helper_records.append(dict(original=rec(original), prepared=rec(destination), byte_identical=content == revised, change='scope text only; predicates unchanged' if delta else None))
put(P / 'HELPER-CONTINUITY.json', helper_records)

# Exact, bounded kernel functions, not an obligation to reread entire kernel files.
kernel_root = S / 'ignored/m2-chaosrand-choice-research-20260918/linux-exec-reference'
elf_root = S / 'ignored/kvm-clock-exec-elf-primary-v1'
kernel_specs = [(kernel_root / 'kernel--events--core.c', ['static int event_enable_on_exec(', 'static void perf_event_enable_on_exec(', 'void perf_event_exec(void)']), (kernel_root / 'fs--exec.c', ['int begin_new_exec(', 'static int exec_binprm(']), (elf_root / 'binfmt_elf.c', ['static int load_elf_binary('])]
kernel_records = []
pieces = []
for source, starts in kernel_specs:
    lines = source.read_text().splitlines(True)
    functions = []
    for prefix in starts:
        candidates = [i for i, line in enumerate(lines) if line.startswith(prefix) and not line.rstrip().endswith(';')]
        require(len(candidates) == 1, f'function start {prefix}: {candidates}')
        lo = candidates[0]
        hi = next(i for i in range(lo + 1, len(lines)) if lines[i].strip() == '}' and lines[i].startswith('}'))
        functions.append(dict(name=prefix, first_line=lo + 1, last_line=hi + 1))
        pieces.append(f'### {source.name}:{lo+1}-{hi+1}\nFull source: {source}\nSHA256: {rec(source)["sha256"]}\nUpstream Linux: 199c9959d3a9b53f346c221757fc7ac507fbac50; vendor kernel identity is not inferred.\n\n' + ''.join(f'{n+1}: {lines[n]}' for n in range(lo, hi + 1)) + '\n')
    kernel_records.append(dict(source=rec(source), functions=functions))
put(P / 'common/KERNEL-FUNCTIONS.txt', ''.join(pieces))
put(P / 'common/KERNEL-INPUTS.json', kernel_records)

documents = []
def document(name, path, stage=2):
    path = Path(path)
    record = rec(path)
    lines = path.read_text().splitlines(True)
    chunks = []
    start, accumulated, size = 1, [], 0
    for number, line in enumerate(lines, 1):
        if accumulated and (len(accumulated) >= 160 or size + len(line.encode()) > 11000):
            chunk = P / 'common/chunks' / f'{name}-{start:05d}-{number-1:05d}.txt'
            put(chunk, ''.join(accumulated))
            chunks.append(dict(**rec(chunk), first_line=start, last_line=number - 1))
            start, accumulated, size = number, [], 0
        accumulated.append(line)
        size += len(line.encode())
    if accumulated:
        chunk = P / 'common/chunks' / f'{name}-{start:05d}-{len(lines):05d}.txt'
        put(chunk, ''.join(accumulated))
        chunks.append(dict(**rec(chunk), first_line=start, last_line=len(lines)))
    require(''.join(Path(r['path']).read_text() for r in chunks) == path.read_text(), f'chunk omission {name}')
    result = dict(name=name, document=record, lines=len(lines), chunks=chunks, stage_order=stage)
    documents.append(result)
    return result

for name in ['code-review', 'deterministic-scheduling-review']:
    document(name, ROOT / '.skills' / name / 'SKILL.md', 1)
document('hermit-instructions', P / 'common/hermit-97c/AGENTS.md', 1)
document('author-report', W / 'REPORT.md')
document('two-import-successor', FIX / 'REPORT.md')
document('full-change', FIX / 'SOURCE.patch')
document('injection-delta', W / 'INJECTION-CORRECTION.patch')
document('oracle-delta', W / 'ORACLE-CHANGES.patch')
document('compile-import-delta', FIX / 'V2-FIX.patch')
for rel in changed:
    document('current-' + Path(rel).stem, SOURCE / rel)
for rel in ['reverie/src/guest.rs', 'reverie/src/tool.rs', 'reverie-process/src/spawn.rs', 'reverie-process/src/container.rs', 'reverie-ptrace/src/lib.rs', 'reverie-ptrace/tests/vsyscall.rs']:
    document('context-' + rel.replace('/', '-').replace('.', '-'), SOURCE / rel)
for rel in ['detcore/src/lib.rs', 'detcore/src/time.rs']:
    document('hermit-current-' + Path(rel).stem, P / 'common/hermit-97c' / rel)
document('kernel-functions', P / 'common/KERNEL-FUNCTIONS.txt')
document('kernel-elf-provenance', elf_root / 'REPORT.md')
document('prior-clock-tests', C / 'source/reverie-ptrace/src/clock_origin_tests.rs')

# Retained observations are evidence, never a successful qualification surrogate.
history = json.loads((W / 'qualification-v2/HISTORY.json').read_text())
for name in ['observation_report', 'observation_result']:
    document(name.replace('_', '-'), history[name]['path'])
obs = C / 'clock-boundary-observation-v2'
document('entry-replay-trace-analysis', obs / 'analysis-v1/TRACE-ANALYSIS.json')
document('entry-replay-raw-events', obs / 'observer/observation/diagnostic/events.jsonl')
document('old-clock-raw-failure', C / 'corrected-component-continuation-v1/controls/new-02/result.json')
document('old-baseline-results', history['baseline_failed_result']['path'])
document('old-corrected-results', history['original_failed_result']['path'])
old_docs = json.loads((D / 'common/COMPLETE_READS.json').read_text())['documents']
historical_names = {'measurement-report', 'measurement-results', 'comparison-report', 'comparison-facts', 'clock-origin-1-0', 'clock-origin-1-1', 'current-kvm-ptrace-comparison'}
historical_names.update(d['name'] for d in old_docs if d['name'].startswith('original-'))
for row in old_docs:
    if row['name'] in historical_names:
        document('historical-' + row['name'], row['document']['path'])

old_final = W / 'qualification-v1/final-v1'
for name in ['REPORT.md', 'RESULTS.json', 'READBACK.json']:
    document('first-compile-' + name.replace('.', '-'), old_final / name)
q = W / 'qualification-v2'
for name in ['README.md', 'EXECUTION.md', 'SELECTORS.json', 'phase.py', 'common.py', 'cache_lease.py', 'bind_dependencies.py', 'retain_artifacts.py', 'prepare.py', 'execute.py']:
    document('caller-' + name.replace('.', '-'), q / name)
common_docs = list(documents)

own = {
    'claude': [('own-clock-report', D / 'claude/CLAUDE-REPORT.md'), ('own-coverage-correction', D / 'claude-coverage-v2/CLAUDE-CORRECTION-REPORT.md'), ('own-coverage-gap', D / 'claude/COVERAGE-GAP.json'), ('own-corrected-custody', D / 'claude-coverage-v2/AUTHENTICATION.json')],
    'native': [('own-clock-report', D / 'native/01-design.final.txt'), ('own-clock-custody', D / 'native/AUTHENTICATION.json')]
}
lane_stats = {}
for lane in ('claude', 'native'):
    lane_docs = []
    # Grounding authentication and own prior report precede new proposal reading.
    for name, path in [('own-grounding', P / lane / 'GROUNDING_AUTHENTICATION.json')] + own[lane]:
        if name == 'own-coverage-gap':
            continue  # Bound in custody; full large machine table is not mandatory prose.
        lane_docs.append(document(lane + '-' + name, path, 1))
    lane_docs += common_docs
    stats = dict(mandatory_documents=len(lane_docs), mandatory_chunks=sum(len(r['chunks']) for r in lane_docs), mandatory_lines=sum(r['lines'] for r in lane_docs))
    lane_stats[lane] = stats
    if lane == 'native':
        index = dict(documents=lane_docs, **stats)
    else:
        index = [dict(name=r['name'], source=r['document'], chunks=r['chunks']) for r in lane_docs]
    put(P / lane / 'COMPLETE_READS.DRAFT.json', index)
    put(P / lane / 'READ-INDEX.DRAFT.md', '\n'.join(f'{i}. {r["name"]}: {r["document"]["path"]} ({r["lines"]} lines)\n' + ''.join('   ' + c['path'] + '\n' for c in r['chunks']) for i, r in enumerate(lane_docs, 1)))

put(P / 'SOURCE-CONTEXT-BINDING.json', dict(base=BASE, authored_equivalent_base='3d4a401ed8959befad0b2f10db59093725f27432', base_tree='73024912837cbbeaa70b6d2173f5b596eebedcb3', source_target=rec(FIX/'TARGET.json'), full_patch=rec(FIX/'SOURCE.patch'), manifest=rec(FIX/'SOURCE-MANIFEST.json'), files=source_files, symlinks=aliases, gitlinks=empty_gitlinks, before=base_rows, hermit_context=hermit_rows, current_source_not_yet_qualified=True, source_approval=False))
put(P / 'COVERAGE-PREPARATION.json', dict(lanes=lane_stats, source_files=len(source_files), symlinks=len(aliases), final_head_pending=True, final_qualification_pending=True, no_external_launch=True, input_boundaries='Common authored source/evidence plus each lane own grounding and own prior report only; no peer report supplied.'))
print(json.dumps(dict(source=SOURCE_TARGET_SHA, lanes=lane_stats, helpers=len(helper_records), external_launches=0), indent=2))
