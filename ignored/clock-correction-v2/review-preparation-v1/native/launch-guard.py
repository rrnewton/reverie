"""Source-only prepared wrapper; invoking it launches the separately authorized review."""
from pathlib import Path
import datetime
import hashlib
import json
import os
import stat
import subprocess
import sys

P = Path(__file__).resolve().parent
STAGE = '01-source'
SESSION = '01a0b38e-9e3c-7883-9816-65199c048935'


def record(path):
    p = Path(path)
    if p.is_symlink() or not stat.S_ISREG(p.lstat().st_mode):
        raise RuntimeError(f'Input is not an ordinary regular file: {p}')
    h = hashlib.sha256()
    with p.open('rb') as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(data)
    return {'path': str(p), 'bytes': p.stat().st_size,
            'sha256': h.hexdigest(), 'mode': stat.S_IMODE(p.stat().st_mode)}


def put(name, value):
    with (P / name).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def snapshot(bound):
    for row in bound.get('symlinks', []):
        p = Path(row['path'])
        if not p.is_symlink() or os.readlink(p) != row['target']:
            raise RuntimeError(f'Input alias changed: {p}')
    rows = []
    for row in bound['files']:
        actual = record(row['path'])
        if actual != row:
            raise RuntimeError(f'Input bytes/mode changed: {row["path"]}')
        rows.append(actual)
    return rows


def main():
    if sys.flags.optimize != 0:
        raise RuntimeError('This caller requires Python optimization disabled')
    target = json.loads((P / 'TARGET.json').read_text())
    if target['session_id'] != SESSION or target['stage'] != STAGE:
        raise RuntimeError('Unexpected session or stage')
    # run_stage retains its established output names and monitoring. Prevent its
    # write_text/wb operations from replacing any prior attempt before invoking it.
    names = [STAGE + suffix for suffix in (
        '.capacity-before.json', '.REFUSED.json', '.launch.json', '.jsonl',
        '.stderr', '.final.txt', '.result.json', '.capacity-after.json')]
    names += ['source-before.json', 'source-after.json', 'guard-launch.json',
              'guard-result.json']
    if any((P / name).exists() or (P / name).is_symlink() for name in names):
        raise RuntimeError('Attempt already exists; preserve it and prepare a successor')
    bound = json.loads((P / 'inputs.json').read_text())
    before = snapshot(bound)
    put('source-before.json', before)
    command = ['/usr/bin/python3', '-B', str(P / 'run_stage.py'), STAGE, SESSION]
    put('guard-launch.json', {
        'at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'command': command, 'cwd': str(P), 'session_id': SESSION,
        'input_index': record(P / 'inputs.json'), 'inputs': len(before),
        'limits': 'Unchanged run_stage.py: 1800 seconds wall; 64 MiB combined raw outputs; fresh all-UID cap 15 and exact-session refusal',
        'permission': 'Root must authorize this prepared caller before invoking it; preparation is not authorization'})
    raw_status = None
    launch_error = None
    try:
        raw_status = subprocess.run(command, cwd=P).returncode
    except Exception as error:
        launch_error = repr(error)
    after = None
    after_error = None
    try:
        after = snapshot(bound)
        put('source-after.json', after)
    except Exception as error:
        after_error = repr(error)
    put('guard-result.json', {
        'at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'raw_runner_status': raw_status, 'launch_error': launch_error,
        'after_snapshot_error': after_error,
        'inputs_unchanged': after is not None and before == after,
        'scope': 'Launch/source custody only. Raw exit zero is not coverage, a verdict, or a design approval.'})
    if launch_error or after_error or before != after:
        return 1
    return raw_status


if __name__ == '__main__':
    raise SystemExit(main())
