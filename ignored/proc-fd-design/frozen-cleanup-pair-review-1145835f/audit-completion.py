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
assert len(inputs) == 80
assert len({r['path'] for r in inputs}) == 80
assert sha((p / 'input-binding.json').read_bytes()) == '34ead62e941a9c9b1f0e3a5e8495e1c6da4cf28360c6b67ec5303a9010afe9f9'
verified = []
for expected in inputs:
    actual = file_row(Path(expected['path']))
    if actual != expected:
        raise RuntimeError('Review input drift: ' + expected['path'])
    verified.append(actual)

candidate = json.loads((p / 'candidate-binding.json').read_text())
source_rows = [{**r, 'path': str(Path(candidate['root']) / r['path'])} for r in candidate['files']]
assert len(source_rows) == 25
env = {**os.environ, 'GIT_NO_LAZY_FETCH': '1', 'GIT_OPTIONAL_LOCKS': '0'}
source_checks = []
for r in source_rows:
    o = {'repository': r['repository'], 'revision': r['revision'], 'git_blob': r['git_blob'], 'git_mode': r['git_mode'], 'path': r['git_path']}
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
assert launch['head'] == exit_record['head'] == '1145835fc804f47ae48b29b89009e6937184175a'
assert launch['tree'] == exit_record['tree'] == 'e470a86697cbb9590c23d45ee5fab9ade12aab01'
assert launch['base'] == exit_record['base'] == '8720799f78f8ef56d796723dbc473a9171a4f4eb'
assert launch['input_binding_sha256'] == sha((p / 'input-binding.json').read_bytes())
assert launch['prompt_sha256'] == sha((p / 'prompt.txt').read_bytes())
assert launch['wall_seconds'] == 900 and launch['kill_after_seconds'] == 10
assert launch['per_output_file_bytes'] == 67108864
actual_tree = subprocess.check_output(['git', 'rev-parse', launch['head'] + '^{tree}'],
                                     cwd=candidate['repositories']['agent-utils']['repository'], env=env).decode().strip()
assert actual_tree == launch['tree']
assert launch['repositories'] == exit_record['repositories'] == candidate['repositories']
for name, repository in candidate['repositories'].items():
    tree = subprocess.check_output(['git', 'rev-parse', repository['head'] + '^{tree}'], cwd=repository['repository'], env=env).decode().strip()
    parent = subprocess.check_output(['git', 'rev-parse', repository['head'] + '^'], cwd=repository['repository'], env=env).decode().strip()
    assert tree == repository['tree'] and parent == repository['base']

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
    'repositories': launch['repositories'],
    'actual_exit': exit_record['exit_code'], 'elapsed_seconds': exit_record['elapsed_seconds'],
    'started_at': exit_record['started_at'], 'finished_at': exit_record['finished_at'],
    'normal_terminal_result': normal, 'result_rows': len(terminal),
    'result_is_error': terminal[0].get('is_error') if len(terminal) == 1 else None,
    'invalid_json_lines': invalid_lines,
    'source_unchanged_by_launcher': exit_record['source_unchanged'],
    'all80_inputs_modes_bytes_unchanged': len(verified) == 80,
    'verified_input_count': len(verified), 'verified_inputs': verified,
    'git_object_copy_readbacks': source_checks,
    'current_candidate_copy_count': len(source_rows),
    'requested_tools_only': sorted(set(t['name'] for t in tools)), 'tool_calls': tools,
    'tool_results': tool_results,
    'public_assistant_text_count': len(assistant),
    'literal_report_extraction': ('Exact JSON-decoded result.result UTF-8, without an added newline'
                                 if normal else 'Last assistant text is partial; no final approval exists'),
    'final_assistant_equals_terminal_result': bool(assistant) and assistant[-1] == literal if normal else None,
    'report': file_row(report),
    'files': [file_row(p / n) for n in ['stdout.jsonl', 'stderr.log', 'launch.json', 'exit.json',
                                     'input-binding.json', 'candidate-binding.json', 'prompt.txt',
                                     'EVIDENCE.md', 'launch-review.py', 'audit-completion.py']],
    'no_review_rerun': True,
    'runtime_attribution': 'No new builds, tests, guests, registry commands, admission or cleanup in this review. AU focused43/mypy13 and parent28+4 observations remain exactly attributed; normal AU make validate was pending at freeze. Actual1839 remains a retained no-result, not a removal proof.'
}
with (p / 'COMPLETION-READBACK.json').open('x') as f:
    json.dump(readback, f, indent=2)
print(json.dumps({k: v for k, v in readback.items()
                  if k not in ['verified_inputs', 'git_object_copy_readbacks', 'tool_calls', 'tool_results', 'files']}, indent=2))
