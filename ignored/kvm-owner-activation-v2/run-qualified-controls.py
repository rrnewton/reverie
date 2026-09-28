from pathlib import Path
import json,subprocess
root=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
folder=root/'ignored/kvm-owner-activation-v2'
args=['--plan','PLAN-1.json','--plan-sha256','3dae9a80a81abc6ddf618d8755a054e9a5ea8ba4281481ec8be98344c17599d2','--source','source-1.json','--source-sha256','2107719eed24132e34f19ebab8861ce3cb36483c7180dffb16a76a232d4d0d06']
def run(script,action):
 subprocess.run(['python3',str(folder/script),action,*args],cwd=root,check=True)
run('packet.py','retain')
run('runner.py','check-1')
run('runner.py','inventory-1')
plan=json.loads((folder/'PLAN-1.json').read_text())
new=['test-entry-gate-1','test-entry-owner-1','test-entry-routing-1','test-failure-completion-1','test-runtime-entry-1','test-vm-entry-1','test-exec-private-wait-1']
for phase in new+[item['phase'] for item in plan['tests'] if item['phase'] not in new]:
 run('runner.py',phase)
run('packet.py','report')
