from pathlib import Path
import subprocess,sys,json,time
e=Path(__file__).resolve().parent
t=time.monotonic()
for script,args in [('seed-cache.py',[]),('native-run.py',['old-1'])]:
 p=subprocess.run([sys.executable,str(e/script),*args]);print(json.dumps({'script':script,'actual_exit':p.returncode,'elapsed_seconds':time.monotonic()-t}),flush=True)
 if p.returncode:sys.exit(p.returncode)
