import json,os,subprocess,time,resource
from pathlib import Path
E=Path(__file__).parent;resource.setrlimit(resource.RLIMIT_CORE,(0,0))
unit='sabre-frame-abi-commit2-20260917.scope'
proc=Path('/proc/self');cg=Path('/sys/fs/cgroup')/(proc/'cgroup').read_text().strip().split('::',1)[1].lstrip('/')
assert cg.name==unit
names=['memory.max','memory.swap.max','memory.events','memory.peak','cpu.max','pids.max','pids.events']
def raw():return {n:(cg/n).read_text() for n in names}
def save(name,obj):
 with (E/name).open('x') as f:json.dump(obj,f,indent=2);f.write('\n')
x=raw();assert x['memory.max'].strip()=='8589934592' and x['memory.swap.max'].strip()=='0' and x['cpu.max'].strip()=='400000 100000' and x['pids.max'].strip()=='1024'
save('before.json',dict(pid=os.getpid(),start=int((proc/'stat').read_text().rsplit(') ',1)[1].split()[19]),cgroup=str(cg),inode=cg.stat().st_ino,raw=x))
argv=['python3',str(E/'commit.py')];started=time.monotonic()
r=subprocess.run(argv)
save('RESULT.json',dict(argv=argv,actual_exit=r.returncode,elapsed_seconds=time.monotonic()-started,raw=raw()))
raise SystemExit(r.returncode)
