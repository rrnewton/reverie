from pathlib import Path
import json,subprocess,hashlib
out=Path(__file__).resolve().parent
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
args=['--plan','PLAN-1.json','--plan-sha256',sha(out/'PLAN-1.json'),'--source','source-1.json','--source-sha256',sha(out/'source-1.json')]
def run(script,action):
 subprocess.run(['python3',str(out/script),action,*args],check=True)
run('runner.py','compile-1')
run('packet.py','retain')
run('runner.py','check-1')
run('runner.py','inventory-1')
plan=json.loads((out/'PLAN-1.json').read_text())
first=plan['new_phases_first']
failures=[]
for phase in first+[t['phase'] for t in plan['tests'] if t['phase'] not in first]:
 try:run('runner.py',phase)
 except subprocess.CalledProcessError:failures.append(phase)
print(json.dumps({'failed_phases':failures}))
run('packet.py','report')
