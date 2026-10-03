#!/usr/bin/env python3
"""Bound this slot's one serial command and retain exact results/descendant cleanup."""
import argparse, ctypes, hashlib, json, os, pathlib, selectors, signal, subprocess, time
from datetime import datetime
from zoneinfo import ZoneInfo

parser = argparse.ArgumentParser()
parser.add_argument('--phase', required=True)
parser.add_argument('--seconds', type=float, required=True)
parser.add_argument('command', nargs=argparse.REMAINDER)
a = parser.parse_args()
command = a.command[1:] if a.command[:1] == ['--'] else a.command
assert command and a.phase.replace('-', '').isalnum() and 0 < a.seconds <= 3600
slot = pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-pselect-capture-repair-20261003')
evidence = slot / 'ignored/pselect-capture-qualification-20261003' / a.phase
evidence.mkdir(exist_ok=False)

def available():
    s = os.statvfs('/data')
    return s.f_bavail * s.f_frsize

def source_binding():
    paths = ['Cargo.lock', 'reverie-kvm/src/executor.rs', 'reverie-kvm/tests/static_elf.rs',
             'reverie-kvm/tests/support/pselect_zero.rs', 'reverie-kvm/tests/fixtures/pselect_zero.c',
             'reverie-kvm/tests/fixtures/pselect_captured_output.c']
    return {p: hashlib.sha256((slot / p).read_bytes()).hexdigest() for p in paths}

def stat(pid):
    s = pathlib.Path(f'/proc/{pid}/stat').read_text()
    fields = s[s.rfind(')') + 2:].split()
    return {'pid': pid, 'state': fields[0], 'ppid': int(fields[1]), 'pgid': int(fields[2]),
            'session': int(fields[3]), 'start': int(fields[19])}

def descendants():
    rows = {}
    for e in os.scandir('/proc'):
        if e.name.isdecimal():
            try:
                row = stat(int(e.name))
            except (FileNotFoundError, ProcessLookupError):
                continue
            rows[row['pid']] = row
    owned = {os.getpid()}
    while True:
        next_owned = owned | {pid for pid, row in rows.items() if row['ppid'] in owned}
        if next_owned == owned:
            break
        owned = next_owned
    return [rows[pid] for pid in sorted(owned - {os.getpid()}) if pid in rows]

def signal_exact(row, sig):
    try:
        fd = os.pidfd_open(row['pid'])
        try:
            current = stat(row['pid'])
            if current['start'] == row['start'] and current['state'] != 'Z':
                signal.pidfd_send_signal(fd, sig)
        finally:
            os.close(fd)
    except (FileNotFoundError, ProcessLookupError):
        pass

free = available()
cpus = sorted(os.sched_getaffinity(0))[:8]
record = {'phase': a.phase, 'command': command, 'cwd': str(slot), 'start_free_bytes': free,
          'cpu_affinity': cpus, 'nice': 10, 'seconds_bound': a.seconds,
          'started_et': datetime.now(ZoneInfo('America/New_York')).strftime('%Y-%m-%d %I:%M:%S %p ET'),
          'source_before': source_binding(),
          'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=slot, text=True).strip()}
(evidence / 'source.patch').write_bytes(subprocess.check_output(['git', 'diff', '--binary', 'HEAD'], cwd=slot))
if free < 280_000_000_000:
    record.update({'refusal': 'disk below 280000000000-byte admission floor', 'rc': 91})
    (evidence / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record), flush=True)
    raise SystemExit(91)
# Adopt/reap only descendants of this one wrapper if the command double-forks.
assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0
cancellation = []
for sig in (signal.SIGINT, signal.SIGTERM):
    signal.signal(sig, lambda n, _frame: cancellation.append(n))

def child_limits():
    os.sched_setaffinity(0, cpus)
    os.nice(10)

env = os.environ.copy()
env.update({'CARGO_TARGET_DIR': str(slot / 'target/pselect-capture-qualification'),
            'CARGO_BUILD_JOBS': '8', 'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': '0',
            'CARGO_PROFILE_TEST_DEBUG': '0',
            'XDG_CACHE_HOME': str(slot / 'target/pselect-capture-script-cache'),
            'REVERIE_REQUIRE_KVM': '1'})
record['resource_environment'] = {k: env[k] for k in ['CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS',
    'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG', 'CARGO_PROFILE_TEST_DEBUG', 'XDG_CACHE_HOME', 'REVERIE_REQUIRE_KVM']}
