import datetime, hashlib, json, os, resource, subprocess, sys, time
from pathlib import Path

p = Path(__file__).resolve().parent
# Disabled by default. The explicit argument is an accidental-launch guard,
# not authorization; root must review/release the exact package first.
if sys.argv[1:] != ['--execute-reviewed-a88f9480']:
    raise SystemExit('review launch is disabled pending root review; no subprocess started')
source = json.loads((p / 'candidate-binding.json').read_text())
inputs = json.loads((p / 'input-binding.json').read_text())

def source_check():
    for row in inputs:
        q = Path(row['path'])
        data = q.read_bytes()
        if (len(data) != row['bytes'] or hashlib.sha256(data).hexdigest() != row['sha256']
                or oct(q.stat().st_mode & 0o777) != row['mode']):
            raise RuntimeError('review input changed: ' + str(q))
    for row in source['files']:
        q = Path(source['root']) / row['path']
        data = q.read_bytes()
        if (len(data) != row['bytes'] or hashlib.sha256(data).hexdigest() != row['sha256']
                or oct(q.stat().st_mode & 0o777) != row['mode']):
            raise RuntimeError('candidate source changed: ' + str(q))

source_check()
cmd = ['timeout', '--signal=TERM', '--kill-after=10s', '900s', 'with-proxy',
       'claude', '-p', '--no-session-persistence', '--output-format', 'stream-json',
       '--verbose', '--permission-mode', 'dontAsk', '--permission-prompts', 'none',
       '--tools', 'Read,Grep,Glob', '--allowedTools', 'Read,Grep,Glob', '--strict-mcp-config']
record = {'command': cmd, 'cwd': str(Path(source['root']) / 'hermit'),
          'base': source['base'], 'head': source['head'], 'tree': source['tree'],
          'input_binding_sha256': hashlib.sha256((p / 'input-binding.json').read_bytes()).hexdigest(),
          'source_patch_sha256': source['candidate_patch_sha256'],
          'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
          'wall_seconds': 900, 'kill_after_seconds': 10, 'per_output_file_bytes': 67108864,
          'prompt_sha256': hashlib.sha256((p / 'prompt.txt').read_bytes()).hexdigest()}
with (p / 'launch.json').open('x') as f:
    json.dump(record, f, indent=2)

def limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (67108864, 67108864))

start = time.monotonic()
with (p / 'stdout.jsonl').open('xb') as out, (p / 'stderr.log').open('xb') as err:
    proc = subprocess.run(cmd, input=(p / 'prompt.txt').read_bytes(), cwd=record['cwd'],
                          stdout=out, stderr=err, preexec_fn=limits)
record.update(exit_code=proc.returncode, elapsed_seconds=time.monotonic()-start,
              finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
try:
    source_check()
    record['source_unchanged'] = True
except Exception as e:
    record['source_unchanged'] = False
    record['source_error'] = str(e)
with (p / 'exit.json').open('x') as f:
    json.dump(record, f, indent=2)
print(json.dumps(record), flush=True)
