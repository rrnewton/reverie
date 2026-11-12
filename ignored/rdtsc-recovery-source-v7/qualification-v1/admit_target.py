#!/usr/bin/python3
"""Create only the approved empty target and phase directories, after authorization."""
import os
from pathlib import Path
from common import json_read, require, write_new
HERE=Path(__file__).resolve().parent
OWNER=HERE.parents[2]
setup=json_read(HERE/'SETUP.json')
target=Path(setup['target'])
require(target==OWNER/'target/rdtsc-recovery-v7-cold-v1','wrong target')
require(not target.exists() and not target.is_symlink(),'target already exists; preserve and refuse')
require(target.parent.is_dir() and not target.parent.is_symlink(),'target parent absent or aliased')
target.mkdir(mode=0o700)
s=target.stat()
require(s.st_uid==os.getuid() and (s.st_mode&0o777)==0o700 and not list(target.iterdir()),'target creation binding')
write_new(HERE/'TARGET-ADMISSION.json',dict(path=str(target),identity=[s.st_dev,s.st_ino,s.st_uid],mode=s.st_mode&0o777,initially_empty=True,imports=[],created_by=str(Path(__file__).resolve())))
for directory in [HERE/'tmp',HERE/'cache',HERE/'controls',HERE/'loader',HERE.parent/'launch']:
    require(not directory.exists() and not directory.is_symlink(),'phase destination already exists: '+str(directory))
    directory.mkdir(mode=0o700)
print(str(HERE/'TARGET-ADMISSION.json'))
