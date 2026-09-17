import json,os,subprocess,time
from pathlib import Path
E=Path(__file__).parent
unit='dbt-loader-native-build1-20260917.scope'
proc=Path('/proc/self');fields=(proc/'stat').read_text().rsplit(') ',1)[1].split();cg=Path('/sys/fs/cgroup')/(proc/'cgroup').read_text().strip().split('::',1)[1].lstrip('/')
assert cg.name==unit
properties=subprocess.run(['systemctl','--user','show',unit,'--property=Id,InvocationID,ControlGroup,MemoryMax,MemorySwapMax,CPUQuotaPerSecUSec,TasksMax,RuntimeMaxUSec'],capture_output=True,text=True,timeout=5)
def save(name,data):
 with (E/name).open('x') as f:json.dump(data,f,indent=2);f.write('\n')
raw={n:(cg/n).read_text() for n in ['memory.max','memory.swap.max','memory.events','memory.peak','cpu.max','pids.max','pids.events']}
save('before.json',dict(pid=os.getpid(),start=int(fields[19]),cgroup=str(cg),device=cg.stat().st_dev,inode=cg.stat().st_ino,properties=properties.stdout,properties_exit=properties.returncode,raw=raw))
assert raw['memory.max'].strip()=='8589934592' and raw['memory.swap.max'].strip()=='0' and raw['cpu.max'].strip()=='400000 100000' and raw['pids.max'].strip()=='1024'
command=json.loads((E/'command.json').read_text());started=time.monotonic()
with (E/'FULL.log').open('xb') as log:p=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT)
save('result.json',dict(argv=command,actual_exit=p.returncode,elapsed_seconds=time.monotonic()-started,raw_after={n:(cg/n).read_text() for n in raw},cgroup=str(cg)))
raise SystemExit(p.returncode)
