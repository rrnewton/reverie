#!/usr/bin/python3
"""Create only the approved private destinations after execution authorization."""
import os
import stat
from pathlib import Path
from common import json_read, owned, require, write_new

HERE = Path(__file__).resolve().parent
setup = json_read(HERE/'SETUP.json')
target, lease, state = HERE/'target', HERE/'lane.lease', HERE/'lease-state'
require(setup['target'] == str(target) and setup['lease'] == str(lease)
        and setup['lease_state'] == str(state), 'private destinations differ')
directories = [target, state, HERE/'tmp', HERE/'cache', HERE/'controls', HERE/'loader', HERE/'launch']
for path in [*directories, lease, HERE/'TARGET-ADMISSION.json']:
    owned(path, HERE)
    require(not path.exists() and not path.is_symlink(), 'destination already exists: '+str(path))
for path in directories:
    path.mkdir(mode=0o700)
    info = path.stat()
    require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700
            and not list(path.iterdir()), 'private directory creation failed')
fd = os.open(lease, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
try:
    info = os.fstat(fd)
    require(stat.S_ISREG(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600
            and info.st_uid == os.getuid() and info.st_size == 0, 'private lease creation failed')
    lease_record = dict(path=str(lease), identity=[info.st_dev, info.st_ino, info.st_uid], mode=0o600, initially_empty=True)
finally:
    os.close(fd)
info = target.stat()
write_new(HERE/'TARGET-ADMISSION.json', dict(path=str(target), identity=[info.st_dev, info.st_ino, info.st_uid],
          mode=0o700, initially_empty=True, imports=[], private_lease=lease_record,
          directories=[str(p) for p in directories], created_by=str(Path(__file__).resolve())))
print(str(HERE/'TARGET-ADMISSION.json'))
