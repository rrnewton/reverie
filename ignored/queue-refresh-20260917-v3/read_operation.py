#!/usr/bin/python3
"""Bounded GitHub GETs through the existing proxy; retain exact raw outcomes."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
COMMANDS = {
    'hermit-open': 'repos/rrnewton/hermit/pulls?state=open&per_page=100&sort=created&direction=desc',
    'reverie-open': 'repos/rrnewton/reverie/pulls?state=open&per_page=100&sort=created&direction=desc',
    'reverie-578': 'repos/rrnewton/reverie/pulls/578',
    'hermit-main': 'repos/rrnewton/hermit/git/ref/heads/main',
    'reverie-main': 'repos/rrnewton/reverie/git/ref/heads/main',
}


def limits():
    resource.setrlimit(resource.RLIMIT_CPU, (15, 15))
    resource.setrlimit(resource.RLIMIT_FSIZE, (16*1024**2, 16*1024**2))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def main():
    name = sys.argv[1]
    if len(sys.argv) == 3:
        endpoint = sys.argv[2]
        assert endpoint.startswith(('repos/rrnewton/hermit/pulls/', 'repos/rrnewton/reverie/pulls/'))
    else:
        endpoint = COMMANDS[name]
    assert name.replace('-', '').isalnum()
    argv = ['/usr/bin/with-proxy', 'gh', 'api', '--method', 'GET', endpoint]
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    begin = time.monotonic()
    with (ROOT/(name+'.stdout')).open('xb') as out, (ROOT/(name+'.stderr')).open('xb') as err:
        process = subprocess.Popen(argv, cwd=ROOT, stdout=out, stderr=err,
                                   start_new_session=True, preexec_fn=limits)
        timed_out = False
        try:
            status = process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            status = process.wait(timeout=10)
    receipt = {'argv': argv, 'cwd': str(ROOT), 'pid': process.pid, 'started_at': started,
               'ended_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
               'wall_seconds': time.monotonic()-begin, 'exit_code': status, 'timed_out': timed_out,
               'bounds': {'cpu_seconds_per_process': 15, 'wall_seconds': 60, 'address_space_limit': 'inherited; no added RLIMIT_AS',
                          'bytes_per_stream': 16*1024**2, 'core_bytes': 0}}
    for channel in ['stdout', 'stderr']:
        data = (ROOT/(name+'.'+channel)).read_bytes()
        receipt[channel+'_sha256'] = hashlib.sha256(data).hexdigest()
        receipt[channel+'_bytes'] = len(data)
    with (ROOT/(name+'.receipt.json')).open('x') as stream:
        json.dump(receipt, stream, indent=2)
        stream.write('\n')
    print(json.dumps(receipt))
    if status != 0 or timed_out:
        raise SystemExit(status or 1)


if __name__ == '__main__':
    main()
