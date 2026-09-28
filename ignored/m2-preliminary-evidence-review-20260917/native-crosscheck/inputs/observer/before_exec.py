#!/usr/bin/python3
"""Wait for this service's observer before executing its exact frozen payload."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import resource
import signal
import socket
import stat
import struct
import sys
import time

MAX_MESSAGE = 65536


def require(value, reason):
    if not value:
        raise RuntimeError(reason)


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def digest_fd(fd):
    value = hashlib.sha256()
    os.lseek(fd, 0, os.SEEK_SET)
    while chunk := os.read(fd, 1024 * 1024):
        value.update(chunk)
    os.lseek(fd, 0, os.SEEK_SET)
    return value.hexdigest()


def process_start(pid):
    text = Path('/proc', str(pid), 'stat').read_text()
    return int(text[text.rindex(')') + 2:].split()[19])


def credentials(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))


def receive(connection, deadline):
    remaining = deadline - time.monotonic()
    require(remaining > 0, 'before-exec receipt deadline expired')
    connection.settimeout(remaining)
    data, ancillary, flags, address = connection.recvmsg(MAX_MESSAGE)
    require(data and not ancillary and not flags, 'before-exec receipt missing or truncated')
    return json.loads(data)


def send(connection, value, deadline):
    remaining = deadline - time.monotonic()
    require(remaining > 0, 'before-exec send deadline expired')
    connection.settimeout(remaining)
    data = encoded(value)
    require(len(data) < MAX_MESSAGE, 'before-exec message exceeds bound')
    require(connection.send(data) == len(data), 'before-exec message incomplete')


def main():
    # These temporary soft limits bound even safehermit's no-systemd branch.
    # Their original hard limits are retained so the exact inherited limits can
    # be restored before exec. A failed restoration refuses execution.
    inherited = {name: resource.getrlimit(kind) for name, kind in
                 [('cpu', resource.RLIMIT_CPU), ('as', resource.RLIMIT_AS), ('core', resource.RLIMIT_CORE)]}
    for name, kind, temporary in [('cpu', resource.RLIMIT_CPU, 2),
                                   ('as', resource.RLIMIT_AS, 512 * 1024**2),
                                   ('core', resource.RLIMIT_CORE, 0)]:
        soft, hard = inherited[name]
        resource.setrlimit(kind, (temporary if soft == resource.RLIM_INFINITY else min(soft, temporary), hard))
    require(ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0, 'before-exec dumpability could not be disabled')
    def expired(_number, _frame):
        raise RuntimeError('before-exec independent deadline expired')
    signal.signal(signal.SIGALRM, expired)
    signal.alarm(25)
    signal.signal(signal.SIGXCPU, expired)
    require(len(sys.argv) == 3, 'before-exec requires exact specification path and hash')
    spec_path = Path(sys.argv[1])
    require(spec_path.is_absolute() and not spec_path.is_symlink(), 'before-exec specification path invalid')
    with spec_path.open('rb') as stream:
        spec_bytes = stream.read(MAX_MESSAGE + 1)
    require(len(spec_bytes) < MAX_MESSAGE and hashlib.sha256(spec_bytes).hexdigest() == sys.argv[2],
            'before-exec specification identity changed')
    spec = json.loads(spec_bytes)
    deadline = min(spec['deadline_monotonic_ns'] / 1e9, time.monotonic() + 25)
    require(time.monotonic() < deadline, 'before-exec specification expired')
    require(process_start(spec['observer_pid']) == spec['observer_start_ticks'], 'before-exec observer generation changed')
    # Python may coerce LC_CTYPE during startup. Refuse that change rather
    # than repairing the caller's environment or hiding the extra input.
    locale_environment = {key: os.environ.get(key) for key in ['LC_CTYPE', 'LC_ALL', 'LANG']}
    require(locale_environment == spec['locale_environment'], 'before-exec inherited locale environment changed')
    environment = dict(os.environb)
    payload = spec['payload']
    require(payload['binary'].startswith('/') and isinstance(payload['arguments'], list), 'before-exec payload argv invalid')
    payload_fd = os.open(payload['binary'], os.O_RDONLY | os.O_CLOEXEC)
    require(stat.S_ISREG(os.fstat(payload_fd).st_mode), 'before-exec payload is not a regular file')
    require(digest_fd(payload_fd) == payload['binary_sha256'], 'before-exec payload hash differs')
    directory_fd = os.open(spec_path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    connection.settimeout(max(.001, deadline - time.monotonic()))
    # Bind/connect a relative pathname while temporarily in the private
    # output directory, then restore the exact host cwd. This avoids both the
    # sockaddr_un path limit and procfs permission assumptions.
    original_cwd_fd = os.open('.', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fchdir(directory_fd)
        connection.connect('before-exec.sock')
    finally:
        os.fchdir(original_cwd_fd)
        os.close(original_cwd_fd)
    peer = credentials(connection)
    require(peer == (spec['observer_pid'], os.getuid(), os.getgid()), 'before-exec observer peer differs')
    request = {'phase': 'ready', 'spec_sha256': sys.argv[2], 'token': spec['token'],
               'pid': os.getpid(), 'start_ticks': process_start(os.getpid()),
               'payload': payload}
    send(connection, request, deadline)
    expected = {'phase': 'exec', 'spec_sha256': sys.argv[2], 'token': spec['token'],
                'pid': os.getpid(), 'start_ticks': request['start_ticks'], 'payload': payload}
    require(receive(connection, deadline) == expected, 'before-exec authorization differs')
    require(process_start(spec['observer_pid']) == spec['observer_start_ticks'], 'before-exec observer generation changed before release')
    require(digest_fd(payload_fd) == payload['binary_sha256'], 'before-exec payload changed before release')
    # The kernel-held opened executable avoids path replacement between the
    # final hash and exec. argv[0] remains the requested absolute payload path.
    require(os.execve in os.supports_fd, 'before-exec requires fd-based execve')
    for name, kind in [('cpu', resource.RLIMIT_CPU), ('as', resource.RLIMIT_AS), ('core', resource.RLIMIT_CORE)]:
        resource.setrlimit(kind, inherited[name])
        require(resource.getrlimit(kind) == inherited[name], 'before-exec could not restore inherited resource limits')
    signal.alarm(0)
    signal.signal(signal.SIGALRM, signal.SIG_DFL)
    signal.signal(signal.SIGXCPU, signal.SIG_DFL)
    # CPython ignores SIGXFSZ (and SIGXFZ where available). Restore the
    # normal exec disposition; systemd's independently checked IgnoreSIGPIPE
    # property is the sole intentional ignored signal from the service.
    for name in ['SIGXFSZ', 'SIGXFZ']:
        if hasattr(signal, name):
            signal.signal(getattr(signal, name), signal.SIG_DFL)
    require(spec['ignore_sigpipe'] is True, 'before-exec service SIGPIPE contract missing')
    signal.signal(signal.SIGPIPE, signal.SIG_IGN)
    connection.close()
    os.close(directory_fd)
    receipt = {'authorization_received': True, 'payload': payload, 'pid': os.getpid(),
               'start_ticks': request['start_ticks'], 'restored_limits': inherited,
               'protocol_environment_keys': [], 'handshake_fds_closed': True,
               'ignore_sigpipe': True, 'python_file_size_signals_restored_to_default': True,
               'environment_sha256': hashlib.sha256(encoded(sorted((k.hex(), v.hex()) for k, v in environment.items()))).hexdigest(),
               'monotonic_ns': time.monotonic_ns(), 'note': 'Written before exec; this is not proof that the payload started.'}
    with (spec_path.parent / 'before-exec-receipt.json').open('x') as stream:
        json.dump(receipt, stream, indent=2)
        stream.write('\n')
    os.execve(payload_fd, [payload['binary'], *payload['arguments']], environment)


if __name__ == '__main__':
    try:
        main()
    except BaseException as error:
        print('before-exec refusal: ' + type(error).__name__ + ': ' + str(error), file=sys.stderr, flush=True)
        sys.exit(126)
