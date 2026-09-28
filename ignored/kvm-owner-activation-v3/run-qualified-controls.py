from pathlib import Path
import json,subprocess
root=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
folder=root/'ignored/kvm-owner-activation-v3'
args=['--plan','PLAN-1.json','--plan-sha256','d8886f8202876a35a2320b19614af54b5cd554e08dd4435be6b87240b32fe266','--source','source-1.json','--source-sha256','047930881d84b518c15b4afed4e86e89a776abaf8dc944e3b3f22795de290362']
def run(script,action):
 subprocess.run(['python3',str(folder/script),action,*args],cwd=root,check=True)
run('runner.py','compile-1')
run('packet.py','retain')
run('runner.py','check-1')
run('runner.py','inventory-1')
plan=json.loads((folder/'PLAN-1.json').read_text())
new=['test-entry-capture-wake-1','test-entry-gate-1','test-entry-owner-1','test-entry-routing-1','test-failure-completion-1','test-runtime-entry-1','test-vm-entry-1','test-exec-private-wait-1']
for phase in new+[item['phase'] for item in plan['tests'] if item['phase'] not in new]:
 run('runner.py',phase)
run('packet.py','report')
