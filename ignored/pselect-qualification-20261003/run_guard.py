#!/usr/bin/env python3
"""Bound one owned qualification command; preserve its actual result."""
import argparse, hashlib, json, os, pathlib, signal, subprocess, time
from datetime import datetime
from zoneinfo import ZoneInfo

parser = argparse.ArgumentParser()
parser.add_argument('--phase', required=True)
parser.add_argument('--seconds', type=float, required=True)
parser.add_argument('command', nargs=argparse.REMAINDER)
a = parser.parse_args()
command = a.command[1:] if a.command[:1] == ['--'] else a.command
assert command and a.phase.replace('-', '').isalnum()
slot = pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-pselect-zero-20261003')
evidence = slot / 'ignored/pselect-qualification-20261003' / a.phase
evidence.mkdir(exist_ok=False)
free = int(subprocess.check_output(['df', '-B1', '--output=avail', '/data'], text=True).splitlines()[-1])
cpus = sorted(os.sched_getaffinity(0))
cpus = cpus[:max(1, min(8, len(cpus) // 2))]
record = {'phase': a.phase, 'command': command, 'cwd': str(slot), 'start_free_bytes': free,
          'cpu_affinity': cpus, 'nice': 10, 'seconds_bound': a.seconds,
          'started_et': datetime.now(ZoneInfo('America/New_York')).strftime('%Y-%m-%d %I:%M:%S %p ET')}
if free < 280_000_000_000:
    record['refusal'] = 'disk below 280000000000-byte admission floor'
    record['rc'] = 91
    (evidence / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    raise SystemExit(91)
def child_limits():
    os.sched_setaffinity(0, cpus)
    os.nice(10)
env = os.environ.copy()
env.update({'CARGO_TARGET_DIR': str(slot / 'target/pselect620-qualification'),
            'CARGO_BUILD_JOBS': '8', 'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': '0',
            'XDG_CACHE_HOME': str(slot / 'target/pselect620-script-cache'),
            'REVERIE_REQUIRE_KVM': '1'})
start = time.monotonic()
reason = None
log = evidence / 'output.log'
minimum = free
with log.open('wb') as out:
    child = subprocess.Popen(command, cwd=slot, env=env, stdout=out, stderr=subprocess.STDOUT,
                             start_new_session=True, preexec_fn=child_limits)
    record['pid'] = child.pid
    while child.poll() is None:
        disk = os.statvfs('/data')
        available = disk.f_bavail * disk.f_frsize
        minimum = min(minimum, available)
        if available < 265_000_000_000:
            reason = 'disk below 265000000000-byte stop floor'
        elif time.monotonic() - start > a.seconds:
            reason = 'command exceeded its declared wall bound'
        elif log.stat().st_size > 128 * 1024 * 1024:
            reason = 'command output exceeded 128 MiB'
        if reason:
            # The unreaped Popen leader pins this pid/generation while its owned group is stopped.
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
            break
        time.sleep(1)
    rc = child.wait()
record.update({'rc': rc, 'stop_reason': reason, 'wall_seconds': time.monotonic() - start,
               'minimum_free_bytes': minimum,
               'end_free_bytes': int(subprocess.check_output(['df', '-B1', '--output=avail', '/data'], text=True).splitlines()[-1]),
               'output_bytes': log.stat().st_size, 'output_sha256': hashlib.sha256(log.read_bytes()).hexdigest()})
(evidence / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record), flush=True)
raise SystemExit(rc if rc >= 0 else 128 - rc)