start = time.monotonic()
reason = None
minimum = free
log = evidence / 'output.log'
limit = 128 * 1024 * 1024
written = 0
witnessed = {}
child = None
cleanup = []
try:
    child = subprocess.Popen(command, cwd=slot, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             start_new_session=True, preexec_fn=child_limits)
    record['leader'] = stat(child.pid)
    sel = selectors.DefaultSelector()
    sel.register(child.stdout, selectors.EVENT_READ)
    last_disk = 0.0
    with log.open('wb') as out:
        while child.poll() is None or sel.get_map():
            now = time.monotonic()
            if now - last_disk >= 1:
                disk = available()
                minimum = min(minimum, disk)
                last_disk = now
                for row in descendants():
                    witnessed[(row['pid'], row['start'])] = row
                if disk < 265_000_000_000:
                    reason = 'disk below 265000000000-byte stop floor'
            if cancellation:
                reason = 'wrapper received cancellation signal'
            if now - start > a.seconds:
                reason = 'command exceeded its declared wall bound'
            if reason:
                break
            for key, _ in sel.select(timeout=0.2):
                data = os.read(key.fd, 65536)
                if not data:
                    sel.unregister(key.fileobj)
                else:
                    retained = data[:max(0, limit - written)]
                    out.write(retained)
                    written += len(retained)
                    if len(retained) != len(data):
                        reason = 'command output exceeded 128 MiB'
                        break
            if child.poll() is not None and sel.get_map():
                # A descendant retaining the stream cannot outlive the same wall bound.
                continue
    sel.close()
except BaseException as exc:
    reason = f'wrapper exception: {type(exc).__name__}: {exc}'
finally:
    # Normal leader exit is not proof of retirement. Census/reap descendants;
    # any live leftover is a failed phase, even if its leader reported success.
    if reason:
        # Known generation-bound ownership must not depend on a fresh census.
        known = dict(witnessed)
        if 'leader' in record:
            row = record['leader']
            known[(row['pid'], row['start'])] = row
        for row in known.values():
            try:
                signal_exact(row, signal.SIGTERM)
            except OSError as exc:
                cleanup.append({'known_signal_error': repr(exc), 'target': row})
    try:
        remaining = descendants()
        if remaining and reason is None:
            live = [r for r in remaining if r['state'] != 'Z']
            if live:
                reason = 'live descendants remained after command completion'
        for sig, bound in [(signal.SIGTERM, 5), (signal.SIGKILL, 5)]:
            for row in remaining:
                witnessed[(row['pid'], row['start'])] = row
                signal_exact(row, sig)
            cleanup.append({'signal': int(sig), 'targets': remaining})
            end = time.monotonic() + bound
            while time.monotonic() < end:
                if child is not None:
                    child.poll()
                # Popen has reaped the command leader; reap only adopted children.
                if child is None or child.returncode is not None:
                    while True:
                        try:
                            pid, _ = os.waitpid(-1, os.WNOHANG)
                        except ChildProcessError:
                            break
                        if pid == 0:
                            break
                remaining = descendants()
                if not remaining:
                    break
                time.sleep(0.1)
            if not remaining:
                break
        record['remaining_descendants'] = remaining
        if remaining:
            reason = reason or 'descendant retirement incomplete'
    except BaseException as exc:
        reason = reason or f'descendant cleanup exception: {type(exc).__name__}: {exc}'
        record['cleanup_exception'] = repr(exc)
        for row in list(witnessed.values()) + ([record['leader']] if 'leader' in record else []):
            try:
                signal_exact(row, signal.SIGKILL)
            except OSError as signal_error:
                cleanup.append({'known_signal_error': repr(signal_error), 'target': row})
        if child is not None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                record['leader_reap_timeout'] = True
    record['cleanup'] = cleanup
    record['witnessed_descendants'] = list(witnessed.values())
    record['actual_command_rc'] = None if child is None else child.poll()
    if child is not None and child.stdout is not None:
        child.stdout.close()
    record['source_after'] = source_binding()
    if record['source_after'] != record['source_before']:
        reason = reason or 'source changed during phase'
    record.update({'stop_reason': reason, 'wall_seconds': time.monotonic() - start,
                   'minimum_free_bytes': minimum, 'end_free_bytes': available(),
                   'output_bytes': log.stat().st_size if log.exists() else 0,
                   'output_sha256': hashlib.sha256(log.read_bytes()).hexdigest() if log.exists() else None,
                   'cancel_signals': cancellation})
    rc = record['actual_command_rc']
    record['rc'] = 94 if reason or rc is None else (rc if rc >= 0 else 128 - rc)
    binaries = {}
    deps = slot / 'target/pselect-capture-qualification/debug/deps'
    if deps.is_dir():
        for entry in deps.iterdir():
            if entry.is_file() and entry.stat().st_mode & 0o111 and entry.name.startswith(('reverie_kvm-', 'static_elf-')):
                binaries[str(entry.relative_to(slot))] = hashlib.sha256(entry.read_bytes()).hexdigest()
    record['test_binaries'] = binaries
    (evidence / 'RESULT.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps({k: record[k] for k in ['phase','rc','actual_command_rc','stop_reason','wall_seconds','minimum_free_bytes','output_bytes']}), flush=True)
raise SystemExit(record['rc'])
