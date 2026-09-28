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
first=['test-public-entry-1','test-main-entry-1','test-spawn-entry-1']
for phase in first+[t['phase'] for t in plan['tests'] if t['phase'] not in first]:
 run('runner.py',phase)
run('packet.py','report')
