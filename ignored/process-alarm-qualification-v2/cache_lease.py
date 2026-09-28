"""Lane-owned admission lease; advisory ownership, not arbitrary-writer immunity."""
import fcntl
import json
import os
from pathlib import Path
import stat
from common import check_file, file_record, json_read, owned, require, write_new


def open_bound(binding):
    path = owned(Path(binding['path']), Path(binding['owner_slot']))
    fd = os.open(path, os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600,
                'lane lease must be a regular private file')
        require([info.st_dev, info.st_ino, info.st_uid] == binding['identity'],
                'lane lease identity changed')
        current = path.lstat()
        require((current.st_dev, current.st_ino) == (info.st_dev, info.st_ino),
                'lane lease path no longer names held inode')
        return fd
    except BaseException:
        os.close(fd)
        raise


def token(plan_sha):
    require(len(plan_sha) == 64 and all(c in '0123456789abcdef' for c in plan_sha),
            'invalid lease plan binding')
    return json.dumps({'schema': 1, 'plan_sha256': plan_sha}, sort_keys=True).encode() + b'\n'


def check_held(fd, binding, plan_sha):
    info = os.fstat(fd)
    path = Path(binding['path']).lstat()
    require([info.st_dev, info.st_ino, info.st_uid] == binding['identity'] and
            (path.st_dev, path.st_ino) == (info.st_dev, info.st_ino), 'held lease replaced')
    require(stat.S_IMODE(info.st_mode) == 0o600 and os.pread(fd, 4096, 0) == token(plan_sha),
            'lane lease token or mode changed')


def claim(binding, plan_sha):
    """Exclusive nonblocking admission, then retain shared hold through readback."""
    fd = open_bound(binding)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        previous = os.pread(fd, 4096, 0)
        if previous:
            prior = json.loads(previous)
            require(previous == token(prior['plan_sha256']), 'malformed prior lease token')
            state = owned(Path(binding['state_directory']), Path(binding['owner_slot']), directory=True)
            completion = json_read(state / (prior['plan_sha256'] + '.json'))
            require(completion['plan_sha256'] == prior['plan_sha256'], 'prior completion identity differs')
            check_file(completion['result']); result = json_read(completion['result']['path'])
            require(result.get('terminal_authenticated') is True or result.get('observer_attempted') is False,
                    'previous phase has no authenticated terminal or no-dispatch result')
            if result.get('terminal_authenticated'):
                check_file(result['observer_result'])
                observed = json_read(result['observer_result']['path'])
                final = observed.get('final_accounting') or observed.get('refusal_final_accounting')
                require(final and final['cgroup_empty'] and final['properties']['MainPID'] == 0 and
                        final['properties']['ControlGroup'] == '' and
                        final['properties']['ActiveState'] in ('inactive','failed'), 'previous service not terminal')
        data = token(plan_sha)
        os.ftruncate(fd, 0)
        require(os.pwrite(fd, data, 0) == len(data), 'short lease token write')
        os.fsync(fd)
        # Another claim always needs EX, including while the observed child
        # later holds SH. The exact token closes the launcher-death handoff gap.
        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        check_held(fd, binding, plan_sha)
        return fd
    except BaseException:
        os.close(fd)
        raise


def join(binding, plan_sha):
    """Observed payload retains the same phase's hold if its launcher exits."""
    fd = open_bound(binding)
    try:
        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        check_held(fd, binding, plan_sha)
        return fd
    except BaseException:
        os.close(fd)
        raise


def complete(fd, binding, plan_sha, result_path):
    """Persist terminal proof; an available lease without this record refuses."""
    check_held(fd, binding, plan_sha)
    result = json_read(result_path)
    if result.get('terminal_authenticated') is not True and result.get('observer_attempted') is not False:
        return  # Deliberately leave unresolved for authenticated explicit recovery.
    state = owned(Path(binding['state_directory']), Path(binding['owner_slot']), directory=True)
    write_new(state / (plan_sha + '.json'), dict(plan_sha256=plan_sha, result=file_record(result_path)))
