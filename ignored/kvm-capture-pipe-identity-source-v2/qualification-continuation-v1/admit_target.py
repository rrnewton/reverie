#!/usr/bin/python3
"""Admit a fresh continuation lease while the parent excludes the old unresolved lease."""
import os,stat
from pathlib import Path
from common import check_file,json_read,require,write_new
HERE=Path(__file__).resolve().parent
s=json_read(HERE/'SETUP.json');origin=Path(s['origin']);prior=json_read(origin/'TARGET-ADMISSION.json')
proof=json_read(origin/'failure-final-v1/TERMINAL-LIFECYCLE.json')
require(proof['properties']['ActiveState']=='inactive' and proof['properties']['MainPID']=='0' and proof['properties']['ControlGroup']=='' and proof['wrapper_session']['members']==[], 'old terminal lifecycle missing')
require(json_read(origin/'failure-final-v1/TERMINAL-READBACK.json')['old_lease_unresolved'] is True,'old refusal not preserved')
target=Path(s['target']);t=target.stat()
require(str(target)==prior['path'] and [t.st_dev,t.st_ino,t.st_uid]==prior['identity'] and stat.S_IMODE(t.st_mode)==0o700 and not target.is_symlink(),'private target identity changed')
lease=HERE/'lane.lease';dirs=[HERE/n for n in ['lease-state','tmp','cache','controls','launch']]
require(s['lease']==str(lease) and s['lease_state']==str(HERE/'lease-state'),'private lease destination changed')
for p in [*dirs,lease,HERE/'TARGET-ADMISSION.json']:require(not p.exists() and not p.is_symlink(),'continuation destination exists')
for p in dirs:p.mkdir(mode=0o700)
fd=os.open(lease,os.O_RDWR|os.O_CREAT|os.O_EXCL|os.O_CLOEXEC|os.O_NOFOLLOW,0o600)
try:
 st=os.fstat(fd);require(st.st_size==0 and stat.S_IMODE(st.st_mode)==0o600,'new lease is not empty/private')
 record=dict(path=str(lease),identity=[st.st_dev,st.st_ino,st.st_uid],mode=0o600,initially_empty=True)
finally:os.close(fd)
write_new(HERE/'TARGET-ADMISSION.json',dict(path=str(target),identity=[t.st_dev,t.st_ino,t.st_uid],mode=0o700,continued_target=True,initially_empty=False,private_lease=record,old_lease_unchanged=True,parent_holds_old_lease_exclusively=True))
