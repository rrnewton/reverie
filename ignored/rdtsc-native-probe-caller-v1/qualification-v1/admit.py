#!/usr/bin/python3
"""Root authorization required: admit only this caller's fresh private state."""
import os
from pathlib import Path
import sys
from common import check_file, file_record, json_read, require, write_new

HERE = Path(__file__).resolve().parent

def main():
    require(len(sys.argv)==2 and file_record(HERE/'CALLER-INPUTS.json')['sha256']==sys.argv[1],
            'admission requires exact root-inspected caller input digest')
    setup = json_read(HERE / 'SETUP.json')
    for row in json_read(HERE / 'CALLER-INPUTS.json')['records']:
        check_file(row)
    for name in ['target', 'tmp', 'loader', 'plans', 'controls', 'driver', 'retained-binaries', 'lease-state', 'private.lease', 'ADMISSION.json']:
        p = HERE / name
        require(not p.exists() and not p.is_symlink(), 'private state already exists: ' + name)
    for name in ['target', 'tmp', 'loader', 'plans', 'controls', 'driver', 'retained-binaries', 'lease-state']:
        (HERE / name).mkdir(mode=0o700)
    fd = os.open(HERE / 'private.lease', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
    os.fsync(fd)
    os.close(fd)
    target = HERE / 'target'
    require(not list(target.iterdir()), 'new target not empty')
    t = target.stat()
    lease = HERE / 'private.lease'
    l = lease.stat()
    require(str(target) == setup['private_target'] and str(lease) == setup['private_lease'], 'wrong admitted paths')
    write_new(HERE / 'ADMISSION.json', dict(initially_empty=True, target=dict(path=str(target), identity=[t.st_dev,t.st_ino,t.st_uid]),
        lease=dict(path=str(lease), owner_slot=str(HERE.parent), identity=[l.st_dev,l.st_ino,l.st_uid], state_directory=str(HERE/'lease-state')),
        shared_lane_lease_used=False, source=file_record(HERE.parent/'source/probe.c'), setup=file_record(HERE/'SETUP.json'),
        caller_manifest=file_record(HERE/'CALLER-INPUTS.json')))

if __name__ == '__main__':
    main()
