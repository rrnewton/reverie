from pathlib import Path
import json,subprocess,hashlib
out=Path(__file__).resolve().parent
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
args=['--plan','PLAN-1.json','--plan-sha256',sha(out/'PLAN-1.json'),'--source','source-1.json','--source-sha256',sha(out/'source-1.json')]
for script,action in [('runner.py','compile-1'),('packet.py','retain'),('runner.py','inventory-1')]:
 subprocess.run(['python3',str(out/script),action,*args],check=True)
r=subprocess.run(['python3',str(out/'runner.py'),'test-exec-tls-negative-1',*args])
print(json.dumps({'runner_status':r.returncode,'phase_result':json.loads((out/'test-exec-tls-negative-1/result.json').read_text())}))
raise SystemExit(r.returncode)
