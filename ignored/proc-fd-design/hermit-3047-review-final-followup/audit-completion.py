from pathlib import Path
import datetime
import hashlib
import json
import os
import stat
import subprocess


p = Path(__file__).resolve().parent
if not (p / 'exit.json').is_file():
    raise SystemExit('Review has not completed; no final report written')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_row(q):
    data = q.read_bytes()
    return {'path': str(q), 'bytes': len(data), 'sha256': sha(data),
            'mode': oct(stat.S_IMODE(q.stat().st_mode))}


inputs = json.loads((p / 'input-binding.json').read_text())
assert len(inputs) == 543
assert len({r['path'] for r in inputs}) == 543
assert sha((p / 'input-binding.json').read_bytes()) == '7f58f5d02c7c85b3d590f9dcaecf53ca86df72a5b3564a3db3e7be7ea2468600'
verified = []
for expected in inputs:
    actual = file_row(Path(expected['path']))
    if actual != expected:
        raise RuntimeError('Review input drift: ' + expected['path'])
    verified.append(actual)

manifest = json.loads((p / 'manifest.json').read_text())
old_manifest = json.loads((p.parent / 'hermit-3047-review-preparation' / 'snapshot-manifest.json').read_text())
prior_manifest = json.loads((p.parent / 'hermit-3047-review-continuation' / 'manifest.json').read_text())
source_rows = manifest['source_files'] + prior_manifest['source_files'] + [r for r in old_manifest['files'] if 'git_blob' in r.get('origin', {})]
assert len(manifest['source_files']) == 66
env = {**os.environ, 'GIT_NO_LAZY_FETCH': '1', 'GIT_OPTIONAL_LOCKS': '0'}
source_checks = []
for r in source_rows:
    o = r['origin']
    data = subprocess.check_output(['git', 'cat-file', 'blob', o['git_blob']], cwd=o['repository'], env=env)
    if data != Path(r['path']).read_bytes():
        raise RuntimeError('Immutable Git object mismatch: ' + r['path'])
    entry = subprocess.check_output(['git', 'ls-tree', o['revision'], '--', o['path']], cwd=o['repository'], env=env).decode().rstrip('\n')
    header, tree_path = entry.split('\t', 1)
    mode, kind, blob = header.split()
    if kind != 'blob' or blob != o['git_blob'] or tree_path != o['path']:
        raise RuntimeError('Source origin mismatch: ' + r['path'])
    if 'git_mode' in o and mode != o['git_mode']:
        raise RuntimeError('Source Git mode mismatch: ' + r['path'])
    source_checks.append({'path': r['path'], 'git_blob': blob, 'git_mode': mode,
                          'revision': o['revision'], 'sha256': sha(data)})

launch = json.loads((p / 'launch.json').read_text())
exit_record = json.loads((p / 'exit.json').read_text())
assert launch['head'] == exit_record['head'] == 'a88f948021a24c6a1ea808f868516c0691c8819a'
assert launch['tree'] == exit_record['tree'] == 'cf2185e73d2335ddd70162e336b27440df5bf3d8'
assert launch['base'] == exit_record['base'] == '158a89f6217b25db9540237f9c1e256cdbaf785c'
assert launch['input_binding_sha256'] == sha((p / 'input-binding.json').read_bytes())
assert launch['prompt_sha256'] == sha((p / 'prompt.txt').read_bytes())
assert launch['wall_seconds'] == 900 and launch['kill_after_seconds'] == 10
assert launch['per_output_file_bytes'] == 67108864
actual_tree = subprocess.check_output(['git', 'rev-parse', launch['head'] + '^{tree}'],
                                     cwd=manifest['source_files'][0]['origin']['repository'], env=env).decode().strip()
assert actual_tree == launch['tree']

raw = (p / 'stdout.jsonl').read_bytes()
rows = []
invalid_lines = []
for number, line in enumerate(raw.decode().splitlines(), 1):
    if not line:
        continue
    try:
        rows.append(json.loads(line))
    except json.JSONDecodeError:
        invalid_lines.append(number)
tools = []
assistant = []
tool_results = []
for r in rows:
    if r.get('type') == 'assistant':
        for block in r.get('message', {}).get('content', []):
            if block.get('type') == 'tool_use':
                tools.append({'name': block['name'], 'input': block.get('input'), 'id': block.get('id')})
            elif block.get('type') == 'text':
                assistant.append(block['text'])
    elif r.get('type') == 'user':
        for block in r.get('message', {}).get('content', []):
            if block.get('type') == 'tool_result':
                tool_results.append({'tool_use_id': block.get('tool_use_id'), 'is_error': bool(block.get('is_error', False))})
for t in tools:
    if t['name'] not in ['Read', 'Grep', 'Glob']:
        raise RuntimeError('Unexpected review tool: ' + t['name'])
terminal = [r for r in rows if r.get('type') == 'result']
normal = (exit_record['exit_code'] == 0 and len(terminal) == 1
          and not terminal[0].get('is_error', False) and not invalid_lines)
if normal:
    literal = terminal[0].get('result')
    if not isinstance(literal, str) or not literal.strip():
        raise RuntimeError('Normal result lacks complete literal text')
    report = p / 'REVIEW.md'
else:
    literal = assistant[-1] if assistant else ''
    report = p / 'REVIEW-PARTIAL.md'
with report.open('xb') as f:
    f.write(literal.encode())
readback = {
    'recorded_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'head': launch['head'], 'base': launch['base'], 'tree': launch['tree'],
    'actual_exit': exit_record['exit_code'], 'elapsed_seconds': exit_record['elapsed_seconds'],
    'started_at': exit_record['started_at'], 'finished_at': exit_record['finished_at'],
    'normal_terminal_result': normal, 'result_rows': len(terminal),
    'result_is_error': terminal[0].get('is_error') if len(terminal) == 1 else None,
    'invalid_json_lines': invalid_lines,
    'source_unchanged_by_launcher': exit_record['source_unchanged'],
    'all543_inputs_modes_bytes_unchanged': len(verified) == 543,
    'verified_input_count': len(verified), 'verified_inputs': verified,
    'git_object_copy_readbacks': source_checks,
    'current_candidate_copy_count': len(manifest['source_files']),
    'requested_tools_only': sorted(set(t['name'] for t in tools)), 'tool_calls': tools,
    'tool_results': tool_results,
    'public_assistant_text_count': len(assistant),
    'literal_report_extraction': ('Exact JSON-decoded result.result UTF-8, without an added newline'
                                 if normal else 'Last assistant text is partial; no final approval exists'),
    'final_assistant_equals_terminal_result': bool(assistant) and assistant[-1] == literal if normal else None,
    'report': file_row(report),
    'files': [file_row(p / n) for n in ['stdout.jsonl', 'stderr.log', 'launch.json', 'exit.json',
                                     'input-binding.json', 'candidate-binding.json', 'prompt.txt',
                                     'prior-a9-EVIDENCE-APPENDIX.md', 'launch-review.py', 'audit-completion.py']],
    'no_review_rerun': True,
    'runtime_attribution': 'No new builds, tests, guests, or parity measurements in this review; aa7 results remain aa7, and original full pinned-image cat is pending after pre-admission refusal.'
}
with (p / 'COMPLETION-READBACK.json').open('x') as f:
    json.dump(readback, f, indent=2)
print(json.dumps({k: v for k, v in readback.items()
                  if k not in ['verified_inputs', 'git_object_copy_readbacks', 'tool_calls', 'tool_results', 'files']}, indent=2))
