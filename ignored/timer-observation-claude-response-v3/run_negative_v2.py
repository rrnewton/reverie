import hashlib,json,subprocess,sys
from pathlib import Path
q=Path(__file__).resolve().parent/'negative-before-v2'
for name in ['compile','list-lib','list-static','test-terminal-race-negative','test-admission-negative']:
 prep=subprocess.run([sys.executable,'-B',str(q/'prepare.py'),name],capture_output=True);(q/(name+'-prepare.stdout')).write_bytes(prep.stdout);(q/(name+'-prepare.stderr')).write_bytes(prep.stderr)
 if prep.returncode:raise SystemExit('prepare failed '+name)
 plan=q/(name+'-plan.json');sha=hashlib.sha256(plan.read_bytes()).hexdigest()
 phase=subprocess.run([sys.executable,'-B',str(q/'phase.py'),'launch',str(plan),sha],capture_output=True);(q/(name+'-launch.stdout')).write_bytes(phase.stdout);(q/(name+'-launch.stderr')).write_bytes(phase.stderr)
 result=json.loads((q/'controls'/name/'result.json').read_text());print(json.dumps({'phase':name,'accepted':result['accepted'],'raw_status':result['raw_status'],'terminal_authenticated':result['terminal_authenticated'],'error':result.get('error')}),flush=True)
 if not result['terminal_authenticated'] or not result['inputs_unchanged'] or (name in ['compile','list-lib','list-static'] and not result['accepted']):raise SystemExit('refusal '+name)
