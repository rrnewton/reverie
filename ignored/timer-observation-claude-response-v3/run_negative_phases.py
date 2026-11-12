import hashlib,json,subprocess,sys
from pathlib import Path
q=Path(__file__).resolve().parent/'negative-before-v1'
for name in ['list-lib','list-static','test-cleanup-negative','test-cleanup-ready','test-bookkeeping-negative','test-no-effect-negative','test-panic-negative','test-independent-delay']:
    prepare=subprocess.run([sys.executable,'-B',str(q/'prepare.py'),name],capture_output=True)
    (q/(name+'-prepare.stdout')).write_bytes(prepare.stdout);(q/(name+'-prepare.stderr')).write_bytes(prepare.stderr)
    if prepare.returncode: raise SystemExit('prepare failed '+name)
    plan=q/(name+'-plan.json');sha=hashlib.sha256(plan.read_bytes()).hexdigest()
    run=subprocess.run([sys.executable,'-B',str(q/'phase.py'),'launch',str(plan),sha],capture_output=True)
    (q/(name+'-launch.stdout')).write_bytes(run.stdout);(q/(name+'-launch.stderr')).write_bytes(run.stderr)
    result=json.loads((q/'controls'/name/'result.json').read_text())
    print(json.dumps({'phase':name,'caller_rc':run.returncode,'accepted':result['accepted'],'raw_status':result['raw_status'],'terminal_authenticated':result['terminal_authenticated'],'readback_error':result.get('readback_error'),'error':result.get('error')}),flush=True)
    if not result['terminal_authenticated'] or not result['inputs_unchanged']: raise SystemExit('lifecycle/source refusal '+name)
    if name in ['list-lib','list-static','test-cleanup-ready','test-independent-delay'] and not result['accepted']: raise SystemExit('positive control failure '+name)
